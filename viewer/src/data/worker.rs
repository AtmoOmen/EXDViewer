use crate::{
    backend::worker,
    worker::{WorkerDirectory, WorkerRequest, WorkerResponse},
};

use super::{DecodedTexture, FileProvider, list_url, with_list_id};
use async_trait::async_trait;
use either::Either;
use image::RgbaImage;
use url::Url;

pub struct WorkerFileProvider(());

impl WorkerFileProvider {
    pub async fn new(handle: WorkerDirectory) -> anyhow::Result<Self> {
        match worker::transact(WorkerRequest::DataSetup(handle)).await {
            WorkerResponse::DataSetup(Ok(())) => Ok(Self(())),
            WorkerResponse::DataSetup(Err(e)) => {
                Err(anyhow::anyhow!("WorkerFileProvider：设置文件夹失败：{e}"))
            }
            _ => Err(anyhow::anyhow!("WorkerFileProvider：响应无效")),
        }
    }

    pub async fn folders() -> anyhow::Result<Vec<WorkerDirectory>> {
        match worker::transact(WorkerRequest::DataGet()).await {
            WorkerResponse::DataGet(Ok(folders)) => Ok(folders),
            WorkerResponse::DataGet(Err(e)) => Err(anyhow::anyhow!(
                "WorkerFileProvider：获取文件夹列表失败：{e}"
            )),
            _ => Err(anyhow::anyhow!("WorkerFileProvider：响应无效")),
        }
    }

    pub async fn add_folder(handle: WorkerDirectory) -> anyhow::Result<()> {
        match worker::transact(WorkerRequest::DataStore(handle)).await {
            WorkerResponse::DataStore(Ok(())) => Ok(()),
            WorkerResponse::DataStore(Err(e)) => {
                Err(anyhow::anyhow!("WorkerFileProvider：添加文件夹失败：{e}"))
            }
            _ => Err(anyhow::anyhow!("WorkerFileProvider：响应无效")),
        }
    }

    pub async fn verify_folder(handle: WorkerDirectory) -> anyhow::Result<()> {
        match worker::transact(WorkerRequest::VerifyFolder((handle, false))).await {
            WorkerResponse::VerifyFolder(Ok(())) => Ok(()),
            WorkerResponse::VerifyFolder(Err(e)) => {
                Err(anyhow::anyhow!("WorkerFileProvider：验证文件夹失败：{e}"))
            }
            _ => Err(anyhow::anyhow!("WorkerFileProvider：响应无效")),
        }
    }
}

#[async_trait(?Send)]
impl FileProvider for WorkerFileProvider {
    async fn read_stream(&self, path: &str) -> anyhow::Result<(Option<String>, Vec<u8>)> {
        log::info!("WorkerFileProvider：正在请求文件 {path:?}");
        if let WorkerResponse::DataRequestFile(result) =
            worker::transact(WorkerRequest::DataRequestFile(path.to_string())).await
        {
            let file =
                result.map_err(|e| ironworks::Error::NotFound(ironworks::ErrorValue::Other(e)))?;
            Ok((Some(file.kind), file.bytes))
        } else {
            Err(anyhow::anyhow!(
                "WorkerFileProvider：工作线程返回了无效响应"
            ))
        }
    }

    /// The list goes to the worker through the port rather than being fetched there as well.
    async fn path_index(&self, api_base: &str) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
        log::info!("WorkerFileProvider：正在构建文件存在映射");
        let paths = with_list_id(api_base, |id| {
            crate::utils::fetch_url(list_url(api_base, id))
        })
        .await?;
        if let WorkerResponse::DataPresence(result) =
            worker::transact(WorkerRequest::DataPresence(paths.clone())).await
        {
            let presence = result.map_err(|e| anyhow::anyhow!("WorkerFileProvider: {e}"))?;
            Ok((paths, presence.0))
        } else {
            Err(anyhow::anyhow!(
                "WorkerFileProvider：工作线程返回了无效响应"
            ))
        }
    }

    async fn read_stream_by_hash(
        &self,
        repository: u8,
        category: u8,
        hash: u64,
        split: bool,
    ) -> anyhow::Result<(Option<String>, Vec<u8>)> {
        log::info!("WorkerFileProvider：正在请求文件 {repository}/{category}/{hash:X}");
        if let WorkerResponse::DataRequestFileByHash(result) = worker::transact(
            WorkerRequest::DataRequestFileByHash((repository, category, hash, split)),
        )
        .await
        {
            let file =
                result.map_err(|e| ironworks::Error::NotFound(ironworks::ErrorValue::Other(e)))?;
            Ok((Some(file.kind), file.bytes))
        } else {
            Err(anyhow::anyhow!(
                "WorkerFileProvider：工作线程返回了无效响应"
            ))
        }
    }

    /// Read and decode in the one round trip, rather than fetching the bytes here only to send them
    /// straight back for decoding.
    async fn read_texture(
        &self,
        path: &str,
        max_dim: Option<u16>,
    ) -> anyhow::Result<DecodedTexture> {
        log::info!("WorkerFileProvider：正在请求纹理 {path:?}");
        if let WorkerResponse::DataRequestTexture(result) = worker::transact(
            WorkerRequest::DataRequestTexture((path.to_owned(), max_dim)),
        )
        .await
        {
            DecodedTexture::from_worker(
                result.map_err(|e| anyhow::anyhow!("WorkerFileProvider：获取纹理失败：{e}"))?,
            )
        } else {
            Err(anyhow::anyhow!(
                "WorkerFileProvider：工作线程返回了无效响应"
            ))
        }
    }

async fn get_icon(&self, path: &str) -> anyhow::Result<Either<Url, RgbaImage>> {
        log::info!("WorkerFileProvider：正在请求图标 {path}");
        Ok(Either::Right(self.read_texture(path, None).await?.image))
    }

    async fn exists_many(&self, paths: &[String]) -> anyhow::Result<Vec<bool>> {
        log::info!("WorkerFileProvider：正在检查文件是否存在 {paths:?}");
        if let WorkerResponse::DataRequestExists(result) =
            worker::transact(WorkerRequest::DataRequestExists(paths.to_vec())).await
        {
            result.map_err(|e| anyhow::anyhow!("WorkerFileProvider：检查文件是否存在时失败：{e}"))
        } else {
            Err(anyhow::anyhow!(
                "WorkerFileProvider：工作线程返回了无效响应"
            ))
        }
    }
}
