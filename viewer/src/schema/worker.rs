use async_trait::async_trait;

use crate::{
    backend::worker,
    worker::{WorkerDirectory, WorkerRequest, WorkerResponse},
};

use super::provider::SchemaProvider;

pub struct WorkerProvider(());

impl WorkerProvider {
    pub async fn new(handle: WorkerDirectory) -> anyhow::Result<Self> {
        match worker::transact(WorkerRequest::SchemaSetup(handle)).await {
            WorkerResponse::SchemaSetup(Ok(())) => Ok(Self(())),
            WorkerResponse::SchemaSetup(Err(e)) => Err(anyhow::anyhow!(
                "WorkerProvider：设置模式文件夹失败: {e}"
            )),
            _ => Err(anyhow::anyhow!("WorkerProvider：无效的模式响应")),
        }
    }

    pub async fn folders() -> anyhow::Result<Vec<WorkerDirectory>> {
        match worker::transact(WorkerRequest::SchemaGet()).await {
            WorkerResponse::SchemaGet(Ok(folders)) => Ok(folders),
            WorkerResponse::SchemaGet(Err(e)) => Err(anyhow::anyhow!(
                "WorkerProvider：读取模式文件夹失败: {e}"
            )),
            _ => Err(anyhow::anyhow!("WorkerProvider：无效的模式响应")),
        }
    }

    pub async fn add_folder(handle: WorkerDirectory) -> anyhow::Result<()> {
        match worker::transact(WorkerRequest::SchemaStore(handle)).await {
            WorkerResponse::SchemaStore(Ok(())) => Ok(()),
            WorkerResponse::SchemaStore(Err(e)) => Err(anyhow::anyhow!(
                "WorkerProvider：添加模式文件夹失败: {e}"
            )),
            _ => Err(anyhow::anyhow!("WorkerProvider：无效的模式响应")),
        }
    }

    pub async fn verify_folder(handle: WorkerDirectory) -> anyhow::Result<()> {
        match worker::transact(WorkerRequest::VerifyFolder((handle, true))).await {
            WorkerResponse::VerifyFolder(Ok(())) => Ok(()),
            WorkerResponse::VerifyFolder(Err(e)) => Err(anyhow::anyhow!(
                "WorkerProvider：校验模式文件夹失败: {e}"
            )),
            _ => Err(anyhow::anyhow!("WorkerProvider：无效的模式响应")),
        }
    }
}

#[async_trait(?Send)]
impl SchemaProvider for WorkerProvider {
    async fn get_schema_text(&self, name: &str) -> anyhow::Result<String> {
        log::info!("WorkerProvider：正在请求模式 {name:?}");
        if let WorkerResponse::SchemaRequestGet(result) =
            worker::transact(WorkerRequest::SchemaRequestGet(format!("{name}.yml"))).await
        {
            result.map_err(|e| anyhow::anyhow!("WorkerProvider：读取模式失败: {e}"))
        } else {
            return Err(anyhow::anyhow!("WorkerProvider：无效的模式响应"));
        }
    }

    fn can_save_schemas(&self) -> bool {
        true
    }

    fn save_schema_start_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    async fn save_schema(&self, name: &str, text: &str) -> anyhow::Result<()> {
        log::info!("WorkerProvider：正在保存模式 {name:?}");
        if let WorkerResponse::SchemaRequestStore(result) = worker::transact(
            WorkerRequest::SchemaRequestStore((format!("{name}.yml"), text.to_string())),
        )
        .await
        {
            result.map_err(|e| anyhow::anyhow!("WorkerProvider：保存模式失败: {e}"))
        } else {
            return Err(anyhow::anyhow!("WorkerProvider：无效的模式响应"));
        }
    }
}
