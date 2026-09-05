use anyhow::Result;
use std::{
    cell::{OnceCell, RefCell},
    num::NonZeroUsize,
    rc::Rc,
};

use crate::{
    data::{
        FileProvider, IconIndex,
        listing::{Listed, Listing},
        web::WebFileProvider,
    },
    excel::base::CachedProvider,
    report::{Recording, Reporter},
    schema::{boxed::BoxedSchemaProvider, web::WebProvider},
    settings::{BackendConfig, InstallLocation, SchemaLocation},
    utils::TrackedPromise,
};

#[derive(Clone)]
pub struct Backend(Rc<BackendImpl>);

struct BackendImpl {
    files: Rc<dyn FileProvider>,
    excel_provider: CachedProvider,
    schema_provider: BoxedSchemaProvider,
    icons: OnceCell<IconIndex>,
    listing: RefCell<Fetch>,
    reporter: Rc<Reporter>,
}

/// The shared listing on its way in. Decoding costs a frame and cannot cross a thread, so the
/// promise carries the bytes and the frame they land on turns them into a [`Listing`].
enum Fetch {
    Idle,
    Fetching(TrackedPromise<Result<(Vec<u8>, Vec<u8>)>>),
    Ready(Rc<Listing>),
    Failed(Rc<str>),
}

impl Backend {
    pub async fn new(config: BackendConfig) -> Result<Self> {
        let reporter = Rc::new(Reporter::new(&config.api_url));
        let excel = async {
            let (files, cache_size): (Rc<dyn FileProvider>, usize) = match config.location {
                #[cfg(not(target_arch = "wasm32"))]
                InstallLocation::Sqpack(path) => {
                    let files: Rc<dyn FileProvider> =
                        Rc::new(crate::data::sqpack::SqpackFileProvider::new(&path));
                    (files, 64)
                }
                #[cfg(target_arch = "wasm32")]
                InstallLocation::Worker(path) => {
                    use crate::data::worker::WorkerFileProvider;
                    let handle = WorkerFileProvider::folders()
                        .await?
                        .into_iter()
                        .find(|f| f.0.name() == path)
                        .ok_or_else(|| anyhow::anyhow!("WorkerFileProvider：未找到条目"))?;
                    WorkerFileProvider::verify_folder(handle.clone()).await?;
                    let files: Rc<dyn FileProvider> =
                        Rc::new(WorkerFileProvider::new(handle).await?);
                    (files, 64)
                }

                InstallLocation::Web(region, version) => {
                    let files: Rc<dyn FileProvider> = Rc::new(
                        WebFileProvider::new(&config.api_url, region.api_name(), version).await?,
                    );
                    (files, 256)
                }
            };
            let files: Rc<dyn FileProvider> = Rc::new(Recording::new(files, reporter.clone()));
            let excel_provider =
                CachedProvider::new(files.clone(), NonZeroUsize::new(cache_size).unwrap()).await?;
            anyhow::Result::<_>::Ok((files, excel_provider))
        };
        let schema = async {
            anyhow::Result::<_>::Ok(match config.schema {
                #[cfg(not(target_arch = "wasm32"))]
                SchemaLocation::Local(path) => {
                    BoxedSchemaProvider::new_local(crate::schema::local::LocalProvider::new(&path))
                }
                #[cfg(target_arch = "wasm32")]
                SchemaLocation::Worker(path) => {
                    use crate::schema::worker::WorkerProvider;
                    let handle = WorkerProvider::folders()
                        .await?
                        .into_iter()
                        .find(|f| f.0.name() == path)
                        .ok_or_else(|| anyhow::anyhow!("WorkerProvider：未找到条目"))?;
                    WorkerProvider::verify_folder(handle.clone()).await?;
                    BoxedSchemaProvider::new_worker(WorkerProvider::new(handle).await?)
                }

                SchemaLocation::Github(location) => {
                    BoxedSchemaProvider::new_web(WebProvider::new_github(&location)?)
                }

                SchemaLocation::Web(base_url) => {
                    BoxedSchemaProvider::new_web(WebProvider::new(base_url))
                }
            })
        };
        let ((files, excel_provider), schema) = futures_util::try_join!(excel, schema)?;
        Ok(Self(Rc::new(BackendImpl {
            files,
            excel_provider,
            schema_provider: schema,
            icons: OnceCell::new(),
            listing: RefCell::new(Fetch::Idle),
            reporter,
        })))
    }

    /// The shared raw-file provider. Read any game file with
    /// [`FileProviderExt::file`](crate::data::FileProviderExt::file), e.g.
    /// `backend.files().file::<Vec<u8>>(path)`.
    pub fn files(&self) -> &Rc<dyn FileProvider> {
        &self.0.files
    }

    pub fn excel(&self) -> &CachedProvider {
        &self.0.excel_provider
    }

    pub fn schema(&self) -> &BoxedSchemaProvider {
        &self.0.schema_provider
    }

    /// Which `ui/icon` files this install ships, once the asset browser has decoded the path list.
    pub fn icons(&self) -> Option<&IconIndex> {
        self.0.icons.get()
    }

    pub fn set_icons(&self, icons: IconIndex) {
        let _ = self.0.icons.set(icons);
    }

    /// What this install holds, by directory, asking for it the first time anything wants it. The
    /// asset tree, the icon subset and a model's animation packs all read the same one, so whoever
    /// gets there first pays for the fetch and the rest are handed it.
    pub fn listing(&self, api: &str) -> Listed {
        let mut held = self.0.listing.borrow_mut();
        if let Fetch::Idle = &*held {
            let files = self.0.files.clone();
            let api = api.to_owned();
            *held = Fetch::Fetching(TrackedPromise::spawn_local(async move {
                files.path_index(&api).await
            }));
        }
        if let Fetch::Fetching(promise) = &*held
            && let Some(landed) = promise.try_get()
        {
            *held =
                match landed
                    .as_ref()
                    .map_err(ToString::to_string)
                    .and_then(|(paths, presence)| {
                        Listing::decode(paths, presence).map_err(|why| why.to_string())
                    }) {
                    Ok(listing) => Fetch::Ready(Rc::new(listing)),
                    Err(why) => Fetch::Failed(why.into()),
                };
        }
        match &*held {
            Fetch::Idle | Fetch::Fetching(_) => Listed::Loading,
            Fetch::Ready(listing) => Listed::Ready(listing.clone()),
            Fetch::Failed(why) => Listed::Failed(why.clone()),
        }
    }

    /// Paths this install carries that the community list does not name, waiting on the user.
    pub fn reporter(&self) -> &Rc<crate::report::Reporter> {
        &self.0.reporter
    }
}

#[cfg(target_arch = "wasm32")]
pub mod worker {
    use std::{
        cell::{LazyCell, RefCell},
        sync::atomic::{AtomicBool, Ordering},
    };

    use gloo_worker::{Spawnable, WorkerBridge};
    use pinned::oneshot;

    use crate::worker::{PreservingCodec, SqpackWorker, WorkerRequest, WorkerResponse};

    static WORKER_FLAG: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static WORKER: LazyCell<WorkerBridge<SqpackWorker>> = LazyCell::new(|| {
            assert!(!WORKER_FLAG.swap(true, Ordering::SeqCst), "Worker already initialized");
            SqpackWorker::spawner()
                .encoding::<PreservingCodec>()
                .with_loader(true)
                .as_module(false)
                .spawn("./worker_loader.js")
        });
    }

    pub async fn transact(input: WorkerRequest) -> WorkerResponse {
        let (tx, rx) = oneshot::channel();
        let tx = RefCell::new(Some(tx));
        let bridge = WORKER.with(|w| {
            w.fork(Some(move |msg| {
                let ret = tx.take().map(|tx| tx.send(msg));
                match ret {
                    Some(Ok(())) => {}
                    Some(Err(_)) => {
                        log::error!("worker: 发送消息失败");
                    }
                    None => {
                        log::error!("worker: tx 已被占用");
                    }
                }
            }))
        });
        bridge.send(input);
        rx.await.unwrap()
    }
}
