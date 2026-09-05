use std::{sync::Arc, time::Duration};

use ironworks::excel::Language;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};
use tokio::sync::oneshot;

use crate::settings::BackendConfig;

use super::{ColumnSelector, McpChannel, McpRequest, McpResponse, RowFormat};

#[derive(Clone, Copy, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpLanguage {
    None,
    Japanese,
    English,
    German,
    French,
    ChineseSimplified,
    ChineseTraditional,
    Korean,
    TaiwanChinese,
}

impl From<McpLanguage> for Language {
    fn from(value: McpLanguage) -> Self {
        match value {
            McpLanguage::None => Self::None,
            McpLanguage::Japanese => Self::Japanese,
            McpLanguage::English => Self::English,
            McpLanguage::German => Self::German,
            McpLanguage::French => Self::French,
            McpLanguage::ChineseSimplified => Self::ChineseSimplified,
            McpLanguage::ChineseTraditional => Self::ChineseTraditional,
            McpLanguage::Korean => Self::Korean,
            McpLanguage::TaiwanChinese => Self::TaiwanChinese,
        }
    }
}

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
    /// 精确表名, 不确定时使用 list_sheets.query
    pub name: String,
    /// 行 ID
    pub row_id: u32,
    /// 子行 ID, 仅子行表需要
    pub subrow_id: Option<u16>,
    /// 返回列, 可使用从 0 开始的列索引或 schema 列名, 默认返回全部列
    pub columns: Option<Vec<ColumnSelector>>,
    /// compact 只返回值, detailed 返回完整类型与原始数据, 默认 compact
    pub format: Option<RowFormat>,
    /// 数据语言, 默认 chinese_simplified
    pub language: Option<McpLanguage>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchCellsParams {
    /// 精确表名, 不确定时使用 list_sheets.query
    pub name: String,
    /// 必填关键词, 这是普通文本搜索, 不是筛选 DSL
    pub query: String,
    /// 只搜索这些列, 可使用列索引或 schema 列名, 默认搜索所有字符串列
    pub columns: Option<Vec<ColumnSelector>>,
    /// 从第几个物理行或子行开始扫描, 默认 0
    pub row_offset: Option<usize>,
    /// 最多扫描多少行, 默认不限制
    pub max_rows: Option<usize>,
    /// 最大返回单元格命中数, 默认 50
    pub max_results: Option<usize>,
    /// 数据语言, 默认 chinese_simplified
    pub language: Option<McpLanguage>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListEmotesParams {
    /// 名称关键词, 子串不区分大小写过滤, 默认返回全部
    pub query: Option<String>,
    /// 最大返回情感动作数, 默认 200
    pub limit: Option<usize>,
    /// 数据语言, 默认 chinese_simplified
    pub language: Option<McpLanguage>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryRowsParams {
    /// 精确表名, 不确定时使用 list_sheets.query
    pub name: String,
    /// 复杂筛选 DSL, 例如 `# = 42`, `Name *= "Potion"`, `Level >= 50 AND Name not *= Test`
    pub filter: Option<String>,
    /// 返回列, 可使用从 0 开始的列索引或 schema 列名, 默认返回全部列
    pub columns: Option<Vec<ColumnSelector>>,
    /// 匹配结果偏移量, 默认 0
    pub offset: Option<usize>,
    /// 返回匹配行数, 默认 50, 服务端会限制最大值
    pub limit: Option<usize>,
    /// 是否为获得精确 matched_rows 而扫描全部结果, 默认 false
    pub count_total: Option<bool>,
    /// 筛选链接列时是否解析目标行显示字段, 默认 false
    pub resolve_links: Option<bool>,
    /// compact 只返回值, detailed 返回完整类型与原始数据, 默认 compact
    pub format: Option<RowFormat>,
    /// 数据语言, 默认 chinese_simplified
    pub language: Option<McpLanguage>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveLinkParams {
    pub name: String,
    pub row_id: u32,
    pub subrow_id: Option<u16>,
    /// 链接列索引或 schema 列名
    pub column: ColumnSelector,
    /// 目标行返回列, 默认返回全部列
    pub target_columns: Option<Vec<ColumnSelector>>,
    pub format: Option<RowFormat>,
    pub language: Option<McpLanguage>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecodeSeStringParams {
    pub name: String,
    pub row_id: u32,
    pub subrow_id: Option<u16>,
    /// 字符串列索引或 schema 列名
    pub column: ColumnSelector,
    pub language: Option<McpLanguage>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetIconPathsParams {
    pub icon_id: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecomposeModelIdParams {
    pub model_id: String,
    /// true 按 64 位武器模型解析, false 按 32 位装备模型解析, 默认按数值宽度推断
    pub weapon: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetSheetInfoParams {
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadAssetParams {
    /// 游戏资源路径, 例如 ui/icon/000000/000001.tex
    pub path: String,
    /// 从资源字节的哪个偏移开始返回, 默认 0
    pub offset: Option<usize>,
    /// 返回字节数, 默认 4096, 服务端最多 65536
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadAssetByHashParams {
    pub repository: u8,
    pub category: u8,
    /// 十进制或 0x 开头的十六进制哈希
    pub hash: String,
    /// true 表示 .index 的拆分哈希, false 表示 .index2 的完整哈希
    pub split: Option<bool>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckAssetPathsParams {
    /// 最多传入 500 个路径
    pub paths: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListAssetPathsParams {
    /// 路径模糊查询, 默认返回全部已安装路径
    pub query: Option<String>,
    /// 是否包含全局路径列表中当前版本未安装的路径
    pub include_missing: Option<bool>,
    /// 是否附带不在全局路径列表中的哈希资源
    pub include_unnamed: Option<bool>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveAssetPathParams {
    pub path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectAssetParams {
    pub path: String,
    /// 每个解析集合最多返回多少项, 默认 100
    pub max_items: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectAssetByHashParams {
    pub repository: u8,
    pub category: u8,
    /// 十进制或 0x 开头的十六进制哈希
    pub hash: String,
    pub split: Option<bool>,
    pub max_items: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecodeTextureParams {
    pub path: String,
    /// 输出图像最长边, 默认 1024, 服务端最多 2048
    pub max_dim: Option<u16>,
}

#[derive(Clone)]
pub struct McpHandler {
    request_tx: McpChannel,
    config: Arc<BackendConfig>,
    language: Language,
    tool_router: ToolRouter<Self>,
}

impl McpHandler {
    pub fn new(request_tx: McpChannel, config: BackendConfig, language: Language) -> Self {
        Self {
            request_tx,
            config: Arc::new(config),
            language,
            tool_router: Self::tool_router(),
        }
    }

    fn language(&self, language: Option<McpLanguage>) -> Language {
        language.map_or(self.language, Into::into)
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

    fn asset_index_timeout() -> Duration {
        Duration::from_secs(120)
    }

    fn parse_hash(hash: &str) -> Result<u64, McpError> {
        let value = hash.trim();
        let parsed = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .map_or_else(|| value.parse::<u64>(), |hex| u64::from_str_radix(hex, 16))
            .map_err(|error| McpError::invalid_params(format!("哈希无效: {error}"), None))?;
        Ok(parsed)
    }

    async fn call(&self, request: McpRequest, timeout: Duration) -> Result<String, McpError> {
        let request_name = request.name();
        log::info!("MCP 请求: {request_name}");

        let tx = self.request_tx.clone();
        let response = tokio::time::timeout(timeout, async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            tx.send((request, resp_tx))
                .await
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
    #[tool(
        description = "按游戏资源路径读取原始字节, 返回流类型、大小、格式识别结果以及受限的 Base64 和十六进制字节片段"
    )]
    async fn read_asset(
        &self,
        Parameters(params): Parameters<ReadAssetParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ReadAsset {
                    path: params.path,
                    offset: params.offset.unwrap_or(0),
                    limit: params.limit,
                },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "按 repository、category、哈希和 split 读取没有路径名的资源, 返回受限原始字节"
    )]
    async fn read_asset_by_hash(
        &self,
        Parameters(params): Parameters<ReadAssetByHashParams>,
    ) -> Result<CallToolResult, McpError> {
        let hash = Self::parse_hash(&params.hash)?;
        let result = self
            .call(
                McpRequest::ReadAssetByHash {
                    repository: params.repository,
                    category: params.category,
                    hash,
                    split: params.split.unwrap_or(false),
                    offset: params.offset.unwrap_or(0),
                    limit: params.limit,
                },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "批量检查资源路径是否存在, 最多一次检查 500 个路径")]
    async fn check_asset_paths(
        &self,
        Parameters(params): Parameters<CheckAssetPathsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::CheckAssetPaths {
                    paths: params.paths,
                },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "分页查询已安装资源路径, 支持模糊搜索、包含未安装路径和附带未命名哈希资源"
    )]
    async fn list_asset_paths(
        &self,
        Parameters(params): Parameters<ListAssetPathsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ListAssetPaths {
                    api_base: self.config.api_url.clone(),
                    query: params.query,
                    include_missing: params.include_missing.unwrap_or(false),
                    include_unnamed: params.include_unnamed.unwrap_or(false),
                    offset: params.offset.unwrap_or(0),
                    limit: params.limit.unwrap_or(100),
                },
                Self::asset_index_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "计算资源路径对应的 split 和 whole 索引哈希, 并检查该路径是否存在")]
    async fn resolve_asset_path(
        &self,
        Parameters(params): Parameters<ResolveAssetPathParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ResolveAssetPath { path: params.path },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "识别并结构化解析资源, 支持纹理、PNG 图像、材质、字体、图标、ULD 布局、SHPK 着色器包、SHCD 着色器代码、SCD 声音容器、LGB 图层组、SGB 共享组、CUTB 过场动画和 TMB 时间轴"
    )]
    async fn inspect_asset(
        &self,
        Parameters(params): Parameters<InspectAssetParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::InspectAsset {
                    path: params.path,
                    max_items: params.max_items.unwrap_or(100),
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "识别并结构化解析没有路径名的哈希资源, 支持上游新增的资源格式")]
    async fn inspect_asset_by_hash(
        &self,
        Parameters(params): Parameters<InspectAssetByHashParams>,
    ) -> Result<CallToolResult, McpError> {
        let hash = Self::parse_hash(&params.hash)?;
        let result = self
            .call(
                McpRequest::InspectAssetByHash {
                    repository: params.repository,
                    category: params.category,
                    hash,
                    split: params.split.unwrap_or(false),
                    max_items: params.max_items.unwrap_or(100),
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "将游戏 TEX 纹理解码为尺寸受限的 PNG 图像, 同时返回源尺寸和输出尺寸")]
    async fn decode_texture(
        &self,
        Parameters(params): Parameters<DecodeTextureParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::DecodeTexture {
                    path: params.path,
                    max_dim: params.max_dim.unwrap_or(1024),
                },
                Self::heavy_timeout(),
            )
            .await?;
        let mut metadata: serde_json::Value = serde_json::from_str(&result).map_err(|error| {
            McpError::internal_error(format!("纹理响应解析失败: {error}"), None)
        })?;
        let png = metadata["png_base64"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| McpError::internal_error("纹理响应缺少 PNG 数据", None))?;
        if let Some(object) = metadata.as_object_mut() {
            object.remove("png_base64");
        }
        Ok(CallToolResult::success(vec![
            Content::text(metadata.to_string()),
            Content::image(png, "image/png"),
        ]))
    }

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
        let config = &self.config;

        let version_info = match &config.location {
            crate::settings::InstallLocation::Web(region, version) => serde_json::json!({
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
        let result = super::process_validate_filter(&params.expression);
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

    #[tool(description = "按图标 ID 获取普通和高分辨率纹理路径")]
    async fn get_icon_paths(
        &self,
        Parameters(params): Parameters<GetIconPathsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = super::process_get_icon_paths(params.icon_id);
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "分解 32 位装备 ModelId 或 64 位武器 ModelId 的各组件")]
    async fn decompose_model_id(
        &self,
        Parameters(params): Parameters<DecomposeModelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = match super::process_decompose_model_id(&params.model_id, params.weapon) {
            McpResponse::Success(result) => result,
            McpResponse::Error(error) => return Err(McpError::invalid_params(error, None)),
        };
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
                    columns: params.columns,
                    row_offset: params.row_offset.unwrap_or(0),
                    max_rows: params.max_rows,
                    max_results: params.max_results.unwrap_or(50),
                    language: self.language(params.language),
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "列出游戏可播放的情感动作与座椅姿态: 每个动作的名称、图标、动作键、坐骑姿势、椅子与地面变体, 以及 /cpose 循环的姿势键; 数据来自 Emote 与 ActionTimeline 表"
    )]
    async fn list_emotes(
        &self,
        Parameters(params): Parameters<ListEmotesParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ListEmotes {
                    language: self.language(params.language),
                    query: params.query,
                    limit: params.limit.unwrap_or(200),
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
                    columns: params.columns,
                    offset: params.offset.unwrap_or(0),
                    limit: params.limit.unwrap_or(50),
                    count_total: params.count_total.unwrap_or(false),
                    resolve_links: params.resolve_links.unwrap_or(false),
                    format: params.format.unwrap_or_default(),
                    language: self.language(params.language),
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
                    row_id: params.row_id,
                    subrow_id: params.subrow_id.unwrap_or(0),
                    columns: params.columns,
                    format: params.format.unwrap_or_default(),
                    language: self.language(params.language),
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

    #[tool(description = "按 schema 关系解析链接列并返回目标行, 支持条件链接和目标列筛选")]
    async fn resolve_link(
        &self,
        Parameters(params): Parameters<ResolveLinkParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ResolveLink {
                    name: params.name,
                    row_id: params.row_id,
                    subrow_id: params.subrow_id.unwrap_or(0),
                    column: params.column,
                    target_columns: params.target_columns,
                    format: params.format.unwrap_or_default(),
                    language: self.language(params.language),
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
                    column: params.column,
                    language: self.language(params.language),
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
}

#[tool_handler(
    instructions = "EXDViewer MCP server，提供 FFXIV 游戏数据表、模式和游戏资源访问能力。资源路径搜索使用 list_asset_paths，读取原始字节使用 read_asset 或 read_asset_by_hash，结构化解析使用 inspect_asset 或 inspect_asset_by_hash，查看纹理使用 decode_texture；不确定表名使用 list_sheets.query；构造筛选前先用 get_sheet_schema 看字段名；行级条件筛选用 query_rows.filter；普通文本搜单元格用 search_cells.query；已知 row_id 后用 get_row 精确取行；宽表使用 columns 限制返回列；默认 compact 输出，需要字符串原始字节等完整信息时使用 detailed"
)]
impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::from_build_env())
            .with_instructions(
                "EXDViewer MCP 服务器，提供 FFXIV 游戏数据表和游戏资源工具。\
                 数据表流程：list_sheets 查询表名 -> get_sheet_schema 看字段 -> validate_filter 检查 DSL -> query_rows 执行行级筛选 -> get_row 精确取行。\
                 资源流程：list_asset_paths 搜索路径 -> resolve_asset_path 查看索引哈希 -> read_asset 分页读取字节、inspect_asset 结构化解析或 decode_texture 查看纹理；未命名资源使用对应的 by_hash 工具。\
                 search_cells 只做普通文本搜索, 不接收 DSL；query_rows 处理 filter。"
                    .to_string(),
            )
    }
}
