use crate::utils::tex_loader;

use super::{
    DecodedTexture, FileProvider, build_local_presence, index_hash, list_url, stream, with_list_id,
};
use crate::utils::fetch_url;
use async_trait::async_trait;
use blocking::unblock;
use either::Either;
use image::RgbaImage;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};
use std::{path::PathBuf, str::FromStr, sync::Arc};
use url::Url;

type Pack = Ironworks<Arc<SqPack<Install>>>;

/// Reads an install on a pool rather than on the thread that draws. Decompressing a package's
/// blocks and decoding what comes out of them is the same work whoever does it, but a zone's worth
/// of it runs for seconds, and every one of those was a frame that did not paint.
pub struct SqpackFileProvider {
    ironworks: Arc<Pack>,
    /// The same resource the `Ironworks` holds. Hash lookups are a SqPack concept, so they need the
    /// concrete type; sharing it keeps one index cache rather than two.
    sqpack: Arc<SqPack<Install>>,
}

impl SqpackFileProvider {
    pub fn new(install_location: &str) -> Self {
        let resource = Install::at_sqpack(PathBuf::from_str(install_location).unwrap());
        let sqpack = Arc::new(SqPack::new(resource));
        Self {
            ironworks: Arc::new(Ironworks::new().with_resource(sqpack.clone())),
            sqpack,
        }
    }
}

#[async_trait(?Send)]
impl FileProvider for SqpackFileProvider {
    async fn read_stream(&self, path: &str) -> anyhow::Result<(Option<String>, Vec<u8>)> {
        let sqpack = self.sqpack.clone();
        let path = path.to_owned();
        unblock(move || {
            let (kind, bytes) = stream(sqpack.file(&path)?)?;
            anyhow::Ok((Some(kind), bytes))
        })
        .await
    }

    async fn read_stream_by_hash(
        &self,
        repository: u8,
        category: u8,
        hash: u64,
        split: bool,
    ) -> anyhow::Result<(Option<String>, Vec<u8>)> {
        let sqpack = self.sqpack.clone();
        unblock(move || {
            let (kind, bytes) = stream(sqpack.file_by_hash(
                repository,
                category,
                index_hash(hash, split),
            )?)?;
            anyhow::Ok((Some(kind), bytes))
        })
        .await
    }

    async fn path_index(&self, api_base: &str) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
        let paths = with_list_id(api_base, |id| fetch_url(list_url(api_base, id))).await?;
        let sqpack = self.sqpack.clone();
        unblock(move || {
            let presence = build_local_presence(&sqpack, &paths)?;
            anyhow::Ok((paths, presence))
        })
        .await
    }

    /// Read and decode in the one hop, rather than crossing to the pool twice for the one texture.
    async fn read_texture(
        &self,
        path: &str,
        max_dim: Option<u16>,
    ) -> anyhow::Result<DecodedTexture> {
        let sqpack = self.sqpack.clone();
        let path = path.to_owned();
        unblock(move || {
            let (_, bytes) = stream(sqpack.file(&path)?)?;
            let (image, source) = tex_loader::decode_preview_sized(&bytes, &path, max_dim)?;
            anyhow::Ok(DecodedTexture {
                image: image.to_rgba8(),
                source,
            })
        })
        .await
    }

    async fn get_icon(&self, path: &str) -> anyhow::Result<Either<Url, RgbaImage>> {
        let ironworks = self.ironworks.clone();
        let path = path.to_owned();
        unblock(move || {
            let data = tex_loader::read(ironworks.as_ref(), &path)?;
            anyhow::Ok(Either::Right(data.into_rgba8()))
        })
        .await
    }

    async fn exists_many(&self, paths: &[String]) -> anyhow::Result<Vec<bool>> {
        let ironworks = self.ironworks.clone();
        let paths = paths.to_vec();
        unblock(move || {
            paths
                .iter()
                .map(|path| anyhow::Ok(ironworks.exists(path)?))
                .collect()
        })
        .await
    }
}
