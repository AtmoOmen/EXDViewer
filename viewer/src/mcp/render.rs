//! 把资源画成位图, 供 MCP 工具直接交给模型查看。全部在 CPU 上完成, 不依赖 egui 的纹理
//! 系统与 GL 上下文, 因此能在没有窗口的 MCP 工作线程里跑。
//!
//! 绘制与取图分开: 每张要用的纹理表在画之前一次性并发取回, 绘制循环里就只剩查表。画布尺寸
//! 一律先过 [`bounded`], 所以一份异常的输入宽度也换不来一次几十 GB 的分配。

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    num::NonZeroUsize,
    rc::Rc,
};

use anyhow::{Result, anyhow, bail};
use egui::{Rect, Vec2, pos2, vec2};
use futures_util::{StreamExt, stream};
use glam::{Mat4, Vec3};
use image::{Rgba, RgbaImage, imageops};
use lru::LruCache;

use crate::{
    assets::{
        magic,
        viewers::{
            Preview, Viewer, font as font_view, icons as icons_view, mdl as mdl_view,
            uld as uld_view,
        },
    },
    backend::Backend,
    utils::tex_loader,
};

/// 渲染输出的最长边上限。
pub const MAX_DIM: u32 = 4096;
/// 画布空白处的底色, 浅色字形与图标在它上面读得出来。
const BACKDROP: Rgba<u8> = Rgba([24, 24, 28, 255]);
/// 字形与图标画的颜色。
const INK: [u8; 4] = [232, 232, 236, 255];
/// 一个字形的格子里, 四周留出的余量占行高的比例。
const GLYPH_PADDING: f32 = 0.18;
/// 布局里连续两个控件之间留出的高度。
const WIDGET_GAP: u32 = 8;
/// 一张图里最多画多少个字形或图标。
const MAX_CELLS: usize = 4096;
/// 一行字最多排多少个字符。
const MAX_TEXT_CHARS: usize = 4096;
/// 一次绘制里同时取回几份纹理, 让几张表的等待叠在一起而不是排队。
const FETCHES: usize = 8;
/// 字形表解码到的尺寸, 正是它们发布的尺寸。
const SHEET_DIM: u16 = 1024;
/// 界面部件解码到的尺寸。部件矩形按原始尺寸记, 所以解码得小一点只是糊, 不影响取到哪一块。
const ATLAS_DIM: u16 = 1024;
/// 纹理表缓存的条数, 以及界面纹理缓存的总字节预算。
const SHEET_CACHE_ENTRIES: usize = 16;
const TEXTURE_CACHE_BYTES: usize = 96 * 1024 * 1024;
/// 模型渲染的视场角, 与查看器的一致。
const FOV: f32 = 40.0_f32.to_radians();
/// 一个模型最多光栅化多少个三角形, 超了就取更粗的细节等级。
const MAX_TRIANGLES: usize = 150_000;

/// 一个字形表缓存的槽位: 路径与要抽出的通道, 到那片墨。
type SheetSlot = LruCache<(String, Option<usize>), Option<Rc<Ink>>>;

thread_local! {
    /// 字形表按 `(路径, 通道)` 跨调用复用。
    static SHEET_CACHE: RefCell<SheetSlot> =
        RefCell::new(LruCache::new(NonZeroUsize::new(SHEET_CACHE_ENTRIES).unwrap()));
    /// 界面纹理按路径跨调用复用, 按字节预算回收。
    static TEXTURE_CACHE: RefCell<TextureCache> =
        RefCell::new(TextureCache::new(TEXTURE_CACHE_BYTES));
}

/// 渲染模型时的方位与远近。
#[derive(Clone, Copy)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    /// 省略时按模型的包围球自动取一个能装下它的距离。
    pub distance: Option<f32>,
    /// 画面缩放, 同时用作字体与布局的字号倍数。
    pub zoom: f32,
    /// 用户显式给出的观察角度, 单位弧度。给定时, 字体与图标字体所在的那块平面也按它转过来,
    /// 于是字会有近大远小的透视; 不给就是正视。
    pub tilt: Option<(f32, f32)>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.15,
            distance: None,
            zoom: 1.0,
            tilt: None,
        }
    }
}

/// 一次渲染的全部可调项。
pub struct Options {
    pub max_dim: u32,
    pub text: Option<String>,
    pub camera: Camera,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_dim: 1024,
            text: None,
            camera: Camera::default(),
        }
    }
}

/// 画好的位图, 以及它是按哪种查看器画出来的。
pub struct Render {
    pub image: RgbaImage,
    pub viewer: Viewer,
}

pub async fn render(backend: &Backend, path: &str, options: &Options) -> Result<Render> {
    let bytes = backend.files().read(path).await?;
    let viewer = magic::sniff(&bytes)
        .map(|format| format.viewer())
        .ok_or_else(|| anyhow!("无法识别 {path} 的格式"))?;
    let limit = options.max_dim.clamp(16, MAX_DIM);
    let image = match viewer {
        Viewer::Texture => {
            let (image, _) = tex_loader::decode_preview_sized(
                &bytes,
                path,
                Some(limit.min(u32::from(u16::MAX)) as u16),
            )?;
            image.to_rgba8()
        }
        Viewer::Image => image::load_from_memory(&bytes)?.to_rgba8(),
        Viewer::Font => render_font(backend, path, &bytes, options, limit).await?,
        Viewer::Icons => render_icon_font(backend, path, &bytes, options, limit).await?,
        Viewer::Uld => render_layout(backend, path, &bytes, options, limit).await?,
        Viewer::Model => render_model(&bytes, options, limit)?,
        other => bail!("{} 目前不能渲染为图片", other.label()),
    };
    Ok(Render {
        image: fit(image, limit),
        viewer,
    })
}

/// 把一张图缩到最长边 `max_dim` 以内, 不放大。
fn fit(image: RgbaImage, max_dim: u32) -> RgbaImage {
    let (width, height) = (image.width(), image.height());
    let longest = width.max(height);
    if longest <= max_dim || longest == 0 {
        return image;
    }
    let scale = max_dim as f32 / longest as f32;
    let width = ((width as f32 * scale).round() as u32).max(1);
    let height = ((height as f32 * scale).round() as u32).max(1);
    imageops::resize(&image, width, height, imageops::FilterType::Triangle)
}

/// 把要求的画布尺寸收进 `max_dim` 见方的框里, 保持长宽比。纹理、字形数或布局尺寸都不受
/// 这份渲染控制, 所以每一次分配都要先过这里。
fn bounded(width: f32, height: f32, max_dim: u32) -> (u32, u32) {
    if !width.is_finite() || !height.is_finite() {
        return (1, 1);
    }
    let width = width.max(1.0);
    let height = height.max(1.0);
    let longest = width.max(height);
    let scale = (max_dim.max(16) as f32 / longest).min(1.0);
    (
        ((width * scale).round() as u32).max(1),
        ((height * scale).round() as u32).max(1),
    )
}

fn canvas(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_pixel(width.max(1), height.max(1), BACKDROP)
}

/// 把一笔颜色按给定的不透明度盖在像素上。
fn blend(into: &mut RgbaImage, x: u32, y: u32, colour: [u8; 4], alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0) * f32::from(colour[3]) / 255.0;
    if alpha <= 0.0 || x >= into.width() || y >= into.height() {
        return;
    }
    let pixel = into.get_pixel_mut(x, y);
    for (channel, source) in colour.iter().take(3).enumerate() {
        let source = f32::from(*source);
        let held = f32::from(pixel.0[channel]);
        pixel.0[channel] = (held + (source - held) * alpha).round() as u8;
    }
    pixel.0[3] = 255;
}

/// 一个纹理表里被当成白墨的那一部分。
struct Ink {
    image: RgbaImage,
    /// 字形表把四个字体分在四个通道上, 就取那一个; 图标表是一张完整的图, 取亮度乘不透明度。
    channel: Option<usize>,
}

impl Ink {
    /// 把一个 uv 矩形里的一片墨按 `colour` 画进 `dest`。每列的源像素与每行的源像素各算一次,
    /// 内层就只是查表与写像素。
    fn blit(&self, source: Rect, dest: Rect, into: &mut RgbaImage, colour: [u8; 4]) {
        let (sheet_width, sheet_height) = (self.image.width(), self.image.height());
        if sheet_width == 0 || sheet_height == 0 {
            return;
        }
        let width = dest.width().round().max(1.0) as u32;
        let height = dest.height().round().max(1.0) as u32;
        let (left, top) = (dest.min.x.round() as i64, dest.min.y.round() as i64);
        // 目标像素被压到画布外时, 只算它盖得住的那一段。
        let (x0, x1) = visible(0, width, left, into.width());
        let (y0, y1) = visible(0, height, top, into.height());
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let columns = (x0..x1)
            .map(|x| source_x(&self.image, source, x, width))
            .collect::<Vec<_>>();
        let channel = self.channel;

        for y in y0..y1 {
            let ty = source_y(&self.image, source, y, height);
            let row = ty * sheet_width;
            for (slot, x) in (x0..x1).enumerate() {
                let ink = match channel {
                    Some(channel) => {
                        f32::from(self.image.as_raw()[(row + columns[slot]) as usize * 4 + channel.min(3)])
                            / 255.0
                    }
                    None => {
                        let at = (row + columns[slot]) as usize * 4;
                        let raw = &self.image.as_raw()[at..at + 4];
                        let lit = f32::from(raw[0].max(raw[1]).max(raw[2])) / 255.0;
                        lit * f32::from(raw[3]) / 255.0
                    }
                };
                if ink <= 0.0 {
                    continue;
                }
                blend(into, x, (top + i64::from(y)) as u32, colour, ink);
            }
        }
    }
}

/// 一段目标像素里落在画布内的那一段, 返回在这段里的起止下标。
fn visible(from: u32, to: u32, at: i64, limit: u32) -> (u32, u32) {
    let first = (0i64 - at).max(0) as u32;
    let last = (i64::from(limit) - at).max(0) as u32;
    (from.max(first), to.min(last))
}

fn source_x(image: &RgbaImage, source: Rect, x: u32, width: u32) -> u32 {
    let u = source.min.x + source.width() * (x as f32 + 0.5) / width as f32;
    ((u.clamp(0.0, 1.0) * image.width() as f32) as u32).min(image.width().saturating_sub(1))
}

fn source_y(image: &RgbaImage, source: Rect, y: u32, height: u32) -> u32 {
    let v = source.min.y + source.height() * (y as f32 + 0.5) / height as f32;
    ((v.clamp(0.0, 1.0) * image.height() as f32) as u32).min(image.height().saturating_sub(1))
}

/// 一次绘制要用的字形表: 在画之前一次性并发取回, 之后只查表。
struct Sheets<'a> {
    held: HashMap<&'a str, HashMap<Option<usize>, Option<Rc<Ink>>>>,
}

impl<'a> Sheets<'a> {
    async fn warm(backend: &Backend, wanted: &[(&'a str, Option<usize>)]) -> Self {
        let mut seen = HashSet::new();
        let unique = wanted
            .iter()
            .filter(|key| seen.insert(**key))
            .copied()
            .collect::<Vec<_>>();
        let decoded = stream::iter(unique.into_iter().map(|(path, channel)| async move {
            (path, channel, sheet(backend, path, channel).await)
        }))
        .buffer_unordered(FETCHES)
        .collect::<Vec<_>>()
        .await;

        let mut held: HashMap<&'a str, HashMap<Option<usize>, Option<Rc<Ink>>>> = HashMap::new();
        for (path, channel, ink) in decoded {
            held.entry(path).or_default().insert(channel, ink);
        }
        Self { held }
    }

    fn get(&self, path: &str, channel: Option<usize>) -> Option<&Ink> {
        self.held.get(path)?.get(&channel)?.as_deref()
    }
}

async fn sheet(backend: &Backend, path: &str, channel: Option<usize>) -> Option<Rc<Ink>> {
    let key = (path.to_owned(), channel);
    if let Some(held) = SHEET_CACHE.with(|cache| cache.borrow_mut().get(&key).cloned()) {
        return held;
    }
    let decoded = backend
        .files()
        .read_texture(path, Some(SHEET_DIM))
        .await
        .ok()
        .map(|decoded| {
            Rc::new(Ink {
                image: decoded.image,
                channel,
            })
        });
    SHEET_CACHE.with(|cache| {
        cache.borrow_mut().put(key, decoded.clone());
    });
    decoded
}

/// 界面纹理的缓存: 条数不限, 按总字节数回收。
struct TextureCache {
    budget: usize,
    used: usize,
    held: HashMap<String, Rc<Atlas>>,
    order: Vec<String>,
}

impl TextureCache {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            held: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn get(&mut self, path: &str) -> Option<Rc<Atlas>> {
        let held = self.held.get(path).cloned();
        if held.is_some() {
            self.order.retain(|at| at != path);
            self.order.push(path.to_owned());
        }
        held
    }

    fn put(&mut self, path: &str, atlas: Rc<Atlas>) {
        let bytes = atlas.image.as_raw().len();
        if bytes > self.budget {
            return;
        }
        if let Some(previous) = self.held.insert(path.to_owned(), atlas) {
            self.used = self.used.saturating_sub(previous.image.as_raw().len());
            self.order.retain(|at| at != path);
        }
        self.used += bytes;
        self.order.push(path.to_owned());
        while self.used > self.budget && self.order.len() > 1 {
            let oldest = self.order.remove(0);
            if let Some(dropped) = self.held.remove(&oldest) {
                self.used = self.used.saturating_sub(dropped.image.as_raw().len());
            }
        }
    }
}

async fn texture(backend: &Backend, path: &str) -> Option<Rc<Atlas>> {
    if let Some(held) = TEXTURE_CACHE.with(|cache| cache.borrow_mut().get(path)) {
        return Some(held);
    }
    let decoded = backend
        .files()
        .read_texture(path, Some(ATLAS_DIM))
        .await
        .ok()
        .map(|decoded| {
            Rc::new(Atlas {
                image: decoded.image,
                source: decoded.source,
            })
        })?;
    TEXTURE_CACHE.with(|cache| cache.borrow_mut().put(path, decoded.clone()));
    Some(decoded)
}

/// 一张界面纹理, 连同它发布时的尺寸 —— 部件矩形是按后者记的。
struct Atlas {
    image: RgbaImage,
    source: [u16; 2],
}

/// 字形按字符取, 建一次索引, 免得每画一个字都扫一遍字表。
struct Glyphs<'a> {
    font: &'a font_view::Rendered,
    index: HashMap<char, usize>,
}

impl<'a> Glyphs<'a> {
    fn new(font: &'a font_view::Rendered) -> Self {
        let mut index = HashMap::with_capacity(font.glyphs.len());
        for (at, glyph) in font.glyphs.iter().enumerate() {
            index.entry(glyph.character).or_insert(at);
        }
        Self { font, index }
    }

    fn cell(&self, character: char) -> Option<&'a font_view::GlyphCell> {
        self.index
            .get(&character)
            .and_then(|at| self.font.glyphs.get(*at))
    }

    /// 每个字形从哪张表、哪个通道取墨。
    fn sheet_of(&self, cell: &'a font_view::GlyphCell) -> Option<(&'a str, Option<usize>)> {
        self.font
            .sheets
            .get(usize::from(cell.file))
            .map(|path| (path.as_str(), Some(usize::from(cell.channel))))
    }
}

async fn render_font(
    backend: &Backend,
    path: &str,
    bytes: &[u8],
    options: &Options,
    limit: u32,
) -> Result<RgbaImage> {
    let Preview::Font(font) = font_view::decode(path, bytes)? else {
        bail!("{path} 不是字体文件");
    };
    let scale = options.camera.zoom.max(0.05);
    let line = font.line_height * scale;
    let glyphs = Glyphs::new(&font);

    let text = options.text.as_deref().filter(|text| !text.is_empty());
    let wanted = match text {
        Some(text) => text
            .chars()
            .take(MAX_TEXT_CHARS)
            .collect::<Vec<_>>(),
        None => font
            .glyphs
            .iter()
            .map(|glyph| glyph.character)
            .take(MAX_CELLS)
            .collect::<Vec<_>>(),
    };
    if wanted.is_empty() {
        bail!("该字体没有可渲染的字形");
    }

    let sheets = Sheets::warm(
        backend,
        &wanted
            .iter()
            .filter_map(|character| glyphs.cell(*character))
            .filter_map(|cell| glyphs.sheet_of(cell))
            .collect::<Vec<_>>(),
    )
    .await;

    let drawn = match text {
        Some(_) => draw_line(&glyphs, &sheets, &wanted, line, scale, limit),
        None => draw_grid(&glyphs, &sheets, &wanted, line, scale, limit),
    };
    Ok(compose(drawn, options, limit))
}

/// 一行字排下来, 宽度按各自的前进量累加。
fn draw_line(
    glyphs: &Glyphs<'_>,
    sheets: &Sheets<'_>,
    wanted: &[char],
    line: f32,
    scale: f32,
    limit: u32,
) -> RgbaImage {
    let advance: f32 = wanted
        .iter()
        .map(|character| {
            glyphs
                .cell(*character)
                .map_or(line * 0.5, |cell| cell.advance * scale)
        })
        .sum();
    let padding = line * GLYPH_PADDING;
    let (width, height) = bounded(advance + padding * 2.0, line + padding * 2.0, limit);
    // 一行装不下时整行一起缩, 而不是把后半截裁掉。
    let shrink = (width as f32 / (advance + padding * 2.0).max(1.0)).min(1.0);
    let mut image = RgbaImage::new(width, height);

    let mut pen = padding * shrink;
    for character in wanted {
        let Some(cell) = glyphs.cell(*character) else {
            pen += line * 0.5 * shrink;
            continue;
        };
        if let Some((path, channel)) = glyphs.sheet_of(cell)
            && let Some(ink) = sheets.get(path, channel)
        {
            let dest = Rect::from_min_size(
                pos2(pen, padding * shrink + cell.offset_y * scale * shrink),
                vec2(cell.size.x * scale * shrink, cell.size.y * scale * shrink),
            );
            ink.blit(cell.source, dest, &mut image, INK);
        }
        pen += cell.advance * scale * shrink;
    }
    image
}

/// 字形铺成一张网格, 每个格子居中放一个。
fn draw_grid(
    glyphs: &Glyphs<'_>,
    sheets: &Sheets<'_>,
    wanted: &[char],
    line: f32,
    scale: f32,
    limit: u32,
) -> RgbaImage {
    let padding = line * GLYPH_PADDING;
    let cell = line + padding * 2.0;
    let columns = (wanted.len() as f32).sqrt().ceil().max(1.0) as u32;
    let rows = (wanted.len() as u32).div_ceil(columns);
    let (width, height) = bounded(
        cell * columns as f32,
        cell * rows as f32,
        limit,
    );
    let mut image = RgbaImage::new(width, height);
    // 画布可能被收小, 格子随之等比例收窄, 否则后面的格子会落到画布外。
    let step_x = width as f32 / columns as f32;
    let step_y = height as f32 / rows as f32;
    let shrink = (step_x.min(step_y) / cell).min(1.0);

    for (index, character) in wanted.iter().enumerate() {
        let Some(cell_glyph) = glyphs.cell(*character) else {
            continue;
        };
        let Some((path, channel)) = glyphs.sheet_of(cell_glyph) else {
            continue;
        };
        let Some(ink) = sheets.get(path, channel) else {
            continue;
        };
        let drawn = vec2(
            cell_glyph.size.x * scale * shrink,
            cell_glyph.size.y * scale * shrink,
        );
        let at = vec2(
            (index as u32 % columns) as f32 * step_x,
            (index as u32 / columns) as f32 * step_y,
        );
        let dest = Rect::from_min_size(
            pos2(
                at.x + (step_x - drawn.x) * 0.5,
                at.y + padding * shrink + cell_glyph.offset_y * scale * shrink,
            ),
            drawn,
        );
        ink.blit(cell_glyph.source, dest, &mut image, INK);
    }
    image
}

async fn render_icon_font(
    backend: &Backend,
    path: &str,
    bytes: &[u8],
    options: &Options,
    limit: u32,
) -> Result<RgbaImage> {
    let Preview::Icons(icons) = icons_view::decode(path, bytes)? else {
        bail!("{path} 不是图标字体文件");
    };
    let Some((_, sheet_path)) = icons.sheets.first() else {
        bail!("该图标字体没有纹理表");
    };
    let scale = options.camera.zoom.max(0.05);
    let cell = icons.largest * scale + Vec2::splat(icons.largest.y * GLYPH_PADDING * scale);
    let shown = icons.icons.len().min(MAX_CELLS);
    let columns = (shown as f32).sqrt().ceil().max(1.0) as u32;
    let rows = (shown as u32).div_ceil(columns);
    let (width, height) = bounded(cell.x * columns as f32, cell.y * rows as f32, limit);
    let mut image = RgbaImage::new(width, height);

    let sheets = Sheets::warm(backend, &[(sheet_path.as_str(), None)]).await;
    let Some(ink) = sheets.get(sheet_path, None) else {
        bail!("{sheet_path} 无法读取");
    };
    let step_x = width as f32 / columns as f32;
    let step_y = height as f32 / rows as f32;
    // 画布被收小后图标跟着缩, 否则它们会互相压在一起。
    let shrink = (step_x / cell.x).min(step_y / cell.y).min(1.0);

    for (index, icon) in icons.icons.iter().take(shown).enumerate() {
        let drawn = icon.size * scale * shrink;
        let at = vec2(
            (index as u32 % columns) as f32 * step_x,
            (index as u32 / columns) as f32 * step_y,
        );
        let dest = Rect::from_min_size(
            pos2(
                at.x + (step_x - drawn.x).max(0.0) * 0.5,
                at.y + (step_y - drawn.y).max(0.0) * 0.5,
            ),
            drawn,
        );
        ink.blit(icon.source, dest, &mut image, INK);
    }
    Ok(compose(image, options, limit))
}

async fn render_layout(
    backend: &Backend,
    path: &str,
    bytes: &[u8],
    options: &Options,
    limit: u32,
) -> Result<RgbaImage> {
    let Preview::Uld(layout) = uld_view::decode(path, bytes)? else {
        bail!("{path} 不是界面布局文件");
    };
    let scale = options.camera.zoom.max(0.05);

    let mut wanted = Vec::new();
    let mut seen = HashSet::new();
    for widget in &layout.widgets {
        for item in &widget.items {
            for piece in &item.pieces {
                if let Some(path) = piece.sprite.texture.as_deref()
                    && seen.insert(path)
                {
                    wanted.push(path);
                }
            }
        }
    }
    let fetched = stream::iter(wanted.into_iter().map(|path| async move {
        (path, texture(backend, path).await)
    }))
    .buffer_unordered(FETCHES)
    .collect::<HashMap<_, _>>()
    .await;

    let mut width = 0f32;
    let mut natural = 0f32;
    for widget in &layout.widgets {
        width = width.max(widget.extent.x * scale);
        natural += widget.extent.y * scale + WIDGET_GAP as f32;
    }
    let (width, height) = bounded(width, natural, limit);
    // 画布被收小后所有部件按同一个比例跟着缩, 否则后面的控件会落到画布外。
    let shrink = match natural > 0.0 {
        true => (height as f32 / natural).min(1.0),
        false => 1.0,
    };
    let mut image = canvas(width, height);

    let mut top = 0f32;
    for widget in &layout.widgets {
        for item in &widget.items {
            let tint = item.tint.to_array();
            for piece in &item.pieces {
                let Some(texture) = piece.sprite.texture.as_deref() else {
                    continue;
                };
                let Some(Some(atlas)) = fetched.get(texture) else {
                    continue;
                };
                let dest = Rect::from_min_max(
                    pos2(
                        piece.dest.min.x * scale * shrink,
                        piece.dest.min.y * scale * shrink + top,
                    ),
                    pos2(
                        piece.dest.max.x * scale * shrink,
                        piece.dest.max.y * scale * shrink + top,
                    ),
                );
                blit_sprite(atlas, piece, dest, tint, &mut image);
            }
        }
        top += (widget.extent.y * scale + WIDGET_GAP as f32) * shrink;
    }
    Ok(image)
}

/// 把一个部件矩形从纹理里取出来, 铺到目标矩形上。部件矩形按纹理的原始像素记, 而纹理可能是
/// 按更小的 mipmap 解码的, 所以两边先归一化再对上。要平铺的部件按源矩形自身的大小重复。
fn blit_sprite(
    atlas: &Atlas,
    piece: &uld_view::Piece,
    dest: Rect,
    tint: [u8; 4],
    into: &mut RgbaImage,
) {
    let sheet = &atlas.image;
    let (sheet_width, sheet_height) = (sheet.width(), sheet.height());
    if sheet_width == 0 || sheet_height == 0 {
        return;
    }
    let sprite = &piece.sprite;
    let (sx, sy) = (f32::from(sprite.x), f32::from(sprite.y));
    let (sw, sh) = (
        f32::from(sprite.width).max(1.0),
        f32::from(sprite.height).max(1.0),
    );
    let (source_width, source_height) = (
        f32::from(atlas.source[0].max(1)),
        f32::from(atlas.source[1].max(1)),
    );
    let width = dest.width().round().max(1.0) as u32;
    let height = dest.height().round().max(1.0) as u32;
    let (left, top) = (dest.min.x.round() as i64, dest.min.y.round() as i64);
    let (x0, x1) = visible(0, width, left, into.width());
    let (y0, y1) = visible(0, height, top, into.height());
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let columns = (x0..x1)
        .map(|x| {
            let u = match piece.tile {
                true => sx + (x as f32 % sw),
                false => sx + sw * (x as f32 + 0.5) / width as f32,
            };
            (((u / source_width) * sheet_width as f32) as u32).min(sheet_width - 1)
        })
        .collect::<Vec<_>>();

    for y in y0..y1 {
        let v = match piece.tile {
            true => sy + (y as f32 % sh),
            false => sy + sh * (y as f32 + 0.5) / height as f32,
        };
        let ty = (((v / source_height) * sheet_height as f32) as u32).min(sheet_height - 1);
        for (slot, x) in (x0..x1).enumerate() {
            let pixel = sheet.get_pixel(columns[slot], ty);
            if pixel.0[3] == 0 {
                continue;
            }
            let colour = [
                (u32::from(pixel.0[0]) * u32::from(tint[0]) / 255) as u8,
                (u32::from(pixel.0[1]) * u32::from(tint[1]) / 255) as u8,
                (u32::from(pixel.0[2]) * u32::from(tint[2]) / 255) as u8,
                pixel.0[3],
            ];
            blend(into, x, (top + i64::from(y)) as u32, colour, 1.0);
        }
    }
}

/// 把一张带透明底的字形图摆到画布上: 没给观察角度就直接叠上去, 给了就按那块平面转过来,
/// 于是近处的字大、远处的字小。
fn compose(drawn: RgbaImage, options: &Options, limit: u32) -> RgbaImage {
    let Some((yaw, pitch)) = options.camera.tilt else {
        let mut image = canvas(drawn.width(), drawn.height());
        overlay(&drawn, &mut image, 0, 0);
        return image;
    };

    let size = vec2(drawn.width() as f32, drawn.height() as f32);
    let corners = plane_corners(size, yaw, pitch, options.camera.zoom);
    let low = corners
        .iter()
        .fold(vec2(f32::INFINITY, f32::INFINITY), |held, point| {
            held.min(*point)
        });
    let high = corners
        .iter()
        .fold(vec2(f32::NEG_INFINITY, f32::NEG_INFINITY), |held, point| {
            held.max(*point)
        });
    let margin = 8.0;
    let (width, height) = bounded(
        high.x - low.x + margin * 2.0,
        high.y - low.y + margin * 2.0,
        limit,
    );
    let shift = vec2(margin - low.x, margin - low.y);

    let mut image = canvas(width, height);
    let corners = corners.map(|corner| corner + shift);
    blit_plane(&drawn, corners, &mut image);
    image
}

/// 一块正视时宽高为 `size` 的板, 绕竖轴转 `yaw`、绕横轴转 `pitch` 之后投影到屏幕上的四角,
/// 顺序为左上、右上、右下、左下。相机站在板的正面, 退开一块板的对角线那么远。
fn plane_corners(size: Vec2, yaw: f32, pitch: f32, zoom: f32) -> [Vec2; 4] {
    let half = size * 0.5 * zoom.max(0.05);
    let local = [
        vec2(-half.x, -half.y),
        vec2(half.x, -half.y),
        vec2(half.x, half.y),
        vec2(-half.x, half.y),
    ];
    let distance = half.length().max(1.0) * 2.0;
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    let (sin_pitch, cos_pitch) = pitch.sin_cos();
    local.map(|point| {
        let x = point.x * cos_yaw;
        let z = point.x * sin_yaw;
        let y = point.y * cos_pitch - z * sin_pitch;
        let z = point.y * sin_pitch + z * cos_pitch;
        let depth = (distance - z).max(distance * 0.05);
        vec2(x * distance / depth, y * distance / depth)
    })
}

/// 把一张位图按四个屏幕角的位置贴到画布上, 两个三角形各自用重心坐标插值 uv。
fn blit_plane(source: &RgbaImage, corners: [Vec2; 4], into: &mut RgbaImage) {
    let uv = [
        vec2(0.0, 0.0),
        vec2(1.0, 0.0),
        vec2(1.0, 1.0),
        vec2(0.0, 1.0),
    ];
    for (a, b, c) in [(0, 1, 2), (0, 2, 3)] {
        fill_triangle(
            source,
            [corners[a], corners[b], corners[c]],
            [uv[a], uv[b], uv[c]],
            into,
        );
    }
}

/// 一个三角形的重心坐标沿 x 每走一步各加多少。沿 y 的那一步由每行重新起算, 用不到。
fn steps_x(positions: [Vec2; 3], area: f32) -> (f32, f32) {
    let (p0, p1, p2) = (positions[0], positions[1], positions[2]);
    ((p1.y - p2.y) / area, (p2.y - p0.y) / area)
}

fn fill_triangle(source: &RgbaImage, positions: [Vec2; 3], uvs: [Vec2; 3], into: &mut RgbaImage) {
    let edge = |a: Vec2, b: Vec2, c: Vec2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let area = edge(positions[0], positions[1], positions[2]);
    if area.abs() < f32::EPSILON || source.width() == 0 || source.height() == 0 {
        return;
    }

    let last_x = into.width().saturating_sub(1) as i64;
    let last_y = into.height().saturating_sub(1) as i64;
    let columns = positions.iter().map(|point| point.x);
    let rows = positions.iter().map(|point| point.y);
    let x0 = (columns.clone().fold(f32::INFINITY, f32::min).floor() as i64).clamp(0, last_x);
    let x1 = (columns.fold(f32::NEG_INFINITY, f32::max).ceil() as i64).clamp(0, last_x);
    let y0 = (rows.clone().fold(f32::INFINITY, f32::min).floor() as i64).clamp(0, last_y);
    let y1 = (rows.fold(f32::NEG_INFINITY, f32::max).ceil() as i64).clamp(0, last_y);
    if x0 > x1 || y0 > y1 {
        return;
    }

    let (step_0, step_1) = steps_x(positions, area);
    let (source_width, source_height) = (source.width() as f32, source.height() as f32);

    for y in y0..=y1 {
        let start = vec2(x0 as f32 + 0.5, y as f32 + 0.5);
        let mut w0 = edge(positions[1], positions[2], start) / area;
        let mut w1 = edge(positions[2], positions[0], start) / area;
        for x in x0..=x1 {
            let w2 = 1.0 - w0 - w1;
            if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                let u = w0 * uvs[0].x + w1 * uvs[1].x + w2 * uvs[2].x;
                let v = w0 * uvs[0].y + w1 * uvs[1].y + w2 * uvs[2].y;
                let sx = (u.clamp(0.0, 1.0) * source_width) as u32;
                let sy = (v.clamp(0.0, 1.0) * source_height) as u32;
                let pixel = source
                    .get_pixel(
                        sx.min(source.width() - 1),
                        sy.min(source.height() - 1),
                    )
                    .0;
                if pixel[3] != 0 {
                    blend(into, x as u32, y as u32, pixel, 1.0);
                }
            }
            w0 += step_0;
            w1 += step_1;
        }
    }
}

/// 把一张透明底的位图按其自身不透明度盖到画布上。
fn overlay(source: &RgbaImage, into: &mut RgbaImage, left: i64, top: i64) {
    let (x0, x1) = visible(0, source.width(), left, into.width());
    let (y0, y1) = visible(0, source.height(), top, into.height());
    for y in y0..y1 {
        for x in x0..x1 {
            let pixel = source.get_pixel(x, y).0;
            if pixel[3] == 0 {
                continue;
            }
            blend(into, (left + i64::from(x)) as u32, (top + i64::from(y)) as u32, pixel, 1.0);
        }
    }
}

/// 模型的一次软件光栅化: 顶点按相机变换到屏幕空间, 逐三角形填色, 深度由 z 缓冲裁决。三角形
/// 太多时取更粗的细节等级, 免得一次渲染把几十万面全部填一遍。
fn render_model(bytes: &[u8], options: &Options, limit: u32) -> Result<RgbaImage> {
    let mut meshes = mdl_view::geometry(bytes, 0)?;
    if triangle_count(&meshes) > MAX_TRIANGLES {
        for lod in [1u8, 2] {
            let coarser = mdl_view::geometry(bytes, lod)?;
            if coarser.is_empty() {
                continue;
            }
            meshes = coarser;
            if triangle_count(&meshes) <= MAX_TRIANGLES {
                break;
            }
        }
    }
    if meshes.iter().all(|mesh| mesh.positions.is_empty()) {
        bail!("此模型没有可绘制的几何");
    }

    let vertices: usize = meshes.iter().map(|mesh| mesh.positions.len()).sum();
    let triangles: usize = meshes.iter().map(|mesh| mesh.indices.len() / 3).sum();
    if triangles == 0 {
        bail!("此模型没有可绘制的几何");
    }

    let mut positions: Vec<Vec3> = Vec::with_capacity(vertices);
    let mut normals: Vec<Vec3> = Vec::with_capacity(vertices);
    let mut indices: Vec<[usize; 3]> = Vec::with_capacity(triangles);
    for mesh in &meshes {
        let base = positions.len();
        positions.extend(mesh.positions.iter().map(|value| Vec3::from_array(*value)));
        normals.extend(
            mesh.normals
                .iter()
                .map(|value| Vec3::from_array(*value).normalize_or_zero()),
        );
        for triangle in mesh.indices.chunks_exact(3) {
            indices.push([
                base + usize::from(triangle[0]),
                base + usize::from(triangle[1]),
                base + usize::from(triangle[2]),
            ]);
        }
    }

    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    for position in &positions {
        low = low.min(*position);
        high = high.max(*position);
    }
    let center = (low + high) * 0.5;
    let radius = ((high - low).length() * 0.5).max(0.01);

    let camera = options.camera;
    let distance = camera
        .distance
        .unwrap_or_else(|| radius / (FOV * 0.5).tan() * 1.15)
        .max(radius * 0.01);
    let eye = center
        + distance
            * Vec3::new(
                camera.pitch.cos() * camera.yaw.sin(),
                camera.pitch.sin(),
                camera.pitch.cos() * camera.yaw.cos(),
            );
    let view = Mat4::look_at_rh(eye, center, Vec3::Y);
    let projection = Mat4::perspective_rh(FOV, 1.0, (radius * 0.01).max(0.001), radius * 20.0);
    let clip = projection * view;
    let light = Vec3::new(-0.45, 0.78, 0.44).normalize();

    let side = limit.max(64);
    let mut image = canvas(side, side);
    let mut depth = vec![f32::INFINITY; (side * side) as usize];
    let last = i64::from(side) - 1;

    for triangle in &indices {
        let mut screen = [[0.0f32; 3]; 3];
        let mut world = [Vec3::ZERO; 3];
        let mut behind = false;
        for (slot, index) in triangle.iter().enumerate() {
            let point = positions[*index];
            world[slot] = point;
            let clip_space = clip * point.extend(1.0);
            if clip_space.w <= 0.0 {
                behind = true;
                break;
            }
            let ndc = clip_space.truncate() / clip_space.w;
            screen[slot] = [
                (ndc.x * 0.5 + 0.5) * side as f32,
                (0.5 - ndc.y * 0.5) * side as f32,
                ndc.z,
            ];
        }
        if behind {
            continue;
        }

        let (a, b, c) = (screen[0], screen[1], screen[2]);
        let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        if area.abs() < f32::EPSILON {
            continue;
        }

        let plain = [(a[0], a[1]), (b[0], b[1]), (c[0], c[1])];
        let x0 = (plain.iter().map(|at| at.0).fold(f32::INFINITY, f32::min).floor() as i64)
            .clamp(0, last);
        let x1 = (plain.iter().map(|at| at.0).fold(f32::NEG_INFINITY, f32::max).ceil() as i64)
            .clamp(0, last);
        let y0 = (plain.iter().map(|at| at.1).fold(f32::INFINITY, f32::min).floor() as i64)
            .clamp(0, last);
        let y1 = (plain.iter().map(|at| at.1).fold(f32::NEG_INFINITY, f32::max).ceil() as i64)
            .clamp(0, last);
        if x0 > x1 || y0 > y1 {
            continue;
        }

        let face = (world[1] - world[0])
            .cross(world[2] - world[0])
            .normalize_or_zero();
        // 重心坐标沿 x 与 y 各走一步加多少, 每像素就只剩加与乘。
        let step_0 = ((b[1] - c[1]) / area, (c[0] - b[0]) / area);
        let step_1 = ((c[1] - a[1]) / area, (a[0] - c[0]) / area);
        let normal_a = normals[triangle[0]];
        let normal_b = normals[triangle[1]];
        let normal_c = normals[triangle[2]];

        for y in y0..=y1 {
            let start_x = x0 as f32 + 0.5;
            let start_y = y as f32 + 0.5;
            let mut w0 = ((b[0] - start_x) * (start_y - a[1]) - (b[1] - start_y) * (start_x - a[0]))
                / area;
            let mut w1 = ((start_x - a[0]) * (c[1] - a[1]) - (start_y - a[1]) * (c[0] - a[0]))
                / area;
            for x in x0..=x1 {
                let w2 = 1.0 - w0 - w1;
                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    let z = w1 * a[2] + w2 * b[2] + w0 * c[2];
                    let slot = (y as u32 * side + x as u32) as usize;
                    if z < depth[slot] {
                        depth[slot] = z;
                        // 插值后的法线不归一化: 单位长度附近误差很小, 省下一次开方。
                        let normal = w1 * normal_a + w2 * normal_b + w0 * normal_c;
                        let lit = normal.dot(light).abs() * 0.75 + 0.25;
                        let facing = 0.55 + 0.45 * normal.dot(face).abs();
                        let value = (lit * facing * 255.0).clamp(0.0, 255.0) as u8;
                        image.put_pixel(x as u32, y as u32, Rgba([value, value, value, 255]));
                    }
                }
                w0 += step_0.0;
                w1 += step_1.0;
            }
        }
    }
    Ok(image)
}

fn triangle_count(meshes: &[mdl_view::Geometry]) -> usize {
    meshes.iter().map(|mesh| mesh.indices.len() / 3).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blitting_a_plane_paints_the_source_colour() {
        let source = RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 255]));
        let mut into = canvas(32, 32);
        blit_plane(
            &source,
            [
                vec2(4.0, 4.0),
                vec2(28.0, 4.0),
                vec2(28.0, 28.0),
                vec2(4.0, 28.0),
            ],
            &mut into,
        );

        assert_eq!(into.get_pixel(16, 16).0, [255, 255, 255, 255]);
        assert_eq!(into.get_pixel(1, 1).0, BACKDROP.0);
    }

    #[test]
    fn a_tilted_plane_stays_inside_its_own_extent() {
        let size = vec2(200.0, 100.0);
        let corners = plane_corners(size, 0.5, 0.3, 1.0);
        let low = corners
            .iter()
            .fold(vec2(f32::INFINITY, f32::INFINITY), |held, point| {
                held.min(*point)
            });
        let high = corners
            .iter()
            .fold(vec2(f32::NEG_INFINITY, f32::NEG_INFINITY), |held, point| {
                held.max(*point)
            });

        assert!(high.x - low.x > 0.0);
        assert!(high.x - low.x <= size.x * 2.0);
        assert!(high.y - low.y <= size.y * 2.0);
    }

    #[test]
    fn an_absurd_canvas_is_bounded_before_it_is_allocated() {
        assert_eq!(bounded(1_000_000.0, 1_000_000.0, 1024), (1024, 1024));
        assert_eq!(bounded(0.0, 0.0, 1024), (1, 1));
        assert_eq!(bounded(f32::NAN, 10.0, 1024), (1, 1));
        assert_eq!(bounded(100.0, 50.0, 1024), (100, 50));
    }
}
