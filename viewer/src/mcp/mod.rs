use std::sync::{Arc, mpsc};

use crate::{
    backend::Backend,
    excel::provider::{ExcelHeader, ExcelProvider, ExcelSheet},
    schema::provider::SchemaProvider,
    settings::BackendConfig,
};
use crate::schema::Schema as ExdSchema;
use ironworks::excel::Language;
use jsonschema::output::{ErrorDescription, OutputUnit};
use handler::McpHandler;

mod handler;

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

type McpChannel = mpsc::SyncSender<(McpRequest, mpsc::SyncSender<McpResponse>)>;

struct AsyncJob {
    promise: poll_promise::Promise<McpResponse>,
    response_tx: mpsc::SyncSender<McpResponse>,
}

pub struct McpBridge {
    pub sender: McpChannel,
}

pub struct McpBridgeState {
    receiver: mpsc::Receiver<(McpRequest, mpsc::SyncSender<McpResponse>)>,
    async_jobs: Vec<AsyncJob>,
    pub language: Language,
}

impl McpBridgeState {
    pub fn new(receiver: mpsc::Receiver<(McpRequest, mpsc::SyncSender<McpResponse>)>) -> Self {
        Self {
            receiver,
            async_jobs: Vec::new(),
            language: Language::ChineseSimplified,
        }
    }

    pub fn tick(&mut self, backend: &Backend) {
        let lang = self.language;
        while let Ok((req, resp_tx)) = self.receiver.try_recv() {
            log::info!("MCP 主线程收到请求: {req:?}");
            let result = dispatch(backend.clone(), req.clone(), lang);
            match result {
                Some(response) => {
                    log::info!("MCP 同步响应已发送");
                    let _ = resp_tx.send(response);
                }
                None => {
                    log::info!("MCP 异步请求已排队");
                    self.async_jobs.push(AsyncJob {
                        promise: poll_promise::Promise::spawn_local(dispatch_async(
                            backend.clone(),
                            req.clone(),
                            lang,
                        )),
                        response_tx: resp_tx,
                    });
                }
            }
        }

        self.async_jobs.retain_mut(|job| {
            if job.promise.ready().is_none() {
                return true;
            }
            let response = std::mem::replace(
                &mut job.promise,
                poll_promise::Promise::spawn_local(futures_util::future::ready(McpResponse::Error(
                    "stale".into(),
                ))),
            )
            .block_and_take();
            let _ = job.response_tx.send(response);
            false
        });
    }
}

fn dispatch(backend: Backend, req: McpRequest, _lang: Language) -> Option<McpResponse> {
    match &req {
        McpRequest::ListSheets {
            query,
            include_misc,
            offset,
            limit,
        } => Some(McpResponse::Success(
            process_list_sheets(&backend, query.as_deref(), *include_misc, *offset, *limit),
        )),
        McpRequest::SearchSheets { query } => {
            Some(McpResponse::Success(process_search_sheets(&backend, query)))
        }
        McpRequest::ValidateFilter { expression } => {
            Some(McpResponse::Success(process_validate_filter(expression)))
        }
        McpRequest::ValidateSchema { text } => {
            Some(McpResponse::Success(process_validate_schema(text)))
        }
        McpRequest::GetIconUrl { icon_id } => {
            Some(McpResponse::Success(process_get_icon_url(*icon_id)))
        }
        McpRequest::DecomposeModelId { model_id } => {
            Some(process_decompose_model_id(model_id))
        }
        McpRequest::GetGameVersion => {
            Some(McpResponse::Success(serde_json::json!({"source": "EXDViewer"}).to_string()))
        }
        _ => None,
    }
}

async fn dispatch_async(backend: Backend, req: McpRequest, lang: Language) -> McpResponse {
    match req {
        McpRequest::GetSheetInfo { name } => process_get_sheet_info(&backend, &name).await,
        McpRequest::GetSheetSchema { name } => process_get_sheet_schema(&backend, &name).await,
        McpRequest::GetSchemaRaw { name } => process_get_schema_raw(&backend, &name).await,
        McpRequest::GetSheetRelations { name } => {
            process_get_sheet_relations(&backend, &name).await
        }
        McpRequest::GetReferencingSheets { target_sheet } => {
            process_get_referencing_sheets(&backend, &target_sheet).await
        }
        McpRequest::QueryRows {
            name,
            filter,
            offset,
            limit,
        } => process_query_rows(&backend, &name, filter.as_deref(), offset, limit, lang).await,
        McpRequest::GetRow {
            name,
            row_id,
            subrow_id,
            search_name,
        } => process_get_row(&backend, &name, row_id, subrow_id, search_name.as_deref(), lang).await,
        McpRequest::SearchCells {
            name,
            query,
            max_results,
        } => process_search_cells(&backend, &name, &query, max_results, lang).await,
        McpRequest::FollowLink {
            name,
            row_id,
            column_index,
        } => process_follow_link(&backend, &name, row_id, column_index, lang).await,
        McpRequest::DecodeSeString {
            name,
            row_id,
            subrow_id,
            column_index,
        } => process_decode_se_string(&backend, &name, row_id, subrow_id, column_index, lang).await,
        McpRequest::SaveSchema { name, text } => process_save_schema(&backend, &name, &text).await,
        _ => McpResponse::Error("unknown request".into()),
    }
}

struct SchemaContext {
    column_names: Vec<(String, String)>,
    display_field: Option<String>,
}

async fn build_schema_context(backend: &Backend, name: &str) -> SchemaContext {
    let schema_provider = backend.schema();
    let yaml_text = schema_provider.get_schema_text(name).await;
    match yaml_text {
        Ok(text) => match ExdSchema::from_str(&text) {
            Ok(Ok(schema)) => {
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
            _ => SchemaContext {
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
    use std::str::FromStr;
    use crate::sheet::ComplexFilter;
    match ComplexFilter::from_str(expression) {
        Ok(f) => serde_json::json!({"valid": true, "expression": expression, "ast": format!("{f:?}")})
            .to_string(),
        Err(e) => serde_json::json!({"valid": false, "expression": expression, "error": e})
            .to_string(),
    }
}

fn process_validate_schema(text: &str) -> String {
    use crate::schema::Schema;
    match Schema::from_str(text) {
        Ok(Ok(s)) => serde_json::json!({"valid": true, "name": s.name, "field_count": s.fields.len()})
            .to_string(),
        Ok(Err(e)) => serde_json::json!({"valid": false, "errors": e.iter().map(|e: &OutputUnit<ErrorDescription>| format!("{} at {}", e.error_description(), e.instance_location())).collect::<Vec<_>>()})
            .to_string(),
        Err(e) => serde_json::json!({"valid": false, "error": format!("{e}")}).to_string(),
    }
}

fn process_get_icon_url(icon_id: u32) -> String {
    let path = crate::excel::get_icon_path(icon_id, false);
    let hires_path = crate::excel::get_icon_path(icon_id, true);
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
    let schema_provider = backend.schema();
    match schema_provider.get_schema_text(name).await {
        Ok(yaml_text) => {
            use crate::schema::Schema;
            match Schema::from_str(&yaml_text) {
                Ok(Ok(s)) => {
                    let fields: Vec<serde_json::Value> = s
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
                            "name": s.name,
                            "display_field": s.display_field,
                            "fields": fields,
                            "relations": s.relations,
                            "field_count": s.fields.len()
                        })
                        .to_string(),
                    )
                }
                Ok(Err(e)) => McpResponse::Success(
                    serde_json::json!({
                        "raw_yaml": yaml_text,
                        "valid": false,
                        "errors": e.iter().map(|e: &jsonschema::output::OutputUnit<jsonschema::output::ErrorDescription>| format!("{}", e.error_description())).collect::<Vec<_>>()
                    })
                    .to_string(),
                ),
                Err(e) => McpResponse::Error(format!("{e}")),
            }
        }
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

async fn process_get_schema_raw(backend: &Backend, name: &str) -> McpResponse {
    let schema_provider = backend.schema();
    match schema_provider.get_schema_text(name).await {
        Ok(text) => {
            McpResponse::Success(serde_json::json!({"name": name, "yaml": text}).to_string())
        }
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

async fn process_get_sheet_relations(backend: &Backend, name: &str) -> McpResponse {
    let schema_provider = backend.schema();
    match schema_provider.get_schema_text(name).await {
        Ok(yaml_text) => {
            use crate::schema::Schema;
            match Schema::from_str(&yaml_text) {
                Ok(Ok(s)) => {
                    let field_rels: Vec<serde_json::Value> = s
                        .fields
                        .iter()
                        .filter(|f| f.relations.is_some())
                        .map(|f| {
                            serde_json::json!({"field": f.name, "relations": f.relations})
                        })
                        .collect();
                    McpResponse::Success(
                        serde_json::json!({"name": name, "relations": field_rels}).to_string(),
                    )
                }
                _ => McpResponse::Error("无法解析模式".into()),
            }
        }
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

async fn process_get_referencing_sheets(backend: &Backend, target_sheet: &str) -> McpResponse {
    let excel = backend.excel();
    let schema_provider = backend.schema();
    let entries = excel.get_entries();
    let mut references: Vec<serde_json::Value> = Vec::new();

    for (sheet_name, _) in entries.iter() {
        if sheet_name == target_sheet {
            continue;
        }
        if let Ok(yaml_text) = schema_provider.get_schema_text(sheet_name).await {
            if let Ok(Ok(schema)) =
                crate::schema::Schema::from_str(&yaml_text)
            {
                let mut ref_info: Vec<serde_json::Value> = Vec::new();
                for field in &schema.fields {
                    if let Some(ref field_rels) = field.relations {
                        if field_rels.contains_key(target_sheet) {
                            ref_info.push(serde_json::json!({"field": field.name, "type": "direct"}));
                        }
                    }
                }
                if let Some(ref sheet_rels) = schema.relations {
                    if sheet_rels.contains_key(target_sheet) {
                        ref_info.push(serde_json::json!({"type": "sheet-level"}));
                    }
                }
                if !ref_info.is_empty() {
                    references.push(serde_json::json!({"sheet": sheet_name, "references": ref_info}));
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
    _filter: Option<&str>,
    offset: usize,
    limit: usize,
    lang: Language,
) -> McpResponse {
    let excel = backend.excel();
    let ctx = build_schema_context(backend, name).await;

    let header = match excel.get_header(name).await {
        Ok(h) => h,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let columns = header.columns();
    let total = sheet.row_count() as usize;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let display_idx = ctx.display_field.as_ref()
        .and_then(|df| ctx.column_names.iter().position(|(n, _)| n == df));

    for (row_id_idx, row_id) in sheet.get_row_ids().enumerate() {
        if row_id_idx < offset {
            continue;
        }
        if rows.len() >= limit.min(200) {
            break;
        }
        if let Ok(row) = sheet.get_row(row_id) {
            let mut row_obj = serde_json::Map::new();
            row_obj.insert("row_id".into(), serde_json::json!(row_id));
            for (i, col_def) in columns.iter().enumerate() {
                let off = u32::from(col_def.offset());
                let col_name = ctx.column_names.get(i).map_or_else(
                    || format!("col_{off}"), |(n, _)| n.clone(),
                );
                let val = if col_def.kind() == ironworks::file::exh::ColumnKind::String {
                    row.read_string(off).map_or(serde_json::json!(null), |s| serde_json::json!(s.to_string()))
                } else {
                    row.read::<u32>(off).map_or(serde_json::json!(null), |v| serde_json::json!(v))
                };
                let mut f = serde_json::Map::new();
                f.insert("name".into(), serde_json::json!(col_name));
                f.insert("value".into(), val);
                if Some(i) == display_idx {
                    f.insert("is_display".into(), serde_json::json!(true));
                }
                row_obj.insert(format!("f_{i}"), serde_json::Value::Object(f));
            }
            rows.push(serde_json::Value::Object(row_obj));
        }
    }

    McpResponse::Success(serde_json::json!({
        "sheet": name, "language": format!("{lang:?}"),
        "total_rows": total, "offset": offset, "limit": limit,
        "display_field": ctx.display_field, "rows": rows
    }).to_string())
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
    let header = match excel.get_header(name).await {
        Ok(h) => h,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let (target_row_id, target_subrow_id) = if let Some(search) = search_name {
        let search_lower = search.to_lowercase();
        let display_idx = ctx.display_field.as_ref()
            .and_then(|df| ctx.column_names.iter().position(|(n, _)| n == df))
            .unwrap_or(0);
        let columns = header.columns();
        let col_def = &columns[display_idx.min(columns.len().saturating_sub(1))];
        let mut found = None;
        for rid in sheet.get_row_ids() {
            if let Ok(r) = sheet.get_row(rid) {
                if col_def.kind() == ironworks::file::exh::ColumnKind::String {
                    if let Ok(s) = r.read_string(u32::from(col_def.offset())) {
                        if s.to_string().to_lowercase().contains(&search_lower) {
                            found = Some((rid, 0u16));
                            break;
                        }
                    }
                } else {
                    if let Ok(v) = r.read::<u32>(u32::from(col_def.offset())) {
                        if v.to_string().to_lowercase().contains(&search_lower) {
                            found = Some((rid, 0u16));
                            break;
                        }
                    }
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
    let columns = header.columns();
    let mut fields = serde_json::Map::new();
    let display_idx = ctx.display_field.as_ref()
        .and_then(|df| ctx.column_names.iter().position(|(n, _)| n == df));

    for (i, col_def) in columns.iter().enumerate() {
        let off = u32::from(col_def.offset());
        let col_name = ctx.column_names.get(i).map_or_else(
            || format!("col_{off}"), |(n, _)| n.clone(),
        );
        let col_type = ctx.column_names.get(i).map_or("unknown", |(_, t)| t);
        let val = if col_def.kind() == ironworks::file::exh::ColumnKind::String {
            row.read_string(off).map_or(serde_json::json!(null), |s| serde_json::json!(s.to_string()))
        } else {
            row.read::<u32>(off).map_or(serde_json::json!(null), |v| serde_json::json!(v))
        };
        let mut f = serde_json::Map::new();
        f.insert("name".into(), serde_json::json!(col_name));
        f.insert("type".into(), serde_json::json!(col_type));
        f.insert("value".into(), val);
        if Some(i) == display_idx {
            f.insert("is_display".into(), serde_json::json!(true));
        }
        fields.insert(format!("f_{i}"), serde_json::Value::Object(f));
    }

    McpResponse::Success(serde_json::json!({
        "sheet": name, "row_id": target_row_id, "subrow_id": target_subrow_id,
        "display_field": ctx.display_field, "field_count": columns.len(), "fields": fields
    }).to_string())
}

async fn process_search_cells(
    backend: &Backend,
    name: &str,
    query: &str,
    max_results: usize,
    lang: Language,
) -> McpResponse {
    let excel = backend.excel();

    let header = match excel.get_header(name).await {
        Ok(h) => h,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let sheet = match excel.get_sheet(name, lang).await {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let column_defs = header.columns();
    let ql = query.to_lowercase();
    let mut results: Vec<serde_json::Value> = Vec::new();

    for row_id in sheet.get_row_ids() {
        if results.len() >= max_results {
            break;
        }
        if let Ok(row) = sheet.get_row(row_id) {
            for col_def in column_defs {
                if results.len() >= max_results {
                    break;
                }
                if col_def.kind() != ironworks::file::exh::ColumnKind::String {
                    continue;
                }
                if let Ok(s) = row.read_string(u32::from(col_def.offset())) {
                    if s.to_string().to_lowercase().contains(&ql) {
                        results.push(serde_json::json!({
                            "row_id": row_id,
                            "column_offset": col_def.offset(),
                            "value": s.to_string()
                        }));
                    }
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
    let header = match excel.get_header(name).await {
        Ok(h) => h,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let sheet = match excel
        .get_sheet(name, lang)
        .await
    {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let row = match sheet.get_row(row_id) {
        Ok(r) => r,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let columns = header.columns();
    let idx = column_index.min(columns.len().saturating_sub(1));
    let col_def = &columns[idx];
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
    let header = match excel.get_header(name).await {
        Ok(h) => h,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let sheet = match excel
        .get_sheet(name, lang)
        .await
    {
        Ok(s) => s,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };
    let row = match sheet.get_subrow(row_id, subrow_id) {
        Ok(r) => r,
        Err(e) => return McpResponse::Error(format!("{e}")),
    };

    let columns = header.columns();
    let idx = column_index.min(columns.len().saturating_sub(1));
    let col_def = &columns[idx];
    if col_def.kind() != ironworks::file::exh::ColumnKind::String {
        return McpResponse::Error(format!(
            "列 {} 的类型为 {:?}，不是 String，无法解码 SeString",
            idx, col_def.kind()
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

async fn process_save_schema(backend: &Backend, name: &str, text: &str) -> McpResponse {
    let schema_provider = backend.schema();
    if !schema_provider.can_save_schemas() {
        return McpResponse::Error("当前模式提供者不支持保存".into());
    }
    match schema_provider.save_schema(name, text).await {
        Ok(()) => {
            McpResponse::Success(serde_json::json!({"saved": true, "name": name}).to_string())
        }
        Err(e) => McpResponse::Error(format!("{e}")),
    }
}

pub struct McpHandle {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for McpHandle {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
    }
}

pub fn start(sender: McpChannel, config: BackendConfig) -> McpHandle {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    std::thread::Builder::new()
        .name("mcp-server".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime for MCP server");

            rt.block_on(async move {
                let tx = sender.clone();
                let handler = Arc::new(McpHandler::new(tx, config));
                let ct = tokio_util::sync::CancellationToken::new();

                let config =
                    rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default()
                        .with_cancellation_token(ct.child_token());

                let service =
                    rmcp::transport::streamable_http_server::StreamableHttpService::new(
                        {
                            let handler = handler.clone();
                            move || Ok(handler.as_ref().clone())
                        },
                        Arc::new(
                            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
                        ),
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
                        let _ = shutdown_rx.await;
                        shutdown_ct.cancel();
                    });

                if let Err(e) = server.await {
                    log::error!("MCP 服务器错误: {e}");
                }
            });
        })
        .expect("Failed to spawn MCP server thread");

    McpHandle {
        shutdown: Some(shutdown_tx),
    }
}
