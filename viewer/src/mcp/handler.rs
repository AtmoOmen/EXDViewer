use std::sync::{Arc, mpsc, Mutex};

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{
        router::tool::ToolRouter,
        wrapper::Parameters,
    },
    model::*,
    tool, tool_handler, tool_router,
};

use crate::settings::BackendConfig;

use super::{McpRequest, McpResponse};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListSheetsParams {
    pub query: Option<String>,
    pub include_misc: Option<bool>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SheetNameParam {
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ValidateFilterParams {
    pub expression: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SaveSchemaParams {
    pub name: String,
    pub text: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetReferencingSheetsParams {
    pub target_sheet: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRowParams {
    pub name: String,
    pub row_id: Option<u32>,
    pub subrow_id: Option<u16>,
    pub search_name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    pub name: String,
    pub query: Option<String>,
    pub filter: Option<String>,
    pub max_results: Option<usize>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FollowLinkParams {
    pub name: String,
    pub row_id: u32,
    pub column_index: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DecodeSeStringParams {
    pub name: String,
    pub row_id: u32,
    pub subrow_id: Option<u16>,
    pub column_index: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetIconUrlParams {
    pub icon_id: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DecomposeModelIdParams {
    pub model_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchSheetsParams {
    pub query: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetSheetInfoParams {
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ResolveDisplayFieldParams {
    pub name: String,
    pub row_id: u32,
    pub subrow_id: Option<u16>,
}

type McpChannel = mpsc::SyncSender<(McpRequest, mpsc::SyncSender<McpResponse>)>;

#[derive(Clone)]
pub struct McpHandler {
    request_tx: McpChannel,
    config: Arc<Mutex<BackendConfig>>,
    tool_router: ToolRouter<Self>,
}

impl McpHandler {
    pub fn new(request_tx: McpChannel, config: BackendConfig) -> Self {
        Self {
            request_tx,
            config: Arc::new(Mutex::new(config)),
            tool_router: Self::tool_router(),
        }
    }

    async fn delegate(&self, request: McpRequest) -> Result<String, McpError> {
        let request_name = format!("{request:?}");
        log::info!("MCP 委托请求: {request_name}");

        let tx = self.request_tx.clone();
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), tokio::task::spawn_blocking(move || {
            let (resp_tx, resp_rx) = mpsc::sync_channel(1);
            let _ = tx.send((request, resp_tx));
            resp_rx.recv()
        }))
        .await
        .map_err(|_| McpError::internal_error("MCP 请求超时", None))?
        .map_err(|e| McpError::internal_error(format!("{e}"), None))?
        .map_err(|e| McpError::internal_error(format!("channel recv: {e}"), None))?;

        log::info!("MCP 收到响应: {request_name}");
        match response {
            McpResponse::Success(s) => Ok(s),
            McpResponse::Error(e) => Err(McpError::internal_error(e, None)),
        }
    }
}

#[tool_router]
impl McpHandler {
    #[tool(description = "列出所有可用的游戏数据表，支持模糊搜索、分页和杂项表开关")]
    async fn list_sheets(
        &self,
        Parameters(params): Parameters<ListSheetsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::ListSheets {
                query: params.query,
                include_misc: params.include_misc.unwrap_or(false),
                offset: params.offset.unwrap_or(0),
                limit: params.limit.unwrap_or(100),
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取指定表的元信息：列数、子行、语言列表")]
    async fn get_sheet_info(
        &self,
        Parameters(params): Parameters<GetSheetInfoParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::GetSheetInfo {
                name: params.name,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取指定表的结构化模式定义（列名、类型、描述、关系映射）")]
    async fn get_sheet_schema(
        &self,
        Parameters(params): Parameters<SheetNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::GetSheetSchema {
                name: params.name,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取指定表的模式 YAML 原文")]
    async fn get_schema_raw(
        &self,
        Parameters(params): Parameters<SheetNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::GetSchemaRaw {
                name: params.name,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取当前加载的游戏数据源和模式数据源信息")]
    async fn get_game_version(&self) -> Result<CallToolResult, McpError> {
        let config = self.config.lock().map_err(|e| McpError::internal_error(format!("{e}"), None))?;

        let version_info = match &config.location {
            crate::settings::InstallLocation::Web(_, version) => serde_json::json!({
                "source": "web",
                "version": version.as_ref().map(|v| v.to_string())
            }),
            #[cfg(not(target_arch = "wasm32"))]
            crate::settings::InstallLocation::Sqpack(path) => serde_json::json!({
                "source": "sqpack",
                "path": path
            }),
            #[cfg(target_arch = "wasm32")]
            crate::settings::InstallLocation::Worker(_) => serde_json::json!({
                "source": "worker"
            }),
        };

        let schema_info = match &config.schema {
            crate::settings::SchemaLocation::Github(gh) => serde_json::json!({
                "source": "github",
                "owner": gh.owner,
                "repo": gh.repo,
                "branch": gh.branch.to_string()
            }),
            crate::settings::SchemaLocation::Web(url) => serde_json::json!({
                "source": "web",
                "url": url
            }),
            #[cfg(not(target_arch = "wasm32"))]
            crate::settings::SchemaLocation::Local(path) => serde_json::json!({
                "source": "local",
                "path": path
            }),
            #[cfg(target_arch = "wasm32")]
            crate::settings::SchemaLocation::Worker(_) => serde_json::json!({
                "source": "worker"
            }),
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::json!({
                "data_source": version_info,
                "schema_source": schema_info
            })
            .to_string(),
        )]))
    }

    #[tool(description = "校验复杂筛选 DSL 表达式语法")]
    async fn validate_filter(
        &self,
        Parameters(params): Parameters<ValidateFilterParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::ValidateFilter {
                expression: params.expression,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "校验模式 YAML 文本是否符合 EXDSchema JSON Schema 规范")]
    async fn validate_schema(
        &self,
        Parameters(params): Parameters<SaveSchemaParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::ValidateSchema {
                text: params.text,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "按图标 ID 获取图标纹理路径")]
    async fn get_icon_url(
        &self,
        Parameters(params): Parameters<GetIconUrlParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::GetIconUrl {
                icon_id: params.icon_id,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "分解 ModelId 字段为模型ID、变体ID、染色ID 组件")]
    async fn decompose_model_id(
        &self,
        Parameters(params): Parameters<DecomposeModelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::DecomposeModelId {
                model_id: params.model_id,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "按名称模糊搜索表名")]
    async fn search_sheets(
        &self,
        Parameters(params): Parameters<SearchSheetsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::SearchSheets {
                query: params.query,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "在指定表的所有行中搜索包含指定文本的单元格")]
    async fn search_cells(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::SearchCells {
                name: params.name,
                query: params.query.unwrap_or_default(),
                max_results: params.max_results.unwrap_or(50),
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "按条件分页查询表中行数据，支持复杂筛选 DSL 表达式")]
    async fn query_rows(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::QueryRows {
                name: params.name,
                filter: params.filter,
                offset: params.offset.unwrap_or(0),
                limit: params.limit.unwrap_or(50),
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "按表名和行 ID/子行 ID 精确获取单行，支持按显示字段值搜索")]
    async fn get_row(
        &self,
        Parameters(params): Parameters<GetRowParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::GetRow {
                name: params.name,
                row_id: params.row_id.unwrap_or(0),
                subrow_id: params.subrow_id.unwrap_or(0),
                search_name: params.search_name,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取指定表的所有关系映射")]
    async fn get_sheet_relations(
        &self,
        Parameters(params): Parameters<SheetNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::GetSheetRelations {
                name: params.name,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "查询哪些表的模式中声明了指向目标表的关系")]
    async fn get_referencing_sheets(
        &self,
        Parameters(params): Parameters<GetReferencingSheetsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::GetReferencingSheets {
                target_sheet: params.target_sheet,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "沿链接字段解析目标行数据")]
    async fn follow_link(
        &self,
        Parameters(params): Parameters<FollowLinkParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::FollowLink {
                name: params.name,
                row_id: params.row_id,
                column_index: params.column_index.unwrap_or(0),
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "解码指定单元格的 SeString 文本")]
    async fn decode_se_string(
        &self,
        Parameters(params): Parameters<DecodeSeStringParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::DecodeSeString {
                name: params.name,
                row_id: params.row_id,
                subrow_id: params.subrow_id.unwrap_or(0),
                column_index: params.column_index.unwrap_or(0),
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "保存模式的 YAML 文本")]
    async fn save_schema(
        &self,
        Parameters(params): Parameters<SaveSchemaParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::SaveSchema {
                name: params.name,
                text: params.text,
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "解析指定行在 GUI 中的主显示文本/值（根据 schema 的 display_field 定义）")]
    async fn resolve_display_field(
        &self,
        Parameters(params): Parameters<ResolveDisplayFieldParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .delegate(McpRequest::ResolveDisplayField {
                name: params.name,
                row_id: params.row_id,
                subrow_id: params.subrow_id.unwrap_or(0),
            })
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

#[tool_handler(instructions = "EXDViewer MCP server，提供 FFXIV 游戏数据表查询、模式管理、搜索等功能")]
impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::from_build_env())
            .with_instructions(
                "EXDViewer MCP 服务器，提供 18 个工具用于查询 FFXIV 游戏数据表。\
                 涵盖：表列表/元信息/模式定义/行查询/跨表搜索/关系查询/SeString 解码/图标/ModelId 分解/模式编辑/筛选校验。"
                    .to_string(),
            )
    }
}
