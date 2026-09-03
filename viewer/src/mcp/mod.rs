use std::{
    cell::RefCell,
    collections::HashMap,
    num::{NonZeroU32, NonZeroUsize},
    str::FromStr,
    time::Duration,
};

use crate::schema::Schema as ExdSchema;
use crate::{
    backend::Backend,
    excel::{
        base::BaseSheet,
        provider::{ExcelHeader, ExcelProvider, ExcelSheet},
    },
    schema::provider::SchemaProvider,
    settings::BackendConfig,
    sheet::{
        CellValue, CompiledFilterInput, ComplexFilter, FilterInput, GlobalContext, MatchOptions,
        SchemaColumn, SchemaColumnMeta, TableContext,
    },
    utils::IconManager,
};
use futures_util::{StreamExt, stream};
use handler::McpHandler;
use ironworks::excel::Language;
use lru::LruCache;
use tokio::sync::{mpsc, oneshot};

mod assets;
mod handler;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ColumnSelector {
    Index(usize),
    Name(String),
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowFormat {
    #[default]
    Compact,
    Detailed,
}

#[derive(Clone, Debug)]
pub enum McpRequest {
    ReadAsset {
        path: String,
        offset: usize,
        limit: Option<usize>,
    },
    ReadAssetByHash {
        repository: u8,
        category: u8,
        hash: u64,
        split: bool,
        offset: usize,
        limit: Option<usize>,
    },
    CheckAssetPaths {
        paths: Vec<String>,
    },
    ListAssetPaths {
        api_base: String,
        query: Option<String>,
        include_missing: bool,
        include_unnamed: bool,
        offset: usize,
        limit: usize,
    },
    ResolveAssetPath {
        path: String,
    },
    InspectAsset {
        path: String,
        max_items: usize,
    },
    InspectAssetByHash {
        repository: u8,
        category: u8,
        hash: u64,
        split: bool,
        max_items: usize,
    },
    DecodeTexture {
        path: String,
        max_dim: u16,
    },
    ListSheets {
        query: Option<String>,
        include_misc: bool,
        offset: usize,
        limit: usize,
    },
    GetSheetInfo {
        name: String,
    },
    GetSheetSchema {
        name: String,
    },
    GetSchemaRaw {
        name: String,
    },
    SearchCells {
        name: String,
        query: String,
        columns: Option<Vec<ColumnSelector>>,
        row_offset: usize,
        max_rows: Option<usize>,
        max_results: usize,
        language: Language,
    },
    QueryRows {
        name: String,
        filter: Option<String>,
        columns: Option<Vec<ColumnSelector>>,
        offset: usize,
        limit: usize,
        count_total: bool,
        resolve_links: bool,
        format: RowFormat,
        language: Language,
    },
    GetRow {
        name: String,
        row_id: u32,
        subrow_id: u16,
        columns: Option<Vec<ColumnSelector>>,
        format: RowFormat,
        language: Language,
    },
    ValidateSchema {
        text: String,
    },
    GetSheetRelations {
        name: String,
    },
    GetReferencingSheets {
        target_sheet: String,
    },
    ResolveLink {
        name: String,
        row_id: u32,
        subrow_id: u16,
        column: ColumnSelector,
        target_columns: Option<Vec<ColumnSelector>>,
        format: RowFormat,
        language: Language,
    },
    DecodeSeString {
        name: String,
        row_id: u32,
        subrow_id: u16,
        column: ColumnSelector,
        language: Language,
    },
    SaveSchema {
        name: String,
        text: String,
    },
}

impl McpRequest {
    fn name(&self) -> &'static str {
        match self {
            Self::ReadAsset { .. } => "read_asset",
            Self::ReadAssetByHash { .. } => "read_asset_by_hash",
            Self::CheckAssetPaths { .. } => "check_asset_paths",
            Self::ListAssetPaths { .. } => "list_asset_paths",
            Self::ResolveAssetPath { .. } => "resolve_asset_path",
            Self::InspectAsset { .. } => "inspect_asset",
            Self::InspectAssetByHash { .. } => "inspect_asset_by_hash",
            Self::DecodeTexture { .. } => "decode_texture",
            Self::ListSheets { .. } => "list_sheets",
            Self::GetSheetInfo { .. } => "get_sheet_info",
            Self::GetSheetSchema { .. } => "get_sheet_schema",
            Self::GetSchemaRaw { .. } => "get_schema_raw",
            Self::SearchCells { .. } => "search_cells",
            Self::QueryRows { .. } => "query_rows",
            Self::GetRow { .. } => "get_row",
            Self::ValidateSchema { .. } => "validate_schema",
            Self::GetSheetRelations { .. } => "get_sheet_relations",
            Self::GetReferencingSheets { .. } => "get_referencing_sheets",
            Self::ResolveLink { .. } => "resolve_link",
            Self::DecodeSeString { .. } => "decode_se_string",
            Self::SaveSchema { .. } => "save_schema",
        }
    }
}

pub enum McpResponse {
    Success(String),
    Error(String),
}

type McpChannel = mpsc::Sender<(McpRequest, oneshot::Sender<McpResponse>)>;

#[derive(Clone)]
struct SchemaSnapshot {
    raw_text: String,
    parsed: ParsedSchema,
}

#[derive(Clone)]
enum ParsedSchema {
    Valid(ExdSchema),
    Invalid(Vec<String>),
}

impl SchemaSnapshot {
    fn validation_errors(&self) -> Vec<String> {
        match &self.parsed {
            ParsedSchema::Valid(_) => Vec::new(),
            ParsedSchema::Invalid(errors) => errors.clone(),
        }
    }

    fn parsed_schema(&self) -> Option<&ExdSchema> {
        match &self.parsed {
            ParsedSchema::Valid(schema) => Some(schema),
            ParsedSchema::Invalid(_) => None,
        }
    }
}

thread_local! {
    static SCHEMA_CACHE: RefCell<Option<LruCache<String, SchemaSnapshot>>> = const { RefCell::new(None) };
    static TABLE_CACHE: RefCell<Option<LruCache<(String, Language), TableContext>>> = const { RefCell::new(None) };
    static SHEET_INDEX: RefCell<Option<Vec<SheetIndexEntry>>> = const { RefCell::new(None) };
    static RELATION_INDEX: RefCell<Option<HashMap<String, Vec<serde_json::Value>>>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct SheetIndexEntry {
    name: String,
    id: i32,
    lowercase_name: String,
}

fn with_schema_cache<T>(func: impl FnOnce(&mut LruCache<String, SchemaSnapshot>) -> T) -> T {
    SCHEMA_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.is_none() {
            *cache = Some(LruCache::new(NonZeroUsize::new(64).unwrap()));
        }
        func(cache.as_mut().expect("schema cache initialized"))
    })
}

async fn load_schema_snapshot(backend: &Backend, name: &str) -> anyhow::Result<SchemaSnapshot> {
    if let Some(snapshot) = with_schema_cache(|cache| cache.get(name).cloned()) {
        return Ok(snapshot);
    }

    let raw_text = backend.schema().get_schema_text(name).await?;
    let parsed = match ExdSchema::from_str(&raw_text) {
        Ok(Ok(schema)) => ParsedSchema::Valid(schema),
        Ok(Err(errors)) => ParsedSchema::Invalid(
            errors
                .iter()
                .map(|error| format!("{} at {}", error.description, error.location))
                .collect(),
        ),
        Err(error) => ParsedSchema::Invalid(vec![format!("{error}")]),
    };
    let snapshot = SchemaSnapshot { raw_text, parsed };
    with_schema_cache(|cache| {
        cache.put(name.to_owned(), snapshot.clone());
    });
    Ok(snapshot)
}

fn invalidate_schema_snapshot(name: &str) {
    with_schema_cache(|cache| {
        cache.pop(name);
    });
    TABLE_CACHE.with(|cell| {
        let mut cache_ref = cell.borrow_mut();
        let Some(cache) = cache_ref.as_mut() else {
            return;
        };
        let keys = cache
            .iter()
            .filter(|((sheet_name, _), _)| sheet_name == name)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            cache.pop(&key);
        }
    });
    RELATION_INDEX.with(|cache| cache.borrow_mut().take());
}

fn schema_column_type_name(meta: &SchemaColumnMeta) -> &'static str {
    match meta {
        SchemaColumnMeta::Scalar => "Scalar",
        SchemaColumnMeta::Icon => "Icon",
        SchemaColumnMeta::ModelId => "ModelId",
        SchemaColumnMeta::Color => "Color",
        SchemaColumnMeta::Link(_) | SchemaColumnMeta::ConditionalLink { .. } => "Link",
    }
}

fn cell_value_to_json(value: &CellValue) -> serde_json::Value {
    value.to_structured_value()
}

async fn build_table_context(
    backend: &Backend,
    name: &str,
    sheet: BaseSheet,
    lang: Language,
) -> anyhow::Result<TableContext> {
    let key = (name.to_owned(), lang);
    if let Some(table) = TABLE_CACHE.with(|cell| {
        cell.borrow_mut()
            .as_mut()
            .and_then(|cache| cache.get(&key).cloned())
    }) {
        return Ok(table);
    }
    let schema = match load_schema_snapshot(backend, name).await {
        Ok(snapshot) => snapshot.parsed_schema().cloned(),
        Err(_) => None,
    };

    let global = GlobalContext::new(
        egui::Context::default(),
        backend.clone(),
        lang,
        IconManager::new(),
    );
    let table = TableContext::new(global, sheet, schema.as_ref());
    TABLE_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        cache
            .get_or_insert_with(|| LruCache::new(NonZeroUsize::new(32).unwrap()))
            .put(key, table.clone());
    });
    Ok(table)
}

fn filter_match_options(resolve_links: bool) -> MatchOptions {
    MatchOptions {
        case_insensitive: true,
        use_display_field: resolve_links,
    }
}

fn row_locations(sheet: &impl ExcelSheet) -> Box<dyn Iterator<Item = (u32, Option<u16>)> + '_> {
    if sheet.has_subrows() {
        Box::new(
            sheet
                .get_subrow_ids()
                .map(|(row_id, subrow_id)| (row_id, Some(subrow_id))),
        )
    } else {
        Box::new(sheet.get_row_ids().map(|row_id| (row_id, None)))
    }
}

fn row_location_count(sheet: &impl ExcelSheet) -> usize {
    if sheet.has_subrows() {
        sheet.subrow_count() as usize
    } else {
        sheet.row_count() as usize
    }
}

fn get_row_at(
    sheet: &BaseSheet,
    row_id: u32,
    subrow_id: Option<u16>,
) -> anyhow::Result<crate::excel::provider::ExcelRow<'_>> {
    match subrow_id {
        Some(subrow_id) => sheet.get_subrow(row_id, subrow_id),
        None => sheet.get_row(row_id),
    }
}

async fn filter_row(
    table: &TableContext,
    row_id: u32,
    subrow_id: Option<u16>,
    row: &crate::excel::provider::ExcelRow<'_>,
    filter: &CompiledFilterInput,
    resolve_links: bool,
) -> anyhow::Result<bool> {
    if !resolve_links {
        return table
            .filter_row(row_id, subrow_id, row, filter)
            .map(|result| result.0);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (matched, in_progress) = table.filter_row(row_id, subrow_id, row, filter)?;
        if !in_progress {
            return Ok(matched);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("解析链接显示字段超时");
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn score_row(
    table: &TableContext,
    row_id: u32,
    subrow_id: Option<u16>,
    row: &crate::excel::provider::ExcelRow<'_>,
    filter: &CompiledFilterInput,
    resolve_links: bool,
) -> anyhow::Result<Option<NonZeroU32>> {
    if !resolve_links {
        return table
            .score_row(row_id, subrow_id, row, filter)
            .map(|result| result.0);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (score, in_progress) = table.score_row(row_id, subrow_id, row, filter)?;
        if !in_progress {
            return Ok(score);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("解析链接显示字段超时");
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[derive(Clone)]
struct SelectedColumn {
    index: usize,
    schema: crate::sheet::SchemaColumn,
    sheet: crate::sheet::SheetColumnDefinition,
}

fn select_columns(
    table: &TableContext,
    selectors: Option<&[ColumnSelector]>,
) -> anyhow::Result<Vec<SelectedColumn>> {
    let columns = table.columns()?;
    let indices = match selectors {
        None => (0..columns.len()).collect(),
        Some(selectors) => {
            let mut indices = Vec::with_capacity(selectors.len());
            for selector in selectors {
                let index = match selector {
                    ColumnSelector::Index(index) => *index,
                    ColumnSelector::Name(name) => columns
                        .iter()
                        .position(|(schema, _)| schema.name() == name)
                        .ok_or_else(|| anyhow::anyhow!("未找到列 '{name}'"))?,
                };
                if index >= columns.len() {
                    anyhow::bail!("列索引 {index} 越界, 当前表共有 {} 列", columns.len());
                }
                if !indices.contains(&index) {
                    indices.push(index);
                }
            }
            indices
        }
    };

    Ok(indices
        .into_iter()
        .map(|index| {
            let (schema, sheet) = columns[index].clone();
            SelectedColumn {
                index,
                schema,
                sheet,
            }
        })
        .collect())
}

fn columns_to_json(columns: &[SelectedColumn], display_idx: Option<u32>) -> Vec<serde_json::Value> {
    columns
        .iter()
        .map(|column| {
            serde_json::json!({
                "index": column.index,
                "name": column.schema.name(),
                "type": schema_column_type_name(column.schema.meta()),
                "storage": format!("{:?}", column.sheet.kind()),
                "offset": column.sheet.offset(),
                "is_display": display_idx == Some(column.index as u32)
            })
        })
        .collect()
}

fn compact_cell_value(value: &CellValue) -> serde_json::Value {
    match value {
        CellValue::String(_) => serde_json::json!(value.display_text().to_string()),
        CellValue::Integer(value) => serde_json::json!(value),
        CellValue::Float(value) => serde_json::json!(value),
        CellValue::Boolean(value) => serde_json::json!(value),
        CellValue::Icon(value) => serde_json::json!(value),
        CellValue::ModelId(value) => value.either(
            |value| serde_json::json!(value),
            |value| serde_json::json!(value),
        ),
        CellValue::Color(value) => serde_json::json!(u32::from_le_bytes(value.to_array())),
        CellValue::InvalidLink(value) | CellValue::InProgressLink(value) => {
            serde_json::json!(value)
        }
        CellValue::ValidLink {
            sheet_name,
            row_id,
            value,
        } => serde_json::json!({
            "sheet": sheet_name.to_string(),
            "row_id": row_id,
            "display": value.as_ref().map(|value| value.display_text().to_string())
        }),
    }
}

fn read_row_values(
    table: &TableContext,
    row: crate::excel::provider::ExcelRow<'_>,
    columns: &[SelectedColumn],
    format: RowFormat,
) -> anyhow::Result<Vec<serde_json::Value>> {
    columns
        .iter()
        .map(|column| {
            let value = table
                .cell_by_offset(row, column.index as u32)?
                .read(false)?;
            Ok(match format {
                RowFormat::Compact => compact_cell_value(&value),
                RowFormat::Detailed => cell_value_to_json(&value),
            })
        })
        .collect()
}

fn build_row_object(
    table: &TableContext,
    row: crate::excel::provider::ExcelRow<'_>,
    row_id: u32,
    subrow_id: Option<u16>,
    columns: &[SelectedColumn],
    format: RowFormat,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let mut row_obj = serde_json::Map::new();
    row_obj.insert("row_id".into(), serde_json::json!(row_id));
    row_obj.insert("subrow_id".into(), serde_json::json!(subrow_id));
    row_obj.insert(
        "values".into(),
        serde_json::Value::Array(read_row_values(table, row, columns, format)?),
    );
    Ok(row_obj)
}

fn process_list_sheets(
    backend: &Backend,
    query: Option<&str>,
    include_misc: bool,
    offset: usize,
    limit: usize,
) -> String {
    let lowercase_query = query.map(str::to_ascii_lowercase);
    let page_limit = limit.min(500);
    SHEET_INDEX.with(|cache| {
        let mut cache = cache.borrow_mut();
        let sheets = cache.get_or_insert_with(|| {
            let mut sheets = backend
                .excel()
                .get_entries()
                .iter()
                .map(|(name, id)| SheetIndexEntry {
                    name: name.clone(),
                    id: *id,
                    lowercase_name: name.to_ascii_lowercase(),
                })
                .collect::<Vec<_>>();
            sheets.sort_unstable_by(|a, b| a.lowercase_name.cmp(&b.lowercase_name));
            sheets
        });
        let mut filtered = sheets.iter().filter(|sheet| {
            (include_misc || sheet.id >= 0)
                && lowercase_query
                    .as_ref()
                    .is_none_or(|query| sheet.lowercase_name.contains(query))
        });
        let total = filtered.clone().count();
        let page = filtered
            .by_ref()
            .skip(offset)
            .take(page_limit)
            .map(|sheet| serde_json::json!({"name": sheet.name, "id": sheet.id}))
            .collect::<Vec<_>>();
        serde_json::json!({
            "total": total,
            "offset": offset,
            "limit": page_limit,
            "has_more": offset.saturating_add(page.len()) < total,
            "sheets": page
        })
        .to_string()
    })
}

fn process_validate_filter(expression: &str) -> String {
    use crate::sheet::ComplexFilter;
    use std::str::FromStr;
    match ComplexFilter::from_str(expression) {
        Ok(f) => {
            serde_json::json!({"valid": true, "expression": expression, "ast": format!("{f:?}")})
                .to_string()
        }
        Err(e) => {
            serde_json::json!({"valid": false, "expression": expression, "error": e}).to_string()
        }
    }
}

fn process_validate_schema(text: &str) -> String {
    use crate::schema::Schema;
    match Schema::from_str(text) {
        Ok(Ok(s)) => serde_json::json!({"valid": true, "name": s.name, "field_count": s.fields.len()})
            .to_string(),
        Ok(Err(e)) => serde_json::json!({"valid": false, "errors": e.iter().map(|e| format!("{} at {}", e.description, e.location)).collect::<Vec<_>>()})
            .to_string(),
        Err(e) => serde_json::json!({"valid": false, "error": format!("{e}")}).to_string(),
    }
}

fn process_get_icon_paths(icon_id: u32) -> String {
    let path = crate::data::get_icon_path(None, icon_id, false, Language::None);
    let hires_path = crate::data::get_icon_path(None, icon_id, true, Language::None);
    serde_json::json!({"icon_id": icon_id, "tex_path": path, "hires_tex_path": hires_path})
        .to_string()
}

fn process_decompose_model_id(model_id: &str, weapon: Option<bool>) -> McpResponse {
    match model_id.parse::<u64>() {
        Ok(value) if !weapon.unwrap_or(value > u64::from(u32::MAX)) => {
            let Ok(value) = u32::try_from(value) else {
                return McpResponse::Error("32 位装备 ModelId 超出范围".into());
            };
            McpResponse::Success(
                serde_json::json!({
                    "kind": "model",
                    "raw": value,
                    "model": (value & 0xFFFF) as u16,
                    "variant": ((value >> 16) & 0xFF) as u8,
                    "stain": ((value >> 24) & 0xFF) as u8
                })
                .to_string(),
            )
        }
        Ok(value) => McpResponse::Success(
            serde_json::json!({
                "kind": "weapon",
                "raw": value,
                "skeleton": (value & 0xFFFF) as u16,
                "model": ((value >> 16) & 0xFFFF) as u16,
                "variant": ((value >> 32) & 0xFFFF) as u16,
                "stain": ((value >> 48) & 0xFFFF) as u16
            })
            .to_string(),
        ),
        Err(e) => McpResponse::Error(format!("无法解析: {e}")),
    }
}

async fn process_get_sheet_info(backend: &Backend, name: &str) -> McpResponse {
    let excel = backend.excel();
    match excel.get_header(name).await {
        Ok(header) => {
            let languages: Vec<String> = header
                .languages()
                .iter()
                .map(|l| format!("{l:?}"))
                .collect();
            McpResponse::Success(
                serde_json::json!({
                    "name": header.name(),
                    "column_count": header.columns().len(),
                    "has_subrows": header.has_subrows(),
                    "languages": languages
                })
                .to_string(),
            )
        }
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

async fn process_get_sheet_schema(backend: &Backend, name: &str) -> McpResponse {
    match load_schema_snapshot(backend, name).await {
        Ok(snapshot) => match snapshot.parsed_schema() {
            Some(schema) => {
                let (expanded_columns, display_column_index) =
                    match SchemaColumn::from_schema(schema) {
                        Ok(columns) => columns,
                        Err(e) => return McpResponse::Error(format!("{e}")),
                    };
                let columns = expanded_columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        serde_json::json!({
                            "index": index,
                            "name": column.name(),
                            "type": schema_column_type_name(column.meta()),
                            "comment": column.comment(),
                            "is_display": display_column_index == Some(index as u32)
                        })
                    })
                    .collect::<Vec<_>>();
                McpResponse::Success(
                    serde_json::json!({
                        "name": schema.name,
                        "display_field": schema.display_field,
                        "fields": schema.fields,
                        "columns": columns,
                        "relations": schema.relations,
                        "field_count": schema.fields.len(),
                        "column_count": columns.len()
                    })
                    .to_string(),
                )
            }
            None => McpResponse::Success(
                serde_json::json!({
                    "raw_yaml": snapshot.raw_text,
                    "valid": false,
                    "errors": snapshot.validation_errors()
                })
                .to_string(),
            ),
        },
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

async fn process_get_schema_raw(backend: &Backend, name: &str) -> McpResponse {
    match load_schema_snapshot(backend, name).await {
        Ok(snapshot) => McpResponse::Success(
            serde_json::json!({"name": name, "yaml": snapshot.raw_text}).to_string(),
        ),
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

fn collect_field_references(
    fields: &[crate::schema::Field],
    scope: &str,
    references: &mut Vec<serde_json::Value>,
) {
    for field in fields {
        let field_name = field.name.as_deref().unwrap_or("Unk");
        let path = if scope.is_empty() {
            field_name.to_owned()
        } else {
            format!("{scope}.{field_name}")
        };
        if let Some(targets) = &field.targets {
            references.extend(targets.iter().map(
                |target| serde_json::json!({"field": path, "target": target, "type": "link"}),
            ));
        }
        if let Some(condition) = &field.condition {
            for (value, targets) in &condition.cases {
                references.extend(targets.iter().map(|target| {
                    serde_json::json!({
                        "field": path,
                        "target": target,
                        "type": "conditional_link",
                        "switch": condition.switch,
                        "switch_value": value
                    })
                }));
            }
        }
        if let Some(relations) = &field.relations {
            references.extend(relations.keys().map(
                |target| serde_json::json!({"field": path, "target": target, "type": "relation"}),
            ));
        }
        if let Some(nested) = &field.fields {
            collect_field_references(nested, &path, references);
        }
    }
}

fn schema_references(schema: &ExdSchema) -> Vec<serde_json::Value> {
    let mut references = Vec::new();
    collect_field_references(&schema.fields, "", &mut references);
    if let Some(relations) = &schema.relations {
        references.extend(
            relations
                .keys()
                .map(|target| serde_json::json!({"target": target, "type": "sheet_relation"})),
        );
    }
    references
}

async fn process_get_sheet_relations(backend: &Backend, name: &str) -> McpResponse {
    match load_schema_snapshot(backend, name).await {
        Ok(snapshot) => match snapshot.parsed_schema() {
            Some(schema) => McpResponse::Success(
                serde_json::json!({"name": name, "relations": schema_references(schema)})
                    .to_string(),
            ),
            None => McpResponse::Error(format!(
                "无法解析模式: {}",
                snapshot.validation_errors().join("; ")
            )),
        },
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

async fn process_get_referencing_sheets(backend: &Backend, target_sheet: &str) -> McpResponse {
    if let Some(references) =
        RELATION_INDEX.with(|cache| cache.borrow().as_ref()?.get(target_sheet).cloned())
    {
        return McpResponse::Success(
            serde_json::json!({
                "target_sheet": target_sheet,
                "count": references.len(),
                "references": references
            })
            .to_string(),
        );
    }

    let sheet_names = backend
        .excel()
        .get_entries()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut schemas = stream::iter(sheet_names)
        .map(|sheet_name| async move {
            let schema = load_schema_snapshot(backend, &sheet_name)
                .await
                .ok()
                .and_then(|snapshot| snapshot.parsed_schema().cloned());
            (sheet_name, schema)
        })
        .buffer_unordered(16);
    let mut index: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    while let Some((sheet_name, schema)) = schemas.next().await {
        let Some(schema) = schema else {
            continue;
        };
        for reference in schema_references(&schema) {
            let Some(target) = reference["target"].as_str() else {
                continue;
            };
            index
                .entry(target.to_owned())
                .or_default()
                .push(serde_json::json!({"sheet": sheet_name, "reference": reference}));
        }
    }
    for references in index.values_mut() {
        references.sort_unstable_by(|a, b| a["sheet"].as_str().cmp(&b["sheet"].as_str()));
    }
    let references = index.get(target_sheet).cloned().unwrap_or_default();
    RELATION_INDEX.with(|cache| cache.replace(Some(index)));

    McpResponse::Success(
        serde_json::json!({
            "target_sheet": target_sheet,
            "count": references.len(),
            "references": references
        })
        .to_string(),
    )
}

struct QueryRowsOptions<'a> {
    name: &'a str,
    filter: Option<&'a str>,
    columns: Option<&'a [ColumnSelector]>,
    offset: usize,
    limit: usize,
    count_total: bool,
    resolve_links: bool,
    format: RowFormat,
    language: Language,
}

async fn process_query_rows(backend: &Backend, options: QueryRowsOptions<'_>) -> McpResponse {
    let QueryRowsOptions {
        name,
        filter,
        columns: column_selectors,
        offset,
        limit,
        count_total,
        resolve_links,
        format,
        language: lang,
    } = options;
    let excel = backend.excel();
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let table_context = match build_table_context(backend, name, sheet.clone(), lang).await {
        Ok(ctx) => ctx,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let columns = match select_columns(&table_context, column_selectors) {
        Ok(columns) => columns,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let total_rows = row_location_count(&sheet);
    let page_limit = limit.min(200);
    let compiled_filter = match filter {
        Some(text) if !text.trim().is_empty() => {
            let parsed = match ComplexFilter::from_str(text) {
                Ok(filter) => FilterInput::Complex(filter),
                Err(e) => {
                    return McpResponse::Error(format!(
                        "筛选表达式无效: {e}。先用 validate_filter 检查语法"
                    ));
                }
            };
            match table_context.compile_filter(&parsed, filter_match_options(resolve_links)) {
                Ok(filter) => Some(filter),
                Err(e) => return McpResponse::Error(format!("{e}")),
            }
        }
        _ => None,
    };

    let mut rows = Vec::with_capacity(page_limit);
    let matched_rows;
    let has_more;
    let mut scanned_rows = 0;

    if let Some(compiled_filter) = compiled_filter.as_ref() {
        let fuzzy = compiled_filter.input().is_some_and(|input| input.has_fuzzy);
        if fuzzy {
            let mut matched = Vec::new();
            for (sequence, (row_id, subrow_id)) in row_locations(&sheet).enumerate() {
                if sequence % 256 == 0 {
                    tokio::task::yield_now().await;
                }
                scanned_rows += 1;
                let row = match get_row_at(&sheet, row_id, subrow_id) {
                    Ok(row) => row,
                    Err(_) => continue,
                };
                let is_match = match filter_row(
                    &table_context,
                    row_id,
                    subrow_id,
                    &row,
                    compiled_filter,
                    resolve_links,
                )
                .await
                {
                    Ok(result) => result,
                    Err(e) => return McpResponse::Error(format!("{e}")),
                };
                if !is_match {
                    continue;
                }
                let score = match score_row(
                    &table_context,
                    row_id,
                    subrow_id,
                    &row,
                    compiled_filter,
                    resolve_links,
                )
                .await
                {
                    Ok(score) => score,
                    Err(e) => return McpResponse::Error(format!("{e}")),
                };
                matched.push((sequence, row_id, subrow_id, score));
            }
            matched.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));
            matched_rows = Some(matched.len());
            has_more = offset.saturating_add(page_limit) < matched.len();
            for (sequence, row_id, subrow_id, score) in
                matched.into_iter().skip(offset).take(page_limit)
            {
                let row = match get_row_at(&sheet, row_id, subrow_id) {
                    Ok(row) => row,
                    Err(_) => continue,
                };
                let mut row_obj = match build_row_object(
                    &table_context,
                    row,
                    row_id,
                    subrow_id,
                    &columns,
                    format,
                ) {
                    Ok(row) => row,
                    Err(e) => return McpResponse::Error(format!("{e}")),
                };
                row_obj.insert("row_index".into(), serde_json::json!(sequence));
                if let Some(score) = score {
                    row_obj.insert("match_score".into(), serde_json::json!(score.get()));
                }
                rows.push(serde_json::Value::Object(row_obj));
            }
        } else {
            let mut match_count = 0;
            let mut found_more = false;
            for (sequence, (row_id, subrow_id)) in row_locations(&sheet).enumerate() {
                if sequence % 256 == 0 {
                    tokio::task::yield_now().await;
                }
                scanned_rows += 1;
                let row = match get_row_at(&sheet, row_id, subrow_id) {
                    Ok(row) => row,
                    Err(_) => continue,
                };
                let is_match = match filter_row(
                    &table_context,
                    row_id,
                    subrow_id,
                    &row,
                    compiled_filter,
                    resolve_links,
                )
                .await
                {
                    Ok(result) => result,
                    Err(e) => return McpResponse::Error(format!("{e}")),
                };
                if !is_match {
                    continue;
                }
                if match_count >= offset && rows.len() < page_limit {
                    let mut row_obj = match build_row_object(
                        &table_context,
                        row,
                        row_id,
                        subrow_id,
                        &columns,
                        format,
                    ) {
                        Ok(row) => row,
                        Err(e) => return McpResponse::Error(format!("{e}")),
                    };
                    row_obj.insert("row_index".into(), serde_json::json!(sequence));
                    rows.push(serde_json::Value::Object(row_obj));
                } else if !count_total && match_count >= offset.saturating_add(page_limit) {
                    found_more = true;
                    break;
                }
                match_count += 1;
            }
            has_more = found_more || offset.saturating_add(page_limit) < match_count;
            matched_rows = (!found_more).then_some(match_count);
        }
    } else {
        matched_rows = Some(total_rows);
        has_more = offset.saturating_add(page_limit) < total_rows;
        for (sequence, (row_id, subrow_id)) in row_locations(&sheet)
            .enumerate()
            .skip(offset)
            .take(page_limit)
        {
            scanned_rows += 1;
            let row = match get_row_at(&sheet, row_id, subrow_id) {
                Ok(row) => row,
                Err(_) => continue,
            };
            let mut row_obj =
                match build_row_object(&table_context, row, row_id, subrow_id, &columns, format) {
                    Ok(row_obj) => row_obj,
                    Err(e) => return McpResponse::Error(format!("{e}")),
                };
            row_obj.insert("row_index".into(), serde_json::json!(sequence));
            rows.push(serde_json::Value::Object(row_obj));
        }
    }

    McpResponse::Success(
        serde_json::json!({
            "sheet": name,
            "language": format!("{lang:?}"),
            "filter": filter,
            "offset": offset,
            "limit": page_limit,
            "total_rows": total_rows,
            "matched_rows": matched_rows,
            "has_more": has_more,
            "scanned_rows": scanned_rows,
            "format": match format { RowFormat::Compact => "compact", RowFormat::Detailed => "detailed" },
            "columns": columns_to_json(&columns, table_context.display_column_idx()),
            "rows": rows
        })
        .to_string(),
    )
}

async fn process_get_row(
    backend: &Backend,
    name: &str,
    row_id: u32,
    subrow_id: u16,
    column_selectors: Option<&[ColumnSelector]>,
    format: RowFormat,
    lang: Language,
) -> McpResponse {
    let excel = backend.excel();
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let table_context = match build_table_context(backend, name, sheet.clone(), lang).await {
        Ok(ctx) => ctx,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let columns = match select_columns(&table_context, column_selectors) {
        Ok(columns) => columns,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let row = match sheet.get_subrow(row_id, subrow_id) {
        Ok(r) => r,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let values = match read_row_values(&table_context, row, &columns, format) {
        Ok(values) => values,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    McpResponse::Success(
        serde_json::json!({
            "sheet": name,
            "row_id": row_id,
            "subrow_id": subrow_id,
            "column_count": table_context.column_count(),
            "format": match format { RowFormat::Compact => "compact", RowFormat::Detailed => "detailed" },
            "columns": columns_to_json(&columns, table_context.display_column_idx()),
            "values": values
        })
        .to_string(),
    )
}

struct SearchCellsOptions<'a> {
    name: &'a str,
    query: &'a str,
    columns: Option<&'a [ColumnSelector]>,
    row_offset: usize,
    max_rows: Option<usize>,
    max_results: usize,
    language: Language,
}

async fn process_search_cells(backend: &Backend, options: SearchCellsOptions<'_>) -> McpResponse {
    let SearchCellsOptions {
        name,
        query,
        columns: column_selectors,
        row_offset,
        max_rows,
        max_results,
        language: lang,
    } = options;
    if query.trim().is_empty() {
        return McpResponse::Error("query 不能为空".into());
    }
    let excel = backend.excel();
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let table_context = match build_table_context(backend, name, sheet.clone(), lang).await {
        Ok(ctx) => ctx,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let columns = match select_columns(&table_context, column_selectors) {
        Ok(columns) => columns,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let ql = query.to_lowercase();
    let result_limit = max_results.min(500);
    let mut results = Vec::with_capacity(result_limit);
    let mut scanned_rows = 0;
    let mut truncated = false;
    let display_idx = table_context.display_column_idx();

    'rows: for (sequence, (row_id, subrow_id)) in row_locations(&sheet)
        .skip(row_offset)
        .take(max_rows.unwrap_or(usize::MAX))
        .enumerate()
    {
        if sequence % 256 == 0 {
            tokio::task::yield_now().await;
        }
        scanned_rows += 1;
        let row = match get_row_at(&sheet, row_id, subrow_id) {
            Ok(row) => row,
            Err(_) => continue,
        };

        for column in &columns {
            if column.sheet.kind() != ironworks::file::exh::ColumnKind::String {
                continue;
            }
            if let Ok(s) = row.read_string(u32::from(column.sheet.offset())) {
                let value = s.to_string();
                if value.to_lowercase().contains(&ql) {
                    if results.len() >= result_limit {
                        truncated = true;
                        break 'rows;
                    }
                    let mut item = serde_json::Map::new();
                    item.insert("row_id".into(), serde_json::json!(row_id));
                    item.insert("subrow_id".into(), serde_json::json!(subrow_id));
                    item.insert("column_index".into(), serde_json::json!(column.index));
                    item.insert(
                        "column_offset".into(),
                        serde_json::json!(column.sheet.offset()),
                    );
                    item.insert(
                        "column_name".into(),
                        serde_json::json!(column.schema.name()),
                    );
                    item.insert("value".into(), serde_json::json!(value));
                    if display_idx == Some(column.index as u32) {
                        item.insert("is_display".into(), serde_json::json!(true));
                    }
                    results.push(serde_json::Value::Object(item));
                }
            }
        }
    }

    McpResponse::Success(
        serde_json::json!({
            "sheet": name,
            "query": query,
            "language": format!("{lang:?}"),
            "count": results.len(),
            "scanned_rows": scanned_rows,
            "row_offset": row_offset,
            "max_rows": max_rows,
            "limit": result_limit,
            "truncated": truncated,
            "matches": results
        })
        .to_string(),
    )
}

struct ResolveLinkOptions<'a> {
    name: &'a str,
    row_id: u32,
    subrow_id: u16,
    column: &'a ColumnSelector,
    target_columns: Option<&'a [ColumnSelector]>,
    format: RowFormat,
    language: Language,
}

async fn process_resolve_link(backend: &Backend, options: ResolveLinkOptions<'_>) -> McpResponse {
    let ResolveLinkOptions {
        name,
        row_id,
        subrow_id,
        column: column_selector,
        target_columns: target_column_selectors,
        format,
        language: lang,
    } = options;
    let excel = backend.excel();
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let table_context = match build_table_context(backend, name, sheet.clone(), lang).await {
        Ok(ctx) => ctx,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let row = match sheet.get_subrow(row_id, subrow_id) {
        Ok(r) => r,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let source_columns =
        match select_columns(&table_context, Some(std::slice::from_ref(column_selector))) {
            Ok(columns) => columns,
            Err(e) => return McpResponse::Error(format!("{e}")),
        };
    let source_column = &source_columns[0];
    let link_value = match table_context
        .cell_by_offset(row, source_column.index as u32)
        .and_then(|cell| cell.read(false))
        .and_then(|value| {
            value
                .coerce_integer()
                .ok_or_else(|| anyhow::anyhow!("链接列无法转换为行 ID"))
        }) {
        Ok(value) => match u32::try_from(value) {
            Ok(value) => value,
            Err(e) => return McpResponse::Error(format!("链接行 ID 无效: {e}")),
        },
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let targets = match source_column.schema.meta() {
        SchemaColumnMeta::Link(link) => link.targets().to_vec(),
        SchemaColumnMeta::ConditionalLink { column_idx, links } => {
            let switch_value = match table_context
                .cell_by_offset(row, *column_idx)
                .and_then(|cell| cell.read(false))
                .and_then(|value| {
                    value
                        .coerce_integer()
                        .and_then(|value| i32::try_from(value).ok())
                        .ok_or_else(|| anyhow::anyhow!("条件链接选择值无效"))
                }) {
                Ok(value) => value,
                Err(e) => return McpResponse::Error(format!("{e}")),
            };
            match links.get(&switch_value) {
                Some(link) => link.targets().to_vec(),
                None => {
                    return McpResponse::Error(format!(
                        "条件链接没有选择值 {switch_value} 对应的目标表"
                    ));
                }
            }
        }
        _ => return McpResponse::Error(format!("列 '{}' 不是链接列", source_column.schema.name())),
    };

    for target_name in &targets {
        let target_sheet = match excel.get_sheet(target_name, lang).await {
            Ok(sheet) => sheet,
            Err(_) => continue,
        };
        let target_row = match target_sheet.get_row(link_value) {
            Ok(row) => row,
            Err(_) => continue,
        };
        let target_context =
            match build_table_context(backend, target_name, target_sheet.clone(), lang).await {
                Ok(context) => context,
                Err(e) => return McpResponse::Error(format!("{e}")),
            };
        let target_columns = match select_columns(&target_context, target_column_selectors) {
            Ok(columns) => columns,
            Err(e) => return McpResponse::Error(format!("{e}")),
        };
        let values = match read_row_values(&target_context, target_row, &target_columns, format) {
            Ok(values) => values,
            Err(e) => return McpResponse::Error(format!("{e}")),
        };

        return McpResponse::Success(
            serde_json::json!({
                "source": {
                    "sheet": name,
                    "row_id": row_id,
                    "subrow_id": subrow_id,
                    "column_index": source_column.index,
                    "column_name": source_column.schema.name()
                },
                "link_row_id": link_value,
                "target": {
                    "sheet": target_name,
                    "row_id": link_value,
                    "format": match format { RowFormat::Compact => "compact", RowFormat::Detailed => "detailed" },
                    "columns": columns_to_json(&target_columns, target_context.display_column_idx()),
                    "values": values
                }
            })
            .to_string(),
        );
    }

    McpResponse::Error(format!(
        "目标行 {link_value} 不存在于候选表: {}",
        targets.join(", ")
    ))
}

async fn process_decode_se_string(
    backend: &Backend,
    name: &str,
    row_id: u32,
    subrow_id: u16,
    column_selector: &ColumnSelector,
    lang: Language,
) -> McpResponse {
    let excel = backend.excel();
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let table_context = match build_table_context(backend, name, sheet.clone(), lang).await {
        Ok(ctx) => ctx,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let row = match sheet.get_subrow(row_id, subrow_id) {
        Ok(r) => r,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let columns = match select_columns(&table_context, Some(std::slice::from_ref(column_selector)))
    {
        Ok(columns) => columns,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let column = &columns[0];
    if column.sheet.kind() != ironworks::file::exh::ColumnKind::String {
        return McpResponse::Error(format!(
            "列 {} 的类型为 {:?}, 不是 String, 无法解码 SeString",
            column.index,
            column.sheet.kind()
        ));
    }
    let value = match row.read_string(u32::from(column.sheet.offset())) {
        Ok(value) => value,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let raw_text = value.to_string();
    let raw_bytes = value.as_bytes();

    use base64::{Engine, prelude::BASE64_STANDARD};
    let base64 = BASE64_STANDARD.encode(raw_bytes);
    let hex = raw_bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");

    McpResponse::Success(
        serde_json::json!({
            "sheet": name,
            "row_id": row_id,
            "subrow_id": subrow_id,
            "column_offset": column.sheet.offset(),
            "column_index": column.index,
            "column_name": column.schema.name(),
            "raw_text": raw_text,
            "bytes_base64": base64,
            "bytes_hex": hex,
            "byte_count": raw_bytes.len()
        })
        .to_string(),
    )
}

async fn process_save_schema(backend: &Backend, name: &str, text: &str) -> McpResponse {
    let schema_provider = backend.schema();
    if !schema_provider.can_save_schemas() {
        return McpResponse::Error("当前模式提供者不支持保存".into());
    }
    match schema_provider.save_schema(name, text).await {
        Ok(()) => {
            invalidate_schema_snapshot(name);
            McpResponse::Success(serde_json::json!({"saved": true, "name": name}).to_string())
        }
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

async fn dispatch_request(backend: &Backend, req: McpRequest) -> McpResponse {
    match req {
        McpRequest::ReadAsset {
            path,
            offset,
            limit,
        } => match assets::read_path(backend, &path, offset, limit).await {
            Ok(result) => McpResponse::Success(result),
            Err(error) => McpResponse::Error(format!("{error}")),
        },
        McpRequest::ReadAssetByHash {
            repository,
            category,
            hash,
            split,
            offset,
            limit,
        } => match assets::read_hash(backend, repository, category, hash, split, offset, limit)
            .await
        {
            Ok(result) => McpResponse::Success(result),
            Err(error) => McpResponse::Error(format!("{error}")),
        },
        McpRequest::CheckAssetPaths { paths } => match assets::exists_many(backend, &paths).await {
            Ok(result) => McpResponse::Success(result),
            Err(error) => McpResponse::Error(format!("{error}")),
        },
        McpRequest::ListAssetPaths {
            api_base,
            query,
            include_missing,
            include_unnamed,
            offset,
            limit,
        } => {
            match assets::list_paths(
                backend,
                &api_base,
                query.as_deref(),
                include_missing,
                include_unnamed,
                offset,
                limit,
            )
            .await
            {
                Ok(result) => McpResponse::Success(result),
                Err(error) => McpResponse::Error(format!("{error}")),
            }
        }
        McpRequest::ResolveAssetPath { path } => match assets::resolve_path(backend, &path).await {
            Ok(result) => McpResponse::Success(result),
            Err(error) => McpResponse::Error(format!("{error}")),
        },
        McpRequest::InspectAsset { path, max_items } => {
            match assets::inspect_path(backend, &path, max_items).await {
                Ok(result) => McpResponse::Success(result),
                Err(error) => McpResponse::Error(format!("{error}")),
            }
        }
        McpRequest::InspectAssetByHash {
            repository,
            category,
            hash,
            split,
            max_items,
        } => {
            match assets::inspect_hash(backend, repository, category, hash, split, max_items).await
            {
                Ok(result) => McpResponse::Success(result),
                Err(error) => McpResponse::Error(format!("{error}")),
            }
        }
        McpRequest::DecodeTexture { path, max_dim } => {
            match assets::decode_texture(backend, &path, max_dim).await {
                Ok(result) => McpResponse::Success(result),
                Err(error) => McpResponse::Error(format!("{error}")),
            }
        }
        McpRequest::ListSheets {
            query,
            include_misc,
            offset,
            limit,
        } => McpResponse::Success(process_list_sheets(
            backend,
            query.as_deref(),
            include_misc,
            offset,
            limit,
        )),
        McpRequest::GetSheetInfo { name } => process_get_sheet_info(backend, &name).await,
        McpRequest::GetSheetSchema { name } => process_get_sheet_schema(backend, &name).await,
        McpRequest::GetSchemaRaw { name } => process_get_schema_raw(backend, &name).await,
        McpRequest::SearchCells {
            name,
            query,
            columns,
            row_offset,
            max_rows,
            max_results,
            language,
        } => {
            process_search_cells(
                backend,
                SearchCellsOptions {
                    name: &name,
                    query: &query,
                    columns: columns.as_deref(),
                    row_offset,
                    max_rows,
                    max_results,
                    language,
                },
            )
            .await
        }
        McpRequest::QueryRows {
            name,
            filter,
            columns,
            offset,
            limit,
            count_total,
            resolve_links,
            format,
            language,
        } => {
            process_query_rows(
                backend,
                QueryRowsOptions {
                    name: &name,
                    filter: filter.as_deref(),
                    columns: columns.as_deref(),
                    offset,
                    limit,
                    count_total,
                    resolve_links,
                    format,
                    language,
                },
            )
            .await
        }
        McpRequest::GetRow {
            name,
            row_id,
            subrow_id,
            columns,
            format,
            language,
        } => {
            process_get_row(
                backend,
                &name,
                row_id,
                subrow_id,
                columns.as_deref(),
                format,
                language,
            )
            .await
        }
        McpRequest::ValidateSchema { text } => McpResponse::Success(process_validate_schema(&text)),
        McpRequest::GetSheetRelations { name } => process_get_sheet_relations(backend, &name).await,
        McpRequest::GetReferencingSheets { target_sheet } => {
            process_get_referencing_sheets(backend, &target_sheet).await
        }
        McpRequest::ResolveLink {
            name,
            row_id,
            subrow_id,
            column,
            target_columns,
            format,
            language,
        } => {
            process_resolve_link(
                backend,
                ResolveLinkOptions {
                    name: &name,
                    row_id,
                    subrow_id,
                    column: &column,
                    target_columns: target_columns.as_deref(),
                    format,
                    language,
                },
            )
            .await
        }
        McpRequest::DecodeSeString {
            name,
            row_id,
            subrow_id,
            column,
            language,
        } => process_decode_se_string(backend, &name, row_id, subrow_id, &column, language).await,
        McpRequest::SaveSchema { name, text } => process_save_schema(backend, &name, &text).await,
    }
}

pub struct McpHandle {
    shutdown: tokio_util::sync::CancellationToken,
    server_join: Option<std::thread::JoinHandle<()>>,
    worker_join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for McpHandle {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(join) = self.server_join.take() {
            let _ = join.join();
        }
        if let Some(join) = self.worker_join.take() {
            let _ = join.join();
        }
    }
}

pub fn start(config: BackendConfig) -> McpHandle {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let (request_tx, mut request_rx) =
        mpsc::channel::<(McpRequest, oneshot::Sender<McpResponse>)>(64);

    let worker_shutdown = shutdown.clone();
    let worker_config = config.clone();
    let worker_join = std::thread::Builder::new()
        .name("mcp-worker".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime for MCP worker");

            let backend = match rt.block_on(Backend::new(worker_config)) {
                Ok(backend) => backend,
                Err(e) => {
                    log::error!("MCP backend 初始化失败: {e}");
                    return;
                }
            };

            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                loop {
                    tokio::select! {
                        () = worker_shutdown.cancelled() => break,
                        request = request_rx.recv() => {
                            let Some((req, resp_tx)) = request else {
                                break;
                            };
                            let backend = backend.clone();
                            tokio::task::spawn_local(async move {
                                let mut resp_tx = resp_tx;
                                tokio::select! {
                                    () = resp_tx.closed() => {}
                                    response = dispatch_request(&backend, req) => {
                                        let _ = resp_tx.send(response);
                                    }
                                }
                            });
                        }
                    }
                }
            });
        })
        .expect("Failed to spawn MCP worker thread");

    let server_shutdown = shutdown.clone();
    let server_join = std::thread::Builder::new()
        .name("mcp-server".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime for MCP server");

            rt.block_on(async move {
                let handler = std::sync::Arc::new(McpHandler::new(
                    request_tx,
                    config,
                    Language::ChineseSimplified,
                ));
                let ct = tokio_util::sync::CancellationToken::new();
                let mut session_manager = rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default();
                session_manager.session_config.keep_alive = None;

                let config =
                    rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default()
                        .with_cancellation_token(ct.child_token());

                let service =
                    rmcp::transport::streamable_http_server::StreamableHttpService::new(
                        {
                            let handler = handler.clone();
                            move || Ok(handler.as_ref().clone())
                        },
                        std::sync::Arc::new(session_manager),
                        config,
                    );

                let router = axum::Router::new().nest_service("/mcp", service);

                let listener = match tokio::net::TcpListener::bind("127.0.0.1:3001").await {
                    Ok(listener) => listener,
                    Err(e) => {
                        log::error!("MCP 服务器无法监听 127.0.0.1:3001: {e}");
                        return;
                    }
                };
                log::info!("MCP 服务器已启动 http://127.0.0.1:3001/mcp");

                let shutdown_ct = ct.clone();
                let server =
                    axum::serve(listener, router).with_graceful_shutdown(async move {
                        server_shutdown.cancelled().await;
                        shutdown_ct.cancel();
                    });

                if let Err(e) = server.await {
                    log::error!("MCP 服务器错误: {e}");
                }
            });
        })
        .expect("Failed to spawn MCP server thread");

    McpHandle {
        shutdown,
        server_join: Some(server_join),
        worker_join: Some(worker_join),
    }
}
