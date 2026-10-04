use std::{sync::Arc, time::Duration};

use ironworks::excel::Language;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};
use tokio::sync::oneshot;

use crate::assets::SearchMode;
use crate::settings::BackendConfig;

use super::{ColumnSelector, McpChannel, McpRequest, McpResponse, RowFormat, render};

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

#[derive(Clone, Copy, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathMatchMode {
    Fuzzy,
    Strict,
    Regex,
}

impl From<PathMatchMode> for SearchMode {
    fn from(value: PathMatchMode) -> Self {
        match value {
            PathMatchMode::Fuzzy => Self::Fuzzy,
            PathMatchMode::Strict => Self::Strict,
            PathMatchMode::Regex => Self::Regex,
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
pub struct GetSheetSchemaParams {
    pub name: String,
    /// 是否附带原始模式 YAML, 默认 false
    pub include_raw: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateFilterParams {
    pub expression: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateSchemaParams {
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
pub struct QueryRowsParams {
    /// 精确表名, 不确定时使用 list_sheets.query
    pub name: String,
    /// 复杂筛选 DSL, 例如 `# = 42`, `Name *= "Potion"`, `Level >= 50 AND Name not *= Test`。
    /// 搜索某段文字用 `* *= "关键词"`, 只搜指定列用 `Name *= "关键词"`
    pub filter: Option<String>,
    /// 返回列, 可使用从 0 开始的列索引或 schema 列名, 默认返回全部列
    pub columns: Option<Vec<ColumnSelector>>,
    /// 匹配结果偏移量, 默认 0
    pub offset: Option<usize>,
    /// 返回匹配行数, 默认 50, 服务端会限制最大值
    pub limit: Option<usize>,
    /// 从第几个物理行或子行开始扫描, 默认 0
    pub row_offset: Option<usize>,
    /// 最多扫描多少行, 默认不限制
    pub max_rows: Option<usize>,
    /// 是否为获得精确 matched_rows 而扫描全部结果, 默认 false
    pub count_total: Option<bool>,
    /// 筛选链接列时是否解析目标行显示字段, 默认 false
    pub resolve_links: Option<bool>,
    /// 是否在每行上报告命中筛选条件的列, 默认 false
    pub matched_columns: Option<bool>,
    /// compact 只返回值, detailed 返回完整类型与原始数据, 默认 compact
    pub format: Option<RowFormat>,
    /// 数据语言, 默认 chinese_simplified
    pub language: Option<McpLanguage>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportSheetParams {
    /// 精确表名, 不确定时使用 list_sheets.query
    pub name: String,
    /// 复杂筛选 DSL, 省略时导出全表
    pub filter: Option<String>,
    /// 导出列, 可使用从 0 开始的列索引或 schema 列名, 默认全部列
    pub columns: Option<Vec<ColumnSelector>>,
    /// 筛选链接列时是否解析目标行显示字段, 默认 false
    pub resolve_links: Option<bool>,
    /// 最多导出多少行, 省略时不限制
    pub limit: Option<usize>,
    /// 输出位置: 以 .csv 结尾当文件名, 否则当目录并在其中以表名命名
    pub output: String,
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
pub struct InspectAssetParams {
    /// 资源路径, 也可以是一个文件夹路径, 此时对其下的每个资源逐一解析
    pub path: String,
    /// 每个解析集合最多返回多少项, 默认 100
    pub max_items: Option<usize>,
    /// 给定文件夹时, 结构化数据直接写进该文件夹下的新文件里, 而不是返回
    pub output: Option<String>,
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
pub struct FindUldUsingTextureParams {
    /// 材质纹理路径, 例如 ui/uld/Achievement.tex
    pub texture_path: String,
    /// 对返回的界面布局路径做筛选, 支持 ext: 后缀与含 / 的字面匹配, 例如 "ext:uld ui/uld/"
    pub query: Option<String>,
    /// fuzzy 模糊、strict 包含、regex 正则, 默认 fuzzy
    pub match_mode: Option<PathMatchMode>,
    /// 匹配结果偏移量, 默认 0
    pub offset: Option<usize>,
    /// 返回界面布局数量上限, 省略时返回全部
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportAssetParams {
    /// 要精确导出的资源路径, 可给多个
    pub paths: Option<Vec<String>>,
    /// 按路径筛选语法批量匹配, 支持 ext: 后缀与含 / 的字面匹配, 例如 "ext:tex ui/icon/"
    pub query: Option<String>,
    /// fuzzy 模糊、strict 包含、regex 正则, 默认 fuzzy
    pub match_mode: Option<PathMatchMode>,
    /// .scd 里要导出的音频流序号, 省略时导出全部可解码的流
    pub stream: Option<usize>,
    /// 本次最多导出多少个资源, 默认 100
    pub limit: Option<usize>,
    /// 输出目录, 其中沿用游戏路径本身的层级
    pub output: String,
    /// .tex 转 PNG 时的最长边, 默认 1024
    pub max_dim: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListIconSetsParams {
    /// 按集合名筛选, 默认返回全部集合
    pub query: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetIconSetParams {
    /// 集合名: 数据表名, 或 other 其他图标、localized 语言图标、all 全部图标
    pub set: String,
    /// 一页返回多少个图标 ID, 默认 100, 服务端最多 2000
    pub page_size: Option<usize>,
    /// 起始页, 从 1 开始, 默认 1
    pub start_page: Option<usize>,
    /// 结束页, 省略时与起始页相同
    pub end_page: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetIconParams {
    pub icon_id: u32,
    /// 是否取高分辨率纹理, 默认 true
    pub hires: Option<bool>,
    /// 输出图像最长边, 默认 512, 服务端最多 2048
    pub max_dim: Option<u16>,
    /// 数据语言, 决定取哪一份语言专有纹理, 默认 chinese_simplified
    pub language: Option<McpLanguage>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderCameraParams {
    /// 水平方位角, 单位度, 默认 0
    pub yaw: Option<f32>,
    /// 俯仰角, 单位度, 默认 8.6
    pub pitch: Option<f32>,
    /// 相机到模型中心的距离, 省略时按模型大小自动取
    pub distance: Option<f32>,
    /// 画面缩放, 同时用作字体与布局的字号倍数, 默认 1
    pub zoom: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderAssetParams {
    /// 要渲染的资源路径
    pub path: String,
    /// 输出图像最长边, 默认 1024, 服务端最多 4096
    pub max_dim: Option<u32>,
    /// 渲染字体时要排的字, 省略时把字体里的字形铺成网格
    pub text: Option<String>,
    /// 观察角度、距离与缩放
    pub camera: Option<RenderCameraParams>,
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

    /// 一条带图片的响应: 元数据留在文本里, PNG 单独作为一份图像内容交出去。
    fn with_image(result: String) -> Result<CallToolResult, McpError> {
        let mut metadata: serde_json::Value = serde_json::from_str(&result)
            .map_err(|error| McpError::internal_error(format!("图像响应解析失败: {error}"), None))?;
        let png = metadata["image_base64"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| McpError::internal_error("图像响应缺少 PNG 数据", None))?;
        if let Some(object) = metadata.as_object_mut() {
            object.remove("image_base64");
        }
        Ok(CallToolResult::success(vec![
            Content::text(metadata.to_string()),
            Content::image(png, "image/png"),
        ]))
    }

    #[cfg(test)]
    pub(super) fn tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
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

    #[tool(
        description = "识别并结构化解析资源, 返回各格式的完整数据。纹理、图像、模型、界面布局、字体与图标字体还能用 render_asset 直接渲染成图片查看; 传入文件夹路径时, 可配合 output 把每个资源的结构化数据写成文件"
    )]
    async fn inspect_asset(
        &self,
        Parameters(params): Parameters<InspectAssetParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::InspectAsset {
                    api_base: self.config.api_url.clone(),
                    path: params.path,
                    max_items: params.max_items.unwrap_or(100),
                    output: params.output,
                },
                Self::asset_index_timeout(),
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

    #[tool(
        description = "把纹理、图像、模型、界面布局、字体或图标字体渲染成可直接查看的 PNG 图; 字体可传入要排的文字, 模型可传入观察角度、距离与缩放"
    )]
    async fn render_asset(
        &self,
        Parameters(params): Parameters<RenderAssetParams>,
    ) -> Result<CallToolResult, McpError> {
        let camera = params.camera.unwrap_or_default();
        let result = self
            .call(
                McpRequest::RenderAsset {
                    path: params.path,
                    max_dim: params
                        .max_dim
                        .unwrap_or(1024)
                        .clamp(16, render::MAX_DIM),
                    text: params.text,
                    yaw: camera.yaw,
                    pitch: camera.pitch,
                    distance: camera.distance,
                    zoom: camera.zoom.unwrap_or(1.0),
                },
                Self::asset_index_timeout(),
            )
            .await?;
        Self::with_image(result)
    }

    #[tool(
        description = "输入材质 (.tex) 路径, 遍历全部界面布局 (.uld) 并返回引用该材质的布局路径; query 对返回路径做筛选, limit 省略时返回全部"
    )]
    async fn find_uld_using_texture(
        &self,
        Parameters(params): Parameters<FindUldUsingTextureParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::FindUldUsingTexture {
                    api_base: self.config.api_url.clone(),
                    texture_path: params.texture_path,
                    query: params.query,
                    match_mode: params.match_mode.unwrap_or(PathMatchMode::Fuzzy).into(),
                    offset: params.offset.unwrap_or(0),
                    limit: params.limit,
                },
                Self::asset_index_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "批量导出资源: 给 paths 精确指定, 或用 query 按路径筛选语法批量匹配。.tex 同时导出为 .png, .scd 同时导出为 .wav, 其余原样落盘"
    )]
    async fn export_asset(
        &self,
        Parameters(params): Parameters<ExportAssetParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ExportAsset {
                    api_base: self.config.api_url.clone(),
                    paths: params.paths.unwrap_or_default(),
                    query: params.query,
                    match_mode: params.match_mode.unwrap_or(PathMatchMode::Fuzzy).into(),
                    stream: params.stream,
                    limit: params.limit.unwrap_or(100),
                    output: params.output,
                    max_dim: params.max_dim.unwrap_or(1024).clamp(16, render::MAX_DIM),
                },
                Self::asset_index_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
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

    #[tool(
        description = "获取指定表的结构化模式定义（列名、类型、描述、关系映射与表元信息）, 可附带原始 YAML"
    )]
    async fn get_sheet_schema(
        &self,
        Parameters(params): Parameters<GetSheetSchemaParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetSheetSchema {
                    name: params.name,
                    include_raw: params.include_raw.unwrap_or(false),
                },
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
        description = "校验复杂筛选 DSL 表达式语法。构造 query_rows 或 export_sheet 的 filter 前先用它检查, 不能用于搜索数据"
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
        Parameters(params): Parameters<ValidateSchemaParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ValidateSchema { text: params.text },
                Self::medium_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "按行或子行分页查询表数据。复杂筛选 DSL 传 filter, 搜索某段文字用 `* *= \"关键词\"`; 已知 row_id 请用 get_row"
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
                    row_offset: params.row_offset.unwrap_or(0),
                    max_rows: params.max_rows,
                    count_total: params.count_total.unwrap_or(false),
                    resolve_links: params.resolve_links.unwrap_or(false),
                    matched_columns: params.matched_columns.unwrap_or(false),
                    format: params.format.unwrap_or_default(),
                    language: self.language(params.language),
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "把数据表导出为 .csv, 默认导出全表, 也可按复杂筛选 DSL 只导出指定的行与列")]
    async fn export_sheet(
        &self,
        Parameters(params): Parameters<ExportSheetParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ExportSheet {
                    name: params.name,
                    filter: params.filter,
                    columns: params.columns,
                    resolve_links: params.resolve_links.unwrap_or(false),
                    limit: params.limit,
                    output: params.output,
                    language: self.language(params.language),
                },
                Self::heavy_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "按表名和行 ID/子行 ID 精确获取单行。已知 row_id 时用这个；模糊找行请先用 query_rows"
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

    #[tool(
        description = "列出全部图标集合: 每个引用图标的数据表算一个集合, 另外还有 other 其他图标、localized 语言图标与 all 全部图标。可用 query 按集合名筛选"
    )]
    async fn list_icon_sets(
        &self,
        Parameters(params): Parameters<ListIconSetsParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::ListIconSets {
                    query: params.query,
                },
                Self::asset_index_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "查看一个图标集合: 返回集合内图标总数与图标 ID, 支持分页, 一页默认 100 个 ID"
    )]
    async fn get_icon_set(
        &self,
        Parameters(params): Parameters<GetIconSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetIconSet {
                    set: params.set,
                    page_size: params.page_size.unwrap_or(100),
                    start_page: params.start_page.unwrap_or(1),
                    end_page: params.end_page,
                },
                Self::asset_index_timeout(),
            )
            .await?;
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "按图标 ID 取回图标: 一次给出可直接查看的图片、纹理路径, 以及哪些数据表的哪些行在使用它"
    )]
    async fn get_icon(
        &self,
        Parameters(params): Parameters<GetIconParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .call(
                McpRequest::GetIcon {
                    icon_id: params.icon_id,
                    hires: params.hires.unwrap_or(true),
                    max_dim: params.max_dim.unwrap_or(512).clamp(1, 2048) as u32,
                    language: self.language(params.language),
                },
                Self::asset_index_timeout(),
            )
            .await?;
        Self::with_image(result)
    }
}

#[tool_handler(
    instructions = "EXDViewer MCP server，提供 FFXIV 游戏数据表、模式和游戏资源访问能力。资源路径搜索使用 list_asset_paths，读取原始字节使用 read_asset 或 read_asset_by_hash，结构化解析使用 inspect_asset 或 inspect_asset_by_hash，把纹理、图像、模型、布局、字体与图标字体画成图片使用 render_asset，批量导出资源使用 export_asset，按材质反查引用它的界面布局使用 find_uld_using_texture；不确定表名使用 list_sheets.query；构造筛选前先用 get_sheet_schema 看字段名；行级条件筛选用 query_rows.filter，搜索某段文字用 `* *= \"关键词\"`；已知 row_id 后用 get_row 精确取行；整表或筛选结果落盘使用 export_sheet；图标集合用 list_icon_sets 与 get_icon_set，单个图标的图片、路径与使用位置用 get_icon；宽表使用 columns 限制返回列；默认 compact 输出，需要字符串原始字节等完整信息时使用 detailed"
)]
impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::from_build_env())
            .with_instructions(
                "EXDViewer MCP 服务器，提供 FFXIV 游戏数据表和游戏资源工具。\
                 数据表流程：list_sheets 查询表名 -> get_sheet_schema 看字段 -> validate_filter 检查 DSL -> query_rows 执行行级筛选 -> get_row 精确取行；导出用 export_sheet。\
                 资源流程：list_asset_paths 搜索路径 -> read_asset 分页读取字节、inspect_asset 结构化解析或 render_asset 渲染成图片；未命名资源使用对应的 by_hash 工具；批量导出用 export_asset；按材质反查引用它的界面布局用 find_uld_using_texture。\
                 图标流程：list_icon_sets 查看集合 -> get_icon_set 取集合内的 ID -> get_icon 取单个图标的图片、纹理路径与使用位置。"
                    .to_string(),
            )
    }
}
