//! 导出: 数据表按筛选写成 CSV, 资源按路径或筛选语法批量落盘。
//!
//! 批量都并发跑: 每个资源的成本几乎全在等待读取上, 排队做等于把等待加在一起。CSV 则相反,
//! 一行一行落盘, 因为一张全表可能有几十万行, 先在内存里攒完再写等于白占一份同大的缓冲。

use std::{
    collections::HashSet,
    io::{BufWriter, Cursor},
    path::Path,
    str::FromStr,
};

use anyhow::{Context as _, Result, bail};
use compact_str::{CompactString, ToCompactString};
use futures_util::{StreamExt, stream};
use ironworks::{
    excel::Language,
    file::{File, scd::SoundContainer},
};

use crate::{
    assets::SearchMode,
    audio,
    backend::Backend,
    excel::{
        base::BaseSheet,
        provider::{ExcelHeader, ExcelProvider},
    },
    sheet::{CompiledFilterInput, ComplexFilter, FilterInput, TableContext},
    utils::tex_loader,
};

use super::{
    ColumnSelector, assets as asset_tools, build_table_context, filter_match_options, filter_row,
    get_row_at, row_locations, select_columns,
};

/// 同时导出几个资源。
const EXPORTS: usize = 8;
/// CSV 的写出缓冲。
const CSV_BUFFER: usize = 256 * 1024;
/// 一个目录最多逐个处理多少资源, 免得一次调用把几千个文件全读一遍再写出来。
const FOLDER_LIMIT: usize = 200;
/// 一个 `.scd` 没点名要哪条流时, 最多导出多少条。
const SOUND_STREAMS: usize = 64;

/// 写文件在各平台都走同一条路, 不阻塞工作线程。
async fn write_file(path: &Path, bytes: Vec<u8>) -> Result<()> {
    let named = path.display().to_string();
    let path = path.to_owned();
    blocking::unblock(move || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
        anyhow::Ok(())
    })
    .await
    .with_context(|| format!("写入 {named} 失败"))
}

/// 输出参数带扩展名就当文件名, 否则当目录并用 `stem` 在其中命名。
fn destination(output: &str, stem: &str, extension: &str) -> std::path::PathBuf {
    let path = Path::new(output);
    match path.extension().is_some() {
        true => path.to_owned(),
        false => path.join(format!("{stem}.{extension}")),
    }
}

pub struct SheetExport<'a> {
    pub name: &'a str,
    pub filter: Option<&'a str>,
    pub columns: Option<&'a [ColumnSelector]>,
    pub resolve_links: bool,
    pub limit: Option<usize>,
    pub output: &'a str,
    pub language: Language,
}

/// 把一张表按筛选写成 CSV: 省略 filter 就是全表。
pub async fn export_sheet(backend: &Backend, options: SheetExport<'_>) -> Result<String> {
    let excel = backend.excel();
    let sheet = excel.get_sheet(options.name, options.language).await?;
    let table = build_table_context(backend, options.name, sheet.clone(), options.language).await?;
    let columns = select_columns(&table, options.columns)?;
    let compiled = compile(&table, options.filter, options.resolve_links)?;

    let has_subrows = <BaseSheet as ExcelHeader>::has_subrows(&sheet);
    let mut header = Vec::with_capacity(columns.len() + 2);
    header.push("行号".to_owned());
    if has_subrows {
        header.push("子行号".to_owned());
    }
    header.extend(
        columns
            .iter()
            .map(|column| column.schema.name().to_owned()),
    );

    let mut record: Vec<CompactString> = Vec::with_capacity(header.len());
    let mut written = 0usize;
    let mut scanned = 0usize;

    let path = destination(options.output, options.name, "csv");
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 {} 失败", parent.display()))?;
    }
    let file =
        std::fs::File::create(&path).with_context(|| format!("创建 {} 失败", path.display()))?;
    let mut writer = csv::Writer::from_writer(BufWriter::with_capacity(CSV_BUFFER, file));
    writer.write_record(&header)?;

    for (sequence, (row_id, subrow_id)) in row_locations(&sheet).enumerate() {
        if sequence % 256 == 0 {
            tokio::task::yield_now().await;
        }
        if options.limit.is_some_and(|limit| written >= limit) {
            break;
        }
        scanned += 1;
        let Ok(row) = get_row_at(&sheet, row_id, subrow_id) else {
            continue;
        };
        if let Some(compiled) = &compiled
            && !filter_row(&table, row_id, subrow_id, &row, compiled, options.resolve_links).await?
        {
            continue;
        }

        record.clear();
        record.push(row_id.to_compact_string());
        if has_subrows {
            record.push(subrow_id.unwrap_or(0).to_compact_string());
        }
        for column in &columns {
            let value = table
                .cell_by_offset(row, column.index as u32)?
                .read(options.resolve_links)?;
            record.push(value.coerce_string());
        }
        writer.write_record(record.iter().map(CompactString::as_bytes))?;
        written += 1;
    }

    writer.flush().with_context(|| format!("写入 {} 失败", path.display()))?;
    drop(writer);
    let byte_count = std::fs::metadata(&path).map_or(0, |meta| meta.len());

    Ok(serde_json::json!({
        "sheet": options.name,
        "filter": options.filter,
        "language": format!("{0:?}", options.language),
        "columns": columns.iter().map(|column| column.schema.name()).collect::<Vec<_>>(),
        "rows": written,
        "scanned_rows": scanned,
        "bytes": byte_count,
        "path": path.to_string_lossy()
    })
    .to_string())
}

fn compile(
    table: &TableContext,
    filter: Option<&str>,
    resolve_links: bool,
) -> Result<Option<CompiledFilterInput>> {
    let Some(text) = filter.filter(|text| !text.trim().is_empty()) else {
        return Ok(None);
    };
    let parsed = ComplexFilter::from_str(text)
        .map_err(|error| anyhow::anyhow!("筛选表达式无效: {error}。先用 validate_filter 检查语法"))?;
    Ok(Some(
        table.compile_filter(&FilterInput::Complex(parsed), filter_match_options(resolve_links))?,
    ))
}

pub struct AssetExport<'a> {
    /// 精确路径, 逐个导出。
    pub paths: Vec<String>,
    /// 按筛选语法匹配路径, 与 `paths` 一起给出时两者都要。
    pub query: Option<&'a str>,
    pub match_mode: SearchMode,
    pub api_base: &'a str,
    /// `.scd` 里要导出的音频流序号, 省略时导出全部可解码的流。
    pub stream: Option<usize>,
    pub limit: usize,
    pub output: &'a str,
    pub max_dim: u32,
}

/// 把资源导出成 PNG、WAV 或原样字节。输出目录下沿用游戏路径本身的层级。
pub async fn export_assets(backend: &Backend, options: AssetExport<'_>) -> Result<String> {
    let mut wanted = options.paths.clone();
    if let Some(query) = options.query.filter(|query| !query.trim().is_empty()) {
        wanted.extend(
            asset_tools::match_paths(
                backend,
                options.api_base,
                query,
                options.match_mode,
                options.limit,
            )
            .await?,
        );
    }
    let mut seen = HashSet::new();
    wanted.retain(|path| seen.insert(path.to_ascii_lowercase()));
    if wanted.is_empty() {
        bail!("没有给出要导出的路径, 也没有筛选出任何路径");
    }
    wanted.truncate(options.limit);

    let options = &options;
    let results = stream::iter(wanted.iter().map(|path| async move {
        (path.as_str(), export_one(backend, path, options).await)
    }))
    .buffer_unordered(EXPORTS)
    .collect::<Vec<_>>()
    .await;

    let mut written = Vec::with_capacity(wanted.len());
    let mut failed = Vec::new();
    for (path, result) in results {
        match result {
            Ok(files) => written.extend(files),
            Err(error) => failed.push(serde_json::json!({"path": path, "error": error.to_string()})),
        }
    }

    Ok(serde_json::json!({
        "requested": wanted.len(),
        "exported": written.len(),
        "files": written,
        "failed": failed,
        "output": options.output
    })
    .to_string())
}

async fn export_one(backend: &Backend, path: &str, options: &AssetExport<'_>) -> Result<Vec<String>> {
    let root = Path::new(options.output);
    let extension = Path::new(path)
        .extension()
        .map(|held| held.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "tex" => {
            let bytes = backend.files().read(path).await?;
            let max_dim = options.max_dim.min(u32::from(u16::MAX)) as u16;
            let (image, _) = tex_loader::decode_preview_sized(&bytes, path, Some(max_dim))?;
            let png = tex_loader::write(image, image::ImageFormat::Png)?;
            let at = stem_path(root, path, "png");
            write_file(&at, png).await?;
            Ok(vec![at.to_string_lossy().into_owned()])
        }
        "scd" => export_sound(backend, path, options, root).await,
        _ => {
            let bytes = backend.files().read(path).await?;
            let at = stem_path(root, path, "");
            write_file(&at, bytes).await?;
            Ok(vec![at.to_string_lossy().into_owned()])
        }
    }
}

async fn export_sound(
    backend: &Backend,
    path: &str,
    options: &AssetExport<'_>,
    root: &Path,
) -> Result<Vec<String>> {
    let bytes = backend.files().read(path).await?;
    let container = SoundContainer::read(Cursor::new(bytes))?;
    let entries = container.entries();
    // 没指定流时导出全部可解码的, 但一个容器有几百条时不该一次写出几百个文件。
    let decoded = match options.stream {
        Some(index) => entries.get(index).map(|entry| vec![(index, entry)]),
        None => Some(entries.iter().enumerate().take(SOUND_STREAMS).collect()),
    };
    let Some(decoded) = decoded else {
        bail!(
            "该音频容器只有 {} 条流, 没有第 {} 条",
            entries.len(),
            options.stream.unwrap_or(0)
        );
    };
    if decoded.is_empty() {
        bail!("该音频容器没有可导出的流");
    }

    let stream = options.stream;
    let mut written = Vec::with_capacity(decoded.len());
    for (index, entry) in decoded {
        let wav = audio::encode_wav(&audio::decode_full(entry)?)?;
        let extension = match stream {
            Some(_) => "wav".to_owned(),
            None => format!("{index}.wav"),
        };
        let at = stem_path(root, path, &extension);
        write_file(&at, wav).await?;
        written.push(at.to_string_lossy().into_owned());
    }
    Ok(written)
}

/// 输出路径: 沿用游戏路径的目录层级, 换掉扩展名。
fn stem_path(root: &Path, path: &str, extension: &str) -> std::path::PathBuf {
    let relative = Path::new(path);
    let mut at = root.join(relative);
    if !extension.is_empty() {
        at.set_extension(extension);
    }
    at
}

/// 把一个文件或一个文件夹的结构化数据写到输出文件夹里的新文件。
pub async fn inspect_to_folder(
    backend: &Backend,
    path: &str,
    api_base: &str,
    output: &str,
    max_items: usize,
) -> Result<String> {
    let is_folder = Path::new(path).extension().is_none();
    let (targets, truncated) = match is_folder {
        true => {
            let found = asset_tools::under_folder(backend, api_base, path).await?;
            if found.is_empty() {
                bail!("{path} 下没有找到任何已安装资源");
            }
            let truncated = found.len() > FOLDER_LIMIT;
            (found, truncated)
        }
        false => (vec![path.to_owned()], false),
    };
    let targets = &targets[..targets.len().min(FOLDER_LIMIT)];

    let root = Path::new(output);
    let results = stream::iter(targets.iter().map(|target| async move {
        let named = format!("{}.json", target.replace('/', "_"));
        let at = root.join(&named);
        let bytes = match backend.files().read(target).await {
            Ok(bytes) => bytes,
            Err(error) => return (target, Err(error.to_string())),
        };
        let json = match asset_tools::inspect(target, &bytes, max_items) {
            Ok(json) => json,
            Err(error) => return (target, Err(error.to_string())),
        };
        match write_file(&at, json.into_bytes()).await {
            Ok(()) => (target, Ok(at.to_string_lossy().into_owned())),
            Err(error) => (target, Err(error.to_string())),
        }
    }))
    .buffer_unordered(EXPORTS)
    .collect::<Vec<_>>()
    .await;

    let mut written = Vec::with_capacity(results.len());
    let mut failed = Vec::new();
    for (target, result) in results {
        match result {
            Ok(at) => written.push(at),
            Err(error) => failed.push(serde_json::json!({"path": target, "error": error})),
        }
    }

    Ok(serde_json::json!({
        "path": path,
        "folder": is_folder,
        "count": targets.len(),
        "truncated": truncated,
        "written": written,
        "failed": failed,
        "output": output
    })
    .to_string())
}
