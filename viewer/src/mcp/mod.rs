use std::{
    cell::RefCell,
    num::{NonZeroU32, NonZeroUsize},
    str::FromStr,
    sync::mpsc,
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
        CellValue, ComplexFilter, FilterInput, GlobalContext, MatchOptions, SchemaColumnMeta,
        TableContext,
    },
    utils::IconManager,
};
use handler::McpHandler;
use ironworks::excel::Language;
use lru::LruCache;
use tokio::sync::oneshot;

mod handler;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub enum McpRequest {
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
    SearchSheets {
        query: String,
    },
    SearchCells {
        name: String,
        query: String,
        max_results: usize,
    },
    QueryRows {
        name: String,
        filter: Option<String>,
        offset: usize,
        limit: usize,
    },
    GetRow {
        name: String,
        row_id: u32,
        subrow_id: u16,
        search_name: Option<String>,
    },
    ValidateFilter {
        expression: String,
    },
    ValidateSchema {
        text: String,
    },
    GetIconUrl {
        icon_id: u32,
    },
    DecomposeModelId {
        model_id: String,
    },
    GetSheetRelations {
        name: String,
    },
    GetReferencingSheets {
        target_sheet: String,
    },
    FollowLink {
        name: String,
        row_id: u32,
        column_index: usize,
    },
    DecodeSeString {
        name: String,
        row_id: u32,
        subrow_id: u16,
        column_index: usize,
    },
    ResolveDisplayField {
        name: String,
        row_id: u32,
        subrow_id: u16,
    },
    SaveSchema {
        name: String,
        text: String,
    },
    GetGameVersion,
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
    fn display_field_index(&self) -> Option<usize> {
        let schema = self.parsed_schema()?;
        schema.display_field.as_ref().and_then(|display_field| {
            schema
                .fields
                .iter()
                .position(|field| field.name.as_deref() == Some(display_field.as_str()))
        })
    }

    fn column_names(&self) -> Vec<(String, String)> {
        self.parsed_schema()
            .map(|schema| {
                schema
                    .fields
                    .iter()
                    .map(|field| {
                        (
                            field.name.clone().unwrap_or_else(|| "?".into()),
                            format!("{:?}", field.r#type),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

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
}

struct SchemaContext {
    column_names: Vec<(String, String)>,
    display_field: Option<String>,
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

async fn build_schema_context(backend: &Backend, name: &str) -> SchemaContext {
    match load_schema_snapshot(backend, name).await {
        Ok(snapshot) => match snapshot.parsed_schema() {
            Some(schema) => {
                let display_field = schema.display_field.clone();
                let column_names: Vec<_> = schema
                    .fields
                    .iter()
                    .map(|f| {
                        let name = f.name.clone().unwrap_or_else(|| "?".into());
                        let type_str = format!("{:?}", f.r#type);
                        (name, type_str)
                    })
                    .collect();
                SchemaContext {
                    column_names,
                    display_field,
                }
            }
            None => SchemaContext {
                column_names: Vec::new(),
                display_field: None,
            },
        },
        Err(_) => SchemaContext {
            column_names: Vec::new(),
            display_field: None,
        },
    }
}

async fn build_table_context(
    backend: &Backend,
    name: &str,
    sheet: BaseSheet,
    lang: Language,
) -> anyhow::Result<TableContext> {
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
    Ok(TableContext::new(global, sheet, schema.as_ref()))
}

fn filter_match_options() -> MatchOptions {
    MatchOptions {
        case_insensitive: true,
        use_display_field: true,
    }
}

fn row_locations(sheet: &impl ExcelSheet) -> Vec<(u32, Option<u16>)> {
    if sheet.has_subrows() {
        sheet
            .get_subrow_ids()
            .map(|(row_id, subrow_id)| (row_id, Some(subrow_id)))
            .collect()
    } else {
        sheet.get_row_ids().map(|row_id| (row_id, None)).collect()
    }
}

fn build_row_fields(
    table: &TableContext,
    row: &crate::excel::provider::ExcelRow<'_>,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let columns = table
        .columns()?
        .into_iter()
        .map(|(schema_column, _sheet_column)| {
            (
                schema_column.name().to_string(),
                schema_column_type_name(schema_column.meta()).to_string(),
            )
        })
        .collect();
    build_row_fields_from_columns(table.display_column_idx(), columns, |column_idx| {
        let cell = table.cell_by_offset(*row, column_idx as u32)?;
        Ok(cell.read(true)?)
    })
}

fn build_row_fields_from_columns(
    display_idx: Option<u32>,
    columns: Vec<(String, String)>,
    mut read_cell: impl FnMut(usize) -> anyhow::Result<CellValue>,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let mut fields = serde_json::Map::new();
    for (i, (name, type_name)) in columns.into_iter().enumerate() {
        let cell_value = read_cell(i)?;
        let value = cell_value_to_json(&cell_value);

        let mut field = serde_json::Map::new();
        field.insert("name".into(), serde_json::json!(name));
        field.insert("type".into(), serde_json::json!(type_name));
        field.insert(
            "kind".into(),
            serde_json::json!(value["kind"].as_str().unwrap_or("Unknown")),
        );
        field.insert("value".into(), value);
        field.insert(
            "display".into(),
            serde_json::json!(cell_value.display_text().to_string()),
        );
        if display_idx == Some(i as u32) {
            field.insert("is_display".into(), serde_json::json!(true));
        }
        fields.insert(format!("f_{i}"), serde_json::Value::Object(field));
    }
    Ok(fields)
}

fn build_row_object(
    table: &TableContext,
    row: &crate::excel::provider::ExcelRow<'_>,
    row_id: u32,
    subrow_id: Option<u16>,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let mut row_obj = serde_json::Map::new();
    row_obj.insert("row_id".into(), serde_json::json!(row_id));
    row_obj.insert("subrow_id".into(), serde_json::json!(subrow_id));
    row_obj.extend(build_row_fields(table, row)?);
    Ok(row_obj)
}

fn process_list_sheets(
    backend: &Backend,
    query: Option<&str>,
    include_misc: bool,
    offset: usize,
    limit: usize,
) -> String {
    let excel = backend.excel();
    let entries = excel.get_entries();
    let mut sheets: Vec<(&String, &i32)> = entries
        .iter()
        .filter(|(_, id)| include_misc || **id >= 0)
        .collect();
    sheets.sort_by_key(|(name, _)| name.to_lowercase());

    if let Some(q) = query {
        let ql = q.to_lowercase();
        sheets.retain(|(name, _)| name.to_lowercase().contains(&ql));
    }
    let total = sheets.len();
    let page: Vec<serde_json::Value> = sheets
        .iter()
        .skip(offset)
        .take(limit.min(500))
        .map(|(name, id)| serde_json::json!({"name": name, "id": id}))
        .collect();
    serde_json::json!({"total": total, "offset": offset, "limit": limit, "sheets": page})
        .to_string()
}

fn process_search_sheets(backend: &Backend, query: &str) -> String {
    let excel = backend.excel();
    let entries = excel.get_entries();
    let ql = query.to_lowercase();
    let mut matches: Vec<serde_json::Value> = entries
        .iter()
        .filter(|(name, _)| name.to_lowercase().contains(&ql))
        .map(|(name, id)| serde_json::json!({"name": name, "id": id}))
        .collect();
    matches.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });
    matches.truncate(100);
    serde_json::json!({"query": query, "count": matches.len(), "matches": matches}).to_string()
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

fn process_get_icon_url(icon_id: u32) -> String {
    let path = crate::data::get_icon_path(icon_id, false);
    let hires_path = crate::data::get_icon_path(icon_id, true);
    serde_json::json!({"icon_id": icon_id, "tex_path": path, "hires_tex_path": hires_path})
        .to_string()
}

fn process_decompose_model_id(model_id: &str) -> McpResponse {
    match model_id.parse::<u64>() {
        Ok(value) => McpResponse::Success(
            serde_json::json!({
                "model": (value & 0x00FF_FFFF) as u32,
                "variant": ((value >> 24) & 0x00FF) as u8,
                "stain": ((value >> 32) & 0x00FF) as u8
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
                let fields: Vec<serde_json::Value> = schema
                    .fields
                    .iter()
                    .map(|f| {
                        serde_json::json!({
                            "name": f.name,
                            "type": format!("{:?}", f.r#type),
                            "count": f.count,
                            "comment": f.comment,
                            "relations": f.relations
                        })
                    })
                    .collect();
                McpResponse::Success(
                    serde_json::json!({
                        "name": schema.name,
                        "display_field": schema.display_field,
                        "fields": fields,
                        "relations": schema.relations,
                        "field_count": schema.fields.len()
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

async fn process_get_sheet_relations(backend: &Backend, name: &str) -> McpResponse {
    match load_schema_snapshot(backend, name).await {
        Ok(snapshot) => match snapshot.parsed_schema() {
            Some(schema) => {
                let field_rels: Vec<serde_json::Value> = schema
                    .fields
                    .iter()
                    .filter(|f| f.relations.is_some())
                    .map(|f| serde_json::json!({"field": f.name, "relations": f.relations}))
                    .collect();
                McpResponse::Success(
                    serde_json::json!({"name": name, "relations": field_rels}).to_string(),
                )
            }
            None => McpResponse::Error(format!(
                "无法解析模式: {}",
                snapshot.validation_errors().join("; ")
            )),
        },
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

async fn process_get_referencing_sheets(backend: &Backend, target_sheet: &str) -> McpResponse {
    let excel = backend.excel();
    let entries = excel.get_entries();
    let mut references: Vec<serde_json::Value> = Vec::new();

    for (sheet_name, _) in entries.iter() {
        if sheet_name == target_sheet {
            continue;
        }
        if let Ok(snapshot) = load_schema_snapshot(backend, sheet_name).await {
            if let Some(schema) = snapshot.parsed_schema() {
                let mut ref_info: Vec<serde_json::Value> = Vec::new();
                for field in &schema.fields {
                    if let Some(ref field_rels) = field.relations {
                        if field_rels.contains_key(target_sheet) {
                            ref_info
                                .push(serde_json::json!({"field": field.name, "type": "direct"}));
                        }
                    }
                }
                if let Some(ref sheet_rels) = schema.relations {
                    if sheet_rels.contains_key(target_sheet) {
                        ref_info.push(serde_json::json!({"type": "sheet-level"}));
                    }
                }
                if !ref_info.is_empty() {
                    references
                        .push(serde_json::json!({"sheet": sheet_name, "references": ref_info}));
                }
            }
        }
    }

    McpResponse::Success(
        serde_json::json!({
            "target_sheet": target_sheet,
            "count": references.len(),
            "referencing_sheets": references
        })
        .to_string(),
    )
}

async fn process_query_rows(
    backend: &Backend,
    name: &str,
    filter: Option<&str>,
    offset: usize,
    limit: usize,
    lang: Language,
) -> McpResponse {
    let excel = backend.excel();
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let ctx = build_schema_context(backend, name).await;
    let table_context = match build_table_context(backend, name, sheet.clone(), lang).await {
        Ok(ctx) => ctx,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let row_locations = row_locations(&sheet);
    let total_rows = row_locations.len();
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
            match table_context.compile_filter(&parsed, filter_match_options()) {
                Ok(filter) => Some(filter),
                Err(e) => return McpResponse::Error(format!("{e}")),
            }
        }
        _ => None,
    };

    let mut rows: Vec<serde_json::Value> = Vec::new();
    let matched_rows;

    if let Some(compiled_filter) = compiled_filter.as_ref() {
        let fuzzy = compiled_filter.input().is_some_and(|input| input.has_fuzzy);
        let mut matched: Vec<(
            usize,
            Option<NonZeroU32>,
            serde_json::Map<String, serde_json::Value>,
        )> = Vec::new();

        for (sequence, (row_id, subrow_id)) in row_locations.into_iter().enumerate() {
            let row = match subrow_id {
                Some(subrow_id) => match sheet.get_subrow(row_id, subrow_id) {
                    Ok(row) => row,
                    Err(_) => continue,
                },
                None => match sheet.get_row(row_id) {
                    Ok(row) => row,
                    Err(_) => continue,
                },
            };

            let (is_match, _in_progress) =
                match table_context.filter_row(row_id, subrow_id, &row, compiled_filter) {
                    Ok(result) => result,
                    Err(e) => return McpResponse::Error(format!("{e}")),
                };
            if !is_match {
                continue;
            }

            let score = if fuzzy {
                match table_context.score_row(row_id, subrow_id, &row, compiled_filter) {
                    Ok((score, _)) => score,
                    Err(e) => return McpResponse::Error(format!("{e}")),
                }
            } else {
                None
            };

            let mut row_obj = match build_row_object(&table_context, &row, row_id, subrow_id) {
                Ok(row_obj) => row_obj,
                Err(e) => return McpResponse::Error(format!("{e}")),
            };
            row_obj.insert("row_index".into(), serde_json::json!(sequence));
            if let Some(score) = score {
                row_obj.insert("match_score".into(), serde_json::json!(score.get()));
            }
            matched.push((sequence, score, row_obj));
        }

        matched_rows = matched.len();
        if fuzzy {
            matched.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }

        rows.extend(
            matched
                .into_iter()
                .skip(offset)
                .take(page_limit)
                .map(|(_, _, row)| serde_json::Value::Object(row)),
        );
    } else {
        matched_rows = total_rows;
        for (sequence, (row_id, subrow_id)) in row_locations.into_iter().enumerate() {
            if sequence < offset {
                continue;
            }
            if rows.len() >= page_limit {
                break;
            }

            let row = match subrow_id {
                Some(subrow_id) => match sheet.get_subrow(row_id, subrow_id) {
                    Ok(row) => row,
                    Err(_) => continue,
                },
                None => match sheet.get_row(row_id) {
                    Ok(row) => row,
                    Err(_) => continue,
                },
            };

            let mut row_obj = match build_row_object(&table_context, &row, row_id, subrow_id) {
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
            "display_field": ctx.display_field,
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
    search_name: Option<&str>,
    lang: Language,
) -> McpResponse {
    let excel = backend.excel();
    let ctx = build_schema_context(backend, name).await;
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let table_context = match build_table_context(backend, name, sheet.clone(), lang).await {
        Ok(ctx) => ctx,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let (target_row_id, target_subrow_id) = if let Some(search) = search_name {
        let search_lower = search.to_lowercase();
        let display_idx = table_context.display_column_idx().unwrap_or(0);
        let (_, col_def) = match table_context.get_column_by_offset(display_idx) {
            Ok(column) => column,
            Err(e) => return McpResponse::Error(format!("{e}")),
        };
        let mut found = None;
        for (rid, sid) in row_locations(&sheet) {
            let row = match sid {
                Some(subrow_id) => match sheet.get_subrow(rid, subrow_id) {
                    Ok(row) => row,
                    Err(_) => continue,
                },
                None => match sheet.get_row(rid) {
                    Ok(row) => row,
                    Err(_) => continue,
                },
            };
            if col_def.kind() == ironworks::file::exh::ColumnKind::String {
                if let Ok(s) = row.read_string(u32::from(col_def.offset())) {
                    if s.to_string().to_lowercase().contains(&search_lower) {
                        found = Some((rid, sid.unwrap_or(0)));
                        break;
                    }
                }
            } else if let Ok(v) = row.read::<u32>(u32::from(col_def.offset())) {
                if v.to_string().to_lowercase().contains(&search_lower) {
                    found = Some((rid, sid.unwrap_or(0)));
                    break;
                }
            }
        }
        match found {
            Some(pair) => pair,
            None => return McpResponse::Error(format!("未找到匹配 '{search}' 的行")),
        }
    } else {
        (row_id, subrow_id)
    };

    let row = match sheet.get_subrow(target_row_id, target_subrow_id) {
        Ok(r) => r,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let fields = match build_row_fields(&table_context, &row) {
        Ok(fields) => fields,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    McpResponse::Success(
        serde_json::json!({
            "sheet": name,
            "row_id": target_row_id,
            "subrow_id": target_subrow_id,
            "display_field": ctx.display_field,
            "field_count": table_context.column_count(),
            "fields": fields
        })
        .to_string(),
    )
}

async fn process_search_cells(
    backend: &Backend,
    name: &str,
    query: &str,
    max_results: usize,
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

    let column_defs = match table_context.columns() {
        Ok(columns) => columns,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let ql = query.to_lowercase();
    let mut results: Vec<serde_json::Value> = Vec::new();
    let row_locations = row_locations(&sheet);
    let scanned_rows = row_locations.len();
    let display_idx = table_context.display_column_idx();

    for (row_id, subrow_id) in row_locations {
        if results.len() >= max_results {
            break;
        }
        let row = match subrow_id {
            Some(subrow_id) => match sheet.get_subrow(row_id, subrow_id) {
                Ok(row) => row,
                Err(_) => continue,
            },
            None => match sheet.get_row(row_id) {
                Ok(row) => row,
                Err(_) => continue,
            },
        };

        for (i, (_, col_def)) in column_defs.iter().enumerate() {
            if results.len() >= max_results {
                break;
            }
            if col_def.kind() != ironworks::file::exh::ColumnKind::String {
                continue;
            }
            if let Ok(s) = row.read_string(u32::from(col_def.offset())) {
                let value = s.to_string();
                if value.to_lowercase().contains(&ql) {
                    let col_name = column_defs[i].0.name().to_string();
                    let mut item = serde_json::Map::new();
                    item.insert("row_id".into(), serde_json::json!(row_id));
                    item.insert("subrow_id".into(), serde_json::json!(subrow_id));
                    item.insert("column_index".into(), serde_json::json!(i));
                    item.insert("column_offset".into(), serde_json::json!(col_def.offset()));
                    item.insert("column_name".into(), serde_json::json!(col_name));
                    item.insert("value".into(), serde_json::json!(value));
                    if display_idx == Some(i as u32) {
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
            "truncated": results.len() >= max_results,
            "matches": results
        })
        .to_string(),
    )
}

async fn process_follow_link(
    backend: &Backend,
    name: &str,
    row_id: u32,
    column_index: usize,
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
    let row = match sheet.get_row(row_id) {
        Ok(r) => r,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let idx = column_index.min(table_context.column_count().saturating_sub(1));
    let ((_, col_def), _) = match table_context.get_column_by_index(idx as u32) {
        Ok(column) => column,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let link_value = match row.read::<u32>(u32::from(col_def.offset())) {
        Ok(v) => v,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    McpResponse::Success(
        serde_json::json!({
            "sheet": name,
            "row_id": row_id,
            "column_offset": col_def.offset(),
            "column_index": idx,
            "link_value": link_value
        })
        .to_string(),
    )
}

async fn process_decode_se_string(
    backend: &Backend,
    name: &str,
    row_id: u32,
    subrow_id: u16,
    column_index: usize,
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

    let idx = column_index.min(table_context.column_count().saturating_sub(1));
    let ((_, col_def), _) = match table_context.get_column_by_index(idx as u32) {
        Ok(column) => column,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    if col_def.kind() != ironworks::file::exh::ColumnKind::String {
        return McpResponse::Error(format!(
            "列 {} 的类型为 {:?}，不是 String，无法解码 SeString",
            idx,
            col_def.kind()
        ));
    }
    let raw = match row.read_string(u32::from(col_def.offset())) {
        Ok(s) => s.to_string(),
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let raw_bytes = raw.as_bytes().to_vec();

    use base64::{Engine, prelude::BASE64_STANDARD};
    let base64 = BASE64_STANDARD.encode(&raw_bytes);
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
            "column_offset": col_def.offset(),
            "column_index": idx,
            "raw_text": raw,
            "bytes_base64": base64,
            "bytes_hex": hex,
            "byte_count": raw_bytes.len()
        })
        .to_string(),
    )
}

async fn process_resolve_display_field(
    backend: &Backend,
    name: &str,
    row_id: u32,
    subrow_id: u16,
    lang: Language,
) -> McpResponse {
    let excel = backend.excel();
    let ctx = build_schema_context(backend, name).await;
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let table_context = match build_table_context(backend, name, sheet.clone(), lang).await {
        Ok(ctx) => ctx,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let Some(display_idx) = table_context.display_column_idx() else {
        return McpResponse::Success(
            serde_json::json!({
                "sheet": name,
                "row_id": row_id,
                "subrow_id": subrow_id,
                "resolved": null,
                "reason": "no display field"
            })
            .to_string(),
        );
    };

    let (_, col_def) = match table_context.get_column_by_offset(display_idx) {
        Ok(column) => column,
        Err(_) => {
            return McpResponse::Success(
                serde_json::json!({
                    "sheet": name,
                    "row_id": row_id,
                    "subrow_id": subrow_id,
                    "resolved": null,
                    "reason": "display field index out of bounds"
                })
                .to_string(),
            );
        }
    };

    let row = match sheet.get_subrow(row_id, subrow_id) {
        Ok(r) => r,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let value = if col_def.kind() == ironworks::file::exh::ColumnKind::String {
        row.read_string(u32::from(col_def.offset()))
            .map_or(serde_json::Value::Null, |s| {
                serde_json::json!(s.to_string())
            })
    } else {
        row.read::<u32>(u32::from(col_def.offset()))
            .map_or(serde_json::Value::Null, |v| serde_json::json!(v))
    };

    McpResponse::Success(
        serde_json::json!({
            "sheet": name,
            "row_id": row_id,
            "subrow_id": subrow_id,
            "column_index": display_idx,
            "column_offset": col_def.offset(),
            "display_field": ctx.display_field,
            "resolved": value
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

async fn dispatch_request(backend: &Backend, req: McpRequest, lang: Language) -> McpResponse {
    match req {
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
        McpRequest::SearchSheets { query } => {
            McpResponse::Success(process_search_sheets(backend, &query))
        }
        McpRequest::SearchCells {
            name,
            query,
            max_results,
        } => process_search_cells(backend, &name, &query, max_results, lang).await,
        McpRequest::QueryRows {
            name,
            filter,
            offset,
            limit,
        } => process_query_rows(backend, &name, filter.as_deref(), offset, limit, lang).await,
        McpRequest::GetRow {
            name,
            row_id,
            subrow_id,
            search_name,
        } => {
            process_get_row(
                backend,
                &name,
                row_id,
                subrow_id,
                search_name.as_deref(),
                lang,
            )
            .await
        }
        McpRequest::ValidateFilter { expression } => {
            McpResponse::Success(process_validate_filter(&expression))
        }
        McpRequest::ValidateSchema { text } => McpResponse::Success(process_validate_schema(&text)),
        McpRequest::GetIconUrl { icon_id } => McpResponse::Success(process_get_icon_url(icon_id)),
        McpRequest::DecomposeModelId { model_id } => process_decompose_model_id(&model_id),
        McpRequest::GetSheetRelations { name } => process_get_sheet_relations(backend, &name).await,
        McpRequest::GetReferencingSheets { target_sheet } => {
            process_get_referencing_sheets(backend, &target_sheet).await
        }
        McpRequest::FollowLink {
            name,
            row_id,
            column_index,
        } => process_follow_link(backend, &name, row_id, column_index, lang).await,
        McpRequest::DecodeSeString {
            name,
            row_id,
            subrow_id,
            column_index,
        } => process_decode_se_string(backend, &name, row_id, subrow_id, column_index, lang).await,
        McpRequest::ResolveDisplayField {
            name,
            row_id,
            subrow_id,
        } => process_resolve_display_field(backend, &name, row_id, subrow_id, lang).await,
        McpRequest::SaveSchema { name, text } => process_save_schema(backend, &name, &text).await,
        McpRequest::GetGameVersion => {
            McpResponse::Success(serde_json::json!({"source": "EXDViewer"}).to_string())
        }
    }
}

pub struct McpHandle {
    shutdown: tokio_util::sync::CancellationToken,
    server_join: std::thread::JoinHandle<()>,
    worker_join: std::thread::JoinHandle<()>,
}

impl Drop for McpHandle {
    fn drop(&mut self) {
        self.shutdown.cancel();
        let _ = std::mem::replace(&mut self.server_join, std::thread::spawn(|| {})).join();
        let _ = std::mem::replace(&mut self.worker_join, std::thread::spawn(|| {})).join();
    }
}

pub fn start(config: BackendConfig) -> McpHandle {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let (request_tx, request_rx) = mpsc::channel::<(McpRequest, oneshot::Sender<McpResponse>)>();

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

            while !worker_shutdown.is_cancelled() {
                let Ok((req, resp_tx)) = request_rx.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };

                let response = rt.block_on(async {
                    dispatch_request(&backend, req, Language::ChineseSimplified).await
                });
                let _ = resp_tx.send(response);
            }
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

                let listener = tokio::net::TcpListener::bind("127.0.0.1:3001")
                    .await
                    .expect("Failed to bind MCP server to 127.0.0.1:3001");
                log::info!("MCP 服务器已启动 http://127.0.0.1:3001/mcp");

                let shutdown_ct = ct.clone();
                let server =
                    axum::serve(listener, router).with_graceful_shutdown(async move {
                        while !server_shutdown.is_cancelled() {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
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
        server_join,
        worker_join,
    }
}
