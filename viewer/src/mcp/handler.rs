use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use ironworks::excel::Language;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};
use tokio::sync::oneshot;

use crate::settings::BackendConfig;

use super::{McpChannel, McpRequest, McpResponse};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSheetsParams {
    pub query: Option<String>,
    pub include_misc: Option<bool>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SheetNameParam {
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateFilterParams {
    pub expression: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveSchemaParams {
    pub name: String,
    pub text: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetReferencingSheetsParams {
    pub target_sheet: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetRowParams {
    /// 精确表名, 不确定时先调用 search_sheets
    pub name: String,
    /// 已知行 ID 时填写, 模糊查找请先用 query_rows 或 search_cells
    pub row_id: Option<u32>,
    /// 子行 ID, 仅子行表需要
    pub subrow_id: Option<u16>,
    /// 只按 display_field 做简单包含搜索, 优先用 query_rows 或 search_cells
    pub search_name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchCellsParams {
    /// 精确表名, 不确定时先调用 search_sheets
    pub name: String,
    /// 必填关键词, 这是普通文本搜索, 不是筛选 DSL
    pub query: String,
    /// 最大返回单元格命中数, 默认 50
    pub max_results: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryRowsParams {
    /// 精确表名, 不确定时先调用 search_sheets
    pub name: String,
    /// 复杂筛选 DSL, 例如 `# = 42`, `Name *= "Potion"`, `Level >= 50 AND Name not *= Test`
    pub filter: Option<String>,
    /// 匹配结果偏移量, 默认 0
    pub offset: Option<usize>,
    /// 返回匹配行数, 默认 50, 服务端会限制最大值
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FollowLinkParams {
    pub name: String,
    pub row_id: u32,
    pub column_index: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecodeSeStringParams {
    pub name: String,
    pub row_id: u32,
    pub subrow_id: Option<u16>,
    pub column_index: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetIconUrlParams {
    pub icon_id: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecomposeModelIdParams {
    pub model_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchSheetsParams {
    pub query: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetSheetInfoParams {
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveDisplayFieldParams {
    pub name: String,
    pub row_id: u32,
    pub subrow_id: Option<u16>,
}

#[derive(Clone)]
pub struct McpHandler {
    request_tx: McpChannel,
    config: Arc<Mutex<BackendConfig>>,
    language: Language,
    tool_router: ToolRouter<Self>,
}

impl McpHandler {
    pub fn new(request_tx: McpChannel, config: BackendConfig, language: Language) -> Self {
        Self {
            request_tx,
            config: Arc::new(Mutex::new(config)),
            language,
            tool_router: Self::tool_router(),
        }
    }

    fn light_timeout() -> Duration {
        Duration::from_secs(12)
    }

    fn medium_timeout() -> Duration {
        Duration::from_secs(20)
    }

    fn heavy_timeout() -> Duration {
        Duration::from_secs(45)
    }

    async fn call(&self, request: McpRequest, timeout: Duration) -> Result<String, McpError> {
        let request_name = format!("{request:?}");
        log::info!("MCP 请求: {request_name}");

        let tx = self.request_tx.clone();
        let response = tokio::time::timeout(timeout, async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            tx.send((request, resp_tx))
                .map_err(|e| McpError::internal_error(format!("MCP 请求发送失败: {e}"), None))?;
            resp_rx
                .await
                .map_err(|e| McpError::internal_error(format!("MCP 响应接收失败: {e}"), None))
        })
        .await
        .map_err(|_| McpError::internal_error("MCP 请求超时", None))??;

        log::info!("MCP 响应完成: {request_name}");
        match response {
            McpResponse::Success(text) => Ok(text),
            McpResponse::Error(error) => Err(McpError::internal_error(error, None)),
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
            .call(
                McpRequest::ListSheets {
                    query: params.query,
                    include_misc: params.include_misc.unwrap_or(false),
                    offset: params.offset.unwrap_or(0),
                    limit: params.limit.unwrap_or(100),
                },
                Self::light_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取指定表的元信息：列数、子行、语言列表")]
    async fn get_sheet_info(
        &self,
        Parameters(params): Parameters<GetSheetInfoParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetSheetInfo { name: params.name },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取指定表的结构化模式定义（列名、类型、描述、关系映射）")]
    async fn get_sheet_schema(
        &self,
        Parameters(params): Parameters<SheetNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetSheetSchema { name: params.name },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取指定表的模式 YAML 原文")]
    async fn get_schema_raw(
        &self,
        Parameters(params): Parameters<SheetNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetSchemaRaw { name: params.name },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "检查 MCP 服务器健康状态")]
    async fn health_check(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::json!({
                "status": "ok",
                "language": format!("{:?}", self.language),
            })
            .to_string(),
        )]))
    }

    #[tool(description = "获取当前加载的游戏数据源和模式数据源信息")]
    async fn get_game_version(&self) -> Result<CallToolResult, McpError> {
        let config = self
            .config
            .lock()
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;

        let version_info = match &config.location {
            crate::settings::InstallLocation::Web(_, region, version) => serde_json::json!({
                "source": "web",
                "region": region.name(),
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

    #[tool(
        description = "校验复杂筛选 DSL 表达式语法。构造 query_rows 的 filter 前先用它检查, 不能用于搜索数据"
    )]
    async fn validate_filter(
        &self,
        Parameters(params): Parameters<ValidateFilterParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ValidateFilter {
                    expression: params.expression,
                },
                Self::light_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "校验模式 YAML 文本是否符合 EXDSchema JSON Schema 规范")]
    async fn validate_schema(
        &self,
        Parameters(params): Parameters<SaveSchemaParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ValidateSchema { text: params.text },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "按图标 ID 获取图标纹理路径")]
    async fn get_icon_url(
        &self,
        Parameters(params): Parameters<GetIconUrlParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetIconUrl {
                    icon_id: params.icon_id,
                },
                Self::light_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "分解 ModelId 字段为模型ID、变体ID、染色ID 组件")]
    async fn decompose_model_id(
        &self,
        Parameters(params): Parameters<DecomposeModelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::DecomposeModelId {
                    model_id: params.model_id,
                },
                Self::light_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "按名称模糊搜索表名")]
    async fn search_sheets(
        &self,
        Parameters(params): Parameters<SearchSheetsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::SearchSheets {
                    query: params.query,
                },
                Self::light_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "在指定表中搜索包含指定文本的字符串单元格。只接受 query 关键词, 不接受 filter DSL；需要行级条件筛选请用 query_rows"
    )]
    async fn search_cells(
        &self,
        Parameters(params): Parameters<SearchCellsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::SearchCells {
                    name: params.name,
                    query: params.query,
                    max_results: params.max_results.unwrap_or(50),
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "按行或子行分页查询表数据。需要复杂筛选 DSL 时传 filter；普通关键词搜单元格请用 search_cells；已知 row_id 请用 get_row"
    )]
    async fn query_rows(
        &self,
        Parameters(params): Parameters<QueryRowsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::QueryRows {
                    name: params.name,
                    filter: params.filter,
                    offset: params.offset.unwrap_or(0),
                    limit: params.limit.unwrap_or(50),
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "按表名和行 ID/子行 ID 精确获取单行。已知 row_id 时用这个；模糊找行请先用 query_rows 或 search_cells"
    )]
    async fn get_row(
        &self,
        Parameters(params): Parameters<GetRowParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetRow {
                    name: params.name,
                    row_id: params.row_id.unwrap_or(0),
                    subrow_id: params.subrow_id.unwrap_or(0),
                    search_name: params.search_name,
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "获取指定表的所有关系映射")]
    async fn get_sheet_relations(
        &self,
        Parameters(params): Parameters<SheetNameParam>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetSheetRelations { name: params.name },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "查询哪些表的模式中声明了指向目标表的关系")]
    async fn get_referencing_sheets(
        &self,
        Parameters(params): Parameters<GetReferencingSheetsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetReferencingSheets {
                    target_sheet: params.target_sheet,
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "沿链接字段解析目标行数据")]
    async fn follow_link(
        &self,
        Parameters(params): Parameters<FollowLinkParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::FollowLink {
                    name: params.name,
                    row_id: params.row_id,
                    column_index: params.column_index.unwrap_or(0),
                },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "解码指定单元格的 SeString 文本")]
    async fn decode_se_string(
        &self,
        Parameters(params): Parameters<DecodeSeStringParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::DecodeSeString {
                    name: params.name,
                    row_id: params.row_id,
                    subrow_id: params.subrow_id.unwrap_or(0),
                    column_index: params.column_index.unwrap_or(0),
                },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "保存模式的 YAML 文本")]
    async fn save_schema(
        &self,
        Parameters(params): Parameters<SaveSchemaParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::SaveSchema {
                    name: params.name,
                    text: params.text,
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "解析指定行在 GUI 中的主显示文本/值（根据 schema 的 display_field 定义）")]
    async fn resolve_display_field(
        &self,
        Parameters(params): Parameters<ResolveDisplayFieldParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ResolveDisplayField {
                    name: params.name,
                    row_id: params.row_id,
                    subrow_id: params.subrow_id.unwrap_or(0),
                },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

#[tool_handler(
    instructions = "EXDViewer MCP server，提供 FFXIV 游戏数据表查询、模式管理、搜索和筛选功能。工具选择规则：不确定表名先用 search_sheets；构造筛选前先用 get_sheet_schema 看字段名；行级条件筛选用 query_rows.filter；普通文本搜单元格用 search_cells.query；已知 row_id 后用 get_row 精确取行；不要把 search_cells 当 DSL 筛选使用"
)]
impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::from_build_env())
            .with_instructions(
                "EXDViewer MCP 服务器，提供 FFXIV 游戏数据表查询工具。\
                 典型流程：search_sheets 找表 -> get_sheet_schema 看字段 -> validate_filter 检查 DSL -> query_rows 执行行级筛选 -> get_row 精确取行。\
                 search_cells 只做普通文本搜索, 不接收 DSL；query_rows 才处理 filter。"
                    .to_string(),
            )
    }
}
