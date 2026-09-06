use std::{cell::RefCell, io::Cursor};

use base64::{Engine, prelude::BASE64_STANDARD};
use image::GenericImageView;
use ironworks::file::{File, layer, uld};
use pathlist::{PathList, Presence};

use crate::{assets::magic, backend::Backend};

const MAX_BYTES: usize = 64 * 1024;
const DEFAULT_BYTES: usize = 4 * 1024;
const MAX_ITEMS: usize = 500;

thread_local! {
    static PATH_INDEX_CACHE: RefCell<Option<PathIndexCache>> = const { RefCell::new(None) };
}

struct PathIndexCache {
    api_url: String,
    entries: Vec<PathEntry>,
    unnamed: Vec<pathlist::Unnamed>,
}

struct PathEntry {
    path: String,
    lowercase_path: String,
    present: bool,
}

fn bounded_limit(limit: Option<usize>, default: usize) -> usize {
    limit.unwrap_or(default).clamp(1, MAX_BYTES)
}

fn bytes_response(
    path: Option<&str>,
    hash: Option<(u8, u8, u64, bool)>,
    stream_kind: Option<String>,
    bytes: Vec<u8>,
    offset: usize,
    limit: Option<usize>,
) -> anyhow::Result<String> {
    let size = bytes.len();
    let offset = offset.min(size);
    let limit = bounded_limit(limit, DEFAULT_BYTES);
    let end = offset.saturating_add(limit).min(size);
    let chunk = &bytes[offset..end];
    let mut result = serde_json::Map::new();
    if let Some(path) = path {
        result.insert("path".into(), serde_json::json!(path));
    }
    if let Some((repository, category, hash, split)) = hash {
        result.insert(
            "hash".into(),
            serde_json::json!({
                "repository": repository,
                "category": category,
                "value": if split { format!("{hash:016X}") } else { format!("{hash:08X}") },
                "split": split
            }),
        );
    }
    let format = crate::assets::magic::sniff(&bytes).map(|format| {
        serde_json::json!({
            "label": format.label(),
            "viewer": format.viewer().label()
        })
    });
    result.insert("stream_kind".into(), serde_json::json!(stream_kind));
    result.insert("size".into(), serde_json::json!(size));
    result.insert("offset".into(), serde_json::json!(offset));
    result.insert("limit".into(), serde_json::json!(limit));
    result.insert(
        "next_offset".into(),
        serde_json::json!((end < size).then_some(end)),
    );
    result.insert("truncated".into(), serde_json::json!(end < size));
    result.insert("format".into(), format.unwrap_or(serde_json::Value::Null));
    result.insert(
        "bytes_base64".into(),
        serde_json::json!(BASE64_STANDARD.encode(chunk)),
    );
    result.insert(
        "bytes_hex".into(),
        serde_json::json!(
            chunk
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    );
    Ok(serde_json::Value::Object(result).to_string())
}

pub async fn read_path(
    backend: &Backend,
    path: &str,
    offset: usize,
    limit: Option<usize>,
) -> anyhow::Result<String> {
    let (stream_kind, bytes) = backend.files().read_stream(path).await?;
    bytes_response(Some(path), None, stream_kind, bytes, offset, limit)
}

pub async fn read_hash(
    backend: &Backend,
    repository: u8,
    category: u8,
    hash: u64,
    split: bool,
    offset: usize,
    limit: Option<usize>,
) -> anyhow::Result<String> {
    if !split && hash > u64::from(u32::MAX) {
        anyhow::bail!("whole 哈希超出 32 位范围");
    }
    let (stream_kind, bytes) = backend
        .files()
        .read_stream_by_hash(repository, category, hash, split)
        .await?;
    bytes_response(
        None,
        Some((repository, category, hash, split)),
        stream_kind,
        bytes,
        offset,
        limit,
    )
}

pub async fn inspect_path(
    backend: &Backend,
    path: &str,
    max_items: usize,
) -> anyhow::Result<String> {
    let bytes = backend.files().read(path).await?;
    inspect(path, &bytes, max_items)
}

pub async fn inspect_hash(
    backend: &Backend,
    repository: u8,
    category: u8,
    hash: u64,
    split: bool,
    max_items: usize,
) -> anyhow::Result<String> {
    if !split && hash > u64::from(u32::MAX) {
        anyhow::bail!("whole 哈希超出 32 位范围");
    }
    let bytes = backend
        .files()
        .read_by_hash(repository, category, hash, split)
        .await?;
    let path = if split {
        format!("{repository}/{category}/{hash:016X}")
    } else {
        format!("{repository}/{category}/{hash:08X}")
    };
    inspect(&path, &bytes, max_items)
}

pub async fn decode_texture(backend: &Backend, path: &str, max_dim: u16) -> anyhow::Result<String> {
    let max_dim = max_dim.clamp(1, 2048);
    let decoded = backend.files().read_texture(path, Some(max_dim)).await?;
    let width = decoded.image.width();
    let height = decoded.image.height();
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(decoded.image).write_to(&mut png, image::ImageFormat::Png)?;
    Ok(serde_json::json!({
        "path": path,
        "source_width": decoded.source[0],
        "source_height": decoded.source[1],
        "width": width,
        "height": height,
        "max_dim": max_dim,
        "png_base64": BASE64_STANDARD.encode(png.into_inner())
    })
    .to_string())
}

pub async fn exists_many(backend: &Backend, paths: &[String]) -> anyhow::Result<String> {
    if paths.len() > MAX_ITEMS {
        anyhow::bail!("一次最多检查 {MAX_ITEMS} 个路径");
    }
    let exists = backend.files().exists_many(paths).await?;
    Ok(serde_json::json!({
        "count": paths.len(),
        "paths": paths.iter().zip(exists).map(|(path, exists)| serde_json::json!({"path": path, "exists": exists})).collect::<Vec<_>>()
    })
    .to_string())
}

fn path_hashes(path: &str) -> serde_json::Value {
    use ironworks::sqpack::IndexHash;

    let (split, whole) = IndexHash::of(&path.to_ascii_lowercase());
    let IndexHash::Whole(whole) = whole else {
        unreachable!("IndexHash::of always returns a whole hash");
    };
    serde_json::json!({
        "split": split.map(|hash| match hash {
            IndexHash::Split(hash) => format!("{hash:016X}"),
            IndexHash::Whole(hash) => format!("{hash:08X}"),
        }),
        "whole": format!("{whole:08X}")
    })
}

async fn load_path_cache(backend: &Backend, api_base: &str) -> anyhow::Result<()> {
    let api_url = api_base.trim_end_matches('/').to_owned();
    let cached = PATH_INDEX_CACHE.with(|cache| {
        cache
            .borrow()
            .as_ref()
            .is_some_and(|cached| cached.api_url == api_url)
    });
    if cached {
        return Ok(());
    }

    let (path_bytes, presence_bytes) = backend.files().path_index(&api_url).await?;
    let paths = PathList::decode(&path_bytes)?;
    let presence = Presence::decode(&presence_bytes)?;
    if paths.list_id() != presence.list_id() {
        anyhow::bail!("路径列表与存在映射的 list_id 不一致");
    }

    let mut entries = Vec::with_capacity(paths.len());
    for (dir_index, directory) in paths.dirs().iter().enumerate() {
        if dir_index % 128 == 0 {
            tokio::task::yield_now().await;
        }
        let offset = paths.name_offset(dir_index)?;
        for (index, name) in paths.names(dir_index)?.into_iter().enumerate() {
            let path = if directory.is_empty() {
                name
            } else {
                format!("{directory}/{name}")
            };
            entries.push(PathEntry {
                lowercase_path: path.to_ascii_lowercase(),
                path,
                present: presence.contains(offset + index),
            });
        }
    }
    PATH_INDEX_CACHE.with(|cache| {
        cache.replace(Some(PathIndexCache {
            api_url,
            entries,
            unnamed: presence.unnamed().to_vec(),
        }));
    });
    Ok(())
}

pub async fn list_paths(
    backend: &Backend,
    api_base: &str,
    query: Option<&str>,
    include_missing: bool,
    include_unnamed: bool,
    offset: usize,
    limit: usize,
) -> anyhow::Result<String> {
    load_path_cache(backend, api_base).await?;
    let query = query.map(str::to_ascii_lowercase);
    let page_limit = limit.clamp(1, MAX_ITEMS);
    Ok(PATH_INDEX_CACHE.with(|cache| {
        let cache = cache.borrow();
        let cache = cache.as_ref().expect("path index initialized");
        let matches = cache.entries.iter().filter(|entry| {
            (include_missing || entry.present)
                && query
                    .as_ref()
                    .is_none_or(|query| entry.lowercase_path.contains(query))
        });
        let total = matches.clone().count();
        let paths = matches
            .skip(offset)
            .take(page_limit)
            .map(|entry| serde_json::json!({"path": entry.path, "present": entry.present, "hashes": path_hashes(&entry.path)}))
            .collect::<Vec<_>>();
        let unnamed = if include_unnamed {
            cache
                .unnamed
                .iter()
                .skip(offset)
                .take(page_limit)
                .map(|file| serde_json::json!({"repository": file.repository, "category": file.category, "hash": if file.split { format!("{:016X}", file.hash) } else { format!("{:08X}", file.hash as u32) }, "split": file.split}))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        serde_json::json!({
            "total": total,
            "offset": offset,
            "limit": page_limit,
            "has_more": offset.saturating_add(paths.len()) < total,
            "paths": paths,
            "unnamed": unnamed,
            "unnamed_total": cache.unnamed.len()
        })
        .to_string()
    }))
}

fn resource_json(
    resource: &ironworks::file::shpk::Resource,
    name: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "id": resource.id(),
        "name": name,
        "kind": resource.kind(),
        "slot": resource.slot(),
        "size": resource.size()
    })
}

fn inspect_texture(bytes: &[u8]) -> anyhow::Result<serde_json::Value> {
    let texture = ironworks::file::tex::Texture::read(Cursor::new(bytes.to_vec()))?;
    let mips = (0..texture.mip_levels())
        .map(|level| {
            let (width, height) = texture.mip_size(level);
            serde_json::json!({"level": level, "width": width, "height": height, "bytes": texture.mip_data(level).map_or(0, <[u8]>::len)})
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "format": format!("{:?}", texture.format()),
        "format_kind": format!("{:?}", texture.format().kind()),
        "components": texture.format().components(),
        "bits_per_pixel": texture.format().bits_per_pixel(),
        "kind": crate::assets::viewers::texture::texture_kind_name(texture.kind()),
        "width": texture.width(),
        "height": texture.height(),
        "depth": texture.depth(),
        "array_size": texture.array_size(),
        "layers": texture.layers(0),
        "mip_levels": mips
    }))
}

fn inspect_image(bytes: &[u8]) -> anyhow::Result<serde_json::Value> {
    let image = image::load_from_memory(bytes)?;
    let (width, height) = image.dimensions();
    let color = image.color();
    Ok(serde_json::json!({
        "width": width,
        "height": height,
        "color_type": format!("{:?}", color),
        "bits_per_pixel": color.bits_per_pixel(),
        "has_alpha": color.has_alpha()
    }))
}

fn inspect_material(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let material = ironworks::file::mtrl::Material::read(Cursor::new(bytes.to_vec()))?;
    let textures = material
        .textures()
        .iter()
        .take(max_items)
        .map(|texture| serde_json::json!({"path": texture.path(), "dx11": texture.dx11()}))
        .collect::<Vec<_>>();
    let samplers = material
        .samplers()
        .iter()
        .take(max_items)
        .map(|sampler| serde_json::json!({"id": sampler.id(), "flags": sampler.flags(), "texture_index": sampler.texture_index()}))
        .collect::<Vec<_>>();
    let constants = material
        .constants()
        .iter()
        .take(max_items)
        .map(|constant| serde_json::json!({"id": constant.id(), "values": material.constant_values(constant)}))
        .collect::<Vec<_>>();
    let color_rows = material
        .color_table()
        .map(|table| {
            (0..table.rows().min(max_items))
                .filter_map(|index| table.row_values(index))
                .map(|row| {
                    serde_json::json!({
                        "diffuse": row.diffuse,
                        "specular": row.specular,
                        "emissive": row.emissive,
                        "roughness": row.roughness,
                        "metalness": row.metalness,
                        "anisotropy": row.anisotropy,
                        "shader_index": row.shader_index,
                        "tile_index": row.tile_index,
                        "tile_alpha": row.tile_alpha,
                        "sphere_index": row.sphere_index,
                        "tile_transform": row.tile_transform
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(serde_json::json!({
        "version": material.version(),
        "shader": material.shader(),
        "shader_flags": material.shader_flags(),
        "uv_sets": material.uv_sets().iter().map(|set| serde_json::json!({"name": set.name(), "index": set.index()})).collect::<Vec<_>>(),
        "color_sets": material.color_sets().iter().map(|set| serde_json::json!({"name": set.name(), "index": set.index()})).collect::<Vec<_>>(),
        "textures": textures,
        "samplers": samplers,
        "shader_keys": material.shader_keys().iter().take(max_items).map(|key| serde_json::json!({"category": key.category(), "value": key.value()})).collect::<Vec<_>>(),
        "constants": constants,
        "color_table": material.color_table().map(|table| serde_json::json!({"kind": format!("{:?}", table.kind()), "rows": table.rows(), "dye_values": table.dye().len(), "values": color_rows}))
    }))
}

fn inspect_font(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let font = ironworks::file::fdt::FontData::read(Cursor::new(bytes.to_vec()))?;
    let kernings = font.kerning();
    Ok(serde_json::json!({
        "texture_size": [font.texture_width(), font.texture_height()],
        "size": font.size(),
        "line_height": font.line_height(),
        "ascent": font.ascent(),
        "descent": font.descent(),
        "glyph_count": font.glyphs().len(),
        "kerning_count": kernings.len(),
        "glyphs": font.glyphs().iter().take(max_items).map(|glyph| serde_json::json!({"character": glyph.character().to_string(), "codepoint": u32::from(glyph.character()), "x": glyph.x(), "y": glyph.y(), "width": glyph.width(), "height": glyph.height(), "texture_file": glyph.texture_file(), "texture_channel": glyph.texture_channel(), "offset_y": glyph.offset_y(), "advance": glyph.advance_width()})).collect::<Vec<_>>(),
        "kernings": kernings.iter().take(max_items).map(|kerning| serde_json::json!({"left": kerning.left().to_string(), "left_codepoint": u32::from(kerning.left()), "right": kerning.right().to_string(), "right_codepoint": u32::from(kerning.right()), "left_shift_jis": kerning.left_shift_jis(), "right_shift_jis": kerning.right_shift_jis(), "offset": kerning.offset()})).collect::<Vec<_>>(),
        "truncated": {
            "glyphs": font.glyphs().len() > max_items,
            "kernings": kernings.len() > max_items
        }
    }))
}

fn inspect_icons(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let icons = ironworks::file::gfd::FontIcons::read(Cursor::new(bytes.to_vec()))?;
    Ok(serde_json::json!({
        "count": icons.icons().len(),
        "icons": icons.icons().iter().take(max_items).map(|icon| serde_json::json!({"id": icon.id(), "left": icon.left(), "top": icon.top(), "width": icon.width(), "height": icon.height(), "redirect": icon.redirect(), "resolved_id": icons.icon(icon.id()).map(|resolved| resolved.id())})).collect::<Vec<_>>()
    }))
}

fn uld_flags_json(flags: uld::NodeFlags) -> serde_json::Value {
    serde_json::json!({
        "bits": flags.bits(),
        "visible": flags.visible(),
        "enabled": flags.enabled(),
        "clip": flags.clip(),
        "fill": flags.fill(),
        "anchor_top": flags.anchor_top(),
        "anchor_bottom": flags.anchor_bottom(),
        "anchor_left": flags.anchor_left(),
        "anchor_right": flags.anchor_right(),
        "has_collision": flags.has_collision()
    })
}

fn uld_text_flags_json(flags: uld::TextFlags) -> serde_json::Value {
    serde_json::json!({
        "bits": flags.bits(),
        "bold": flags.bold(),
        "italic": flags.italic(),
        "edge": flags.edge(),
        "glare": flags.glare(),
        "multiline": flags.multiline(),
        "ellipsis": flags.ellipsis(),
        "word_wrap": flags.word_wrap(),
        "emboss": flags.emboss()
    })
}

fn uld_node_kind_json(kind: &uld::NodeKind) -> serde_json::Value {
    match kind {
        uld::NodeKind::Res => serde_json::json!({"name": "Res"}),
        uld::NodeKind::Image(image) => serde_json::json!({
            "name": "Image",
            "part_list_id": image.part_list_id,
            "part_id": image.part_id,
            "flip_horizontal": image.flip_horizontal,
            "flip_vertical": image.flip_vertical,
            "wrap": image.wrap,
            "unknown": image.unknown
        }),
        uld::NodeKind::Text(text) => serde_json::json!({
            "name": "Text",
            "text_id": text.text_id,
            "color": text.color,
            "alignment": text.alignment,
            "font": format!("{:?}", text.font),
            "font_size": text.font_size,
            "edge_color": text.edge_color,
            "flags": uld_text_flags_json(text.flags),
            "sheet_type": text.sheet_type,
            "char_spacing": text.char_spacing,
            "line_spacing": text.line_spacing,
            "flags2": text.flags2
        }),
        uld::NodeKind::NineGrid(grid) => serde_json::json!({
            "name": "NineGrid",
            "part_list_id": grid.part_list_id,
            "part_id": grid.part_id,
            "parts_type": grid.parts_type,
            "render_type": grid.render_type,
            "top_offset": grid.top_offset,
            "bottom_offset": grid.bottom_offset,
            "left_offset": grid.left_offset,
            "right_offset": grid.right_offset,
            "blend_mode": grid.blend_mode,
            "unknown": grid.unknown
        }),
        uld::NodeKind::Counter(counter) => serde_json::json!({
            "name": "Counter",
            "part_list_id": counter.part_list_id,
            "part_id": counter.part_id,
            "number_width": counter.number_width,
            "comma_width": counter.comma_width,
            "space_width": counter.space_width,
            "alignment": counter.alignment,
            "unknown": counter.unknown
        }),
        uld::NodeKind::Collision(collision) => serde_json::json!({
            "name": "Collision",
            "kind": collision.kind,
            "unknown": collision.unknown,
            "x": collision.x,
            "y": collision.y,
            "radius": collision.radius
        }),
        uld::NodeKind::ClippingMask(mask) => serde_json::json!({
            "name": "ClippingMask",
            "part_list_id": mask.part_list_id,
            "part_id": mask.part_id
        }),
        uld::NodeKind::Component {
            component_id,
            instance,
        } => serde_json::json!({
            "name": "Component",
            "component_id": component_id,
            "instance": {
                "index": instance.index,
                "up": instance.up,
                "down": instance.down,
                "left": instance.left,
                "right": instance.right,
                "cursor": instance.cursor,
                "flags": instance.flags,
                "unknown": instance.unknown,
                "offset_x": instance.offset_x,
                "offset_y": instance.offset_y
            }
        }),
        uld::NodeKind::Unknown { node_type, data } => serde_json::json!({
            "name": "Unknown",
            "node_type": node_type,
            "data_bytes": data.len()
        }),
    }
}

fn uld_node_json(node: &uld::Node) -> serde_json::Value {
    serde_json::json!({
        "id": node.id(),
        "parent_id": node.parent_id(),
        "next_sibling_id": node.next_sibling_id(),
        "previous_sibling_id": node.previous_sibling_id(),
        "child_node_id": node.child_node_id(),
        "type": node.node_type(),
        "x": node.x(),
        "y": node.y(),
        "width": node.width(),
        "height": node.height(),
        "rotation": node.rotation(),
        "scale_x": node.scale_x(),
        "scale_y": node.scale_y(),
        "origin_x": node.origin_x(),
        "origin_y": node.origin_y(),
        "priority": node.priority(),
        "tab_index": node.tab_index(),
        "flags": uld_flags_json(node.flags()),
        "multiply": node.multiply(),
        "add": node.add(),
        "alpha": node.alpha(),
        "clip_count": node.clip_count(),
        "timeline_id": node.timeline_id(),
        "unknown": node.unknown(),
        "kind": uld_node_kind_json(node.kind()),
        "trailing_bytes": node.trailing().len()
    })
}

fn uld_key_group_json(group: &uld::KeyGroup) -> serde_json::Value {
    serde_json::json!({
        "usage": format!("{:?}", group.usage()),
        "kind": format!("{:?}", group.kind()),
        "keyframe_count": group.keyframe_count(),
        "keyframe_size": group.keyframe_size(),
        "data_bytes": group.data().len(),
        "data_hex": group
            .data()
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join("")
    })
}

fn uld_animation_json(animation: &uld::Animation, index: usize) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "start": animation.start_frame(),
        "end": animation.end_frame(),
        "groups": animation.groups().iter().map(uld_key_group_json).collect::<Vec<_>>()
    })
}

fn inspect_uld(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let layout = uld::UiLayout::read(Cursor::new(bytes.to_vec()))?;
    let part_lists = layout
        .part_lists()
        .iter()
        .take(max_items)
        .map(|list| serde_json::json!({"id": list.id(), "part_count": list.parts().len()}))
        .collect::<Vec<_>>();
    let mut parts = Vec::with_capacity(max_items);
    'parts: for list in layout.part_lists() {
        for (index, part) in list.parts().iter().enumerate() {
            if parts.len() == max_items {
                break 'parts;
            }
            parts.push(serde_json::json!({
                "list_id": list.id(),
                "index": index,
                "texture_id": part.texture_id(),
                "u": part.u(),
                "v": part.v(),
                "width": part.width(),
                "height": part.height()
            }));
        }
    }
    let total_parts = layout
        .part_lists()
        .iter()
        .map(|list| list.parts().len())
        .sum::<usize>();
    let components = layout
        .components()
        .iter()
        .take(max_items)
        .map(|component| {
            serde_json::json!({
                "id": component.id(),
                "kind": format!("{:?}", component.kind()),
                "ignore_input": component.ignore_input(),
                "drag_arrow": component.drag_arrow(),
                "drop_arrow": component.drop_arrow(),
                "node_count": component.nodes().len(),
                "nodes": component.nodes().iter().map(uld_node_json).collect::<Vec<_>>(),
                "trailing_bytes": component.trailing().len()
            })
        })
        .collect::<Vec<_>>();
    let timelines = layout
        .timelines()
        .iter()
        .take(max_items)
        .map(|timeline| {
            serde_json::json!({
                "id": timeline.id(),
                "animation_count": timeline.animations().len(),
                "label_set_count": timeline.label_sets().len(),
                "animations": timeline.animations().iter().enumerate().map(|(index, animation)| uld_animation_json(animation, index)).collect::<Vec<_>>(),
                "label_sets": timeline.label_sets().iter().enumerate().map(|(index, animation)| uld_animation_json(animation, index)).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let mut animations = Vec::with_capacity(max_items);
    for timeline in layout.timelines() {
        for (index, animation) in timeline.animations().iter().enumerate() {
            if animations.len() == max_items {
                break;
            }
            animations.push(serde_json::json!({
                "timeline_id": timeline.id(),
                "index": index,
                "start": animation.start_frame(),
                "end": animation.end_frame(),
                "group_count": animation.groups().len()
            }));
        }
        if animations.len() == max_items {
            break;
        }
    }
    let widgets = layout
        .widgets()
        .iter()
        .take(max_items)
        .map(|widget| {
            serde_json::json!({
                "id": widget.id(),
                "alignment": format!("{:?}", widget.alignment()),
                "themed_assets": widget.themed_assets(),
                "x": widget.x(),
                "y": widget.y(),
                "node_count": widget.nodes().len(),
                "nodes": widget.nodes().iter().map(uld_node_json).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "version": layout.version().as_str(),
        "textures": layout.textures().iter().take(max_items).map(|texture| serde_json::json!({"id": texture.id(), "path": texture.path(), "icon_id": texture.icon_id(), "theme_bitmask": texture.theme_bitmask()})).collect::<Vec<_>>(),
        "texture_count": layout.textures().len(),
        "part_lists": part_lists,
        "part_list_count": layout.part_lists().len(),
        "parts": parts,
        "components": components,
        "component_count": layout.components().len(),
        "timelines": timelines,
        "timeline_count": layout.timelines().len(),
        "animations": animations,
        "widgets": widgets,
        "widget_count": layout.widgets().len(),
        "truncated": {
            "textures": layout.textures().len() > max_items,
            "part_lists": layout.part_lists().len() > max_items,
            "parts": total_parts > max_items,
            "components": layout.components().len() > max_items,
            "widgets": layout.widgets().len() > max_items,
            "timelines": layout.timelines().len() > max_items
        }
    }))
}

fn inspect_shpk(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let package = ironworks::file::shpk::ShaderPackage::parse(bytes)?;
    let shaders = package
        .shaders()
        .iter()
        .take(max_items)
        .map(|shader| {
            serde_json::json!({
                "stage": format!("{:?}", shader.stage()),
                "blob_offset": shader.blob_offset(),
                "blob_size": shader.blob_size(),
                "resource_count": shader.resources().len()
            })
        })
        .collect::<Vec<_>>();
    let mut shader_resources = Vec::with_capacity(max_items);
    for (shader_index, shader) in package.shaders().iter().enumerate() {
        for resource in shader.resources() {
            if shader_resources.len() == max_items {
                break;
            }
            shader_resources.push(serde_json::json!({
                "shader_index": shader_index,
                "resource": resource_json(resource, package.name(resource))
            }));
        }
        if shader_resources.len() == max_items {
            break;
        }
    }
    let mut passes = Vec::with_capacity(max_items);
    for (node_index, node) in package.nodes().iter().enumerate() {
        for pass in node.passes() {
            if passes.len() == max_items {
                break;
            }
            passes.push(serde_json::json!({
                "node_index": node_index,
                "id": pass.id(),
                "stages": pass.stages()
            }));
        }
        if passes.len() == max_items {
            break;
        }
    }
    let resources = |items: &[ironworks::file::shpk::Resource]| {
        items
            .iter()
            .take(max_items)
            .map(|resource| resource_json(resource, package.name(resource)))
            .collect::<Vec<_>>()
    };
    Ok(serde_json::json!({
        "version": package.version(),
        "directx": format!("{:?}", package.directx()),
        "blob_offset": package.blobs_offset(),
        "bytecode_size": package.bytecode_size(),
        "shaders": shaders,
        "shader_count": package.shaders().len(),
        "shader_resources": shader_resources,
        "material_params": package.material_params().iter().take(max_items).map(|param| serde_json::json!({"id": param.id(), "offset": param.byte_offset(), "size": param.byte_size(), "default": package.param_default(param)})).collect::<Vec<_>>(),
        "param_buffer_size": package.param_buffer_size(),
        "constants": resources(package.constants()),
        "samplers": resources(package.samplers()),
        "textures": resources(package.textures()),
        "uavs": resources(package.uavs()),
        "keys": {
            "system": package.system_keys().iter().take(max_items).map(|key| serde_json::json!({"id": key.id(), "default": key.default_value()})).collect::<Vec<_>>(),
            "scene": package.scene_keys().iter().take(max_items).map(|key| serde_json::json!({"id": key.id(), "default": key.default_value()})).collect::<Vec<_>>(),
            "material": package.material_keys().iter().take(max_items).map(|key| serde_json::json!({"id": key.id(), "default": key.default_value()})).collect::<Vec<_>>(),
            "subview_defaults": package.technique_subview()
        },
        "key_counts": {"system": package.system_keys().len(), "scene": package.scene_keys().len(), "material": package.material_keys().len()},
        "nodes": package.nodes().iter().take(max_items).map(|node| serde_json::json!({"id": node.id(), "keys": node.keys(), "pass_count": node.passes().len()})).collect::<Vec<_>>(),
        "node_count": package.nodes().len(),
        "passes": passes,
        "aliases": package.aliases().iter().take(max_items).map(|alias| serde_json::json!({"selector": alias.selector(), "node": alias.node()})).collect::<Vec<_>>(),
        "alias_count": package.aliases().len(),
        "cluster_count": package.clusters().len()
    }))
}

fn inspect_shcd(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let code = ironworks::file::shcd::ShaderCode::parse(bytes)?;
    let resources = |items: &[ironworks::file::shpk::Resource]| {
        items
            .iter()
            .take(max_items)
            .map(|resource| resource_json(resource, code.name(resource)))
            .collect::<Vec<_>>()
    };
    Ok(serde_json::json!({
        "version": code.version(),
        "stage": format!("{:?}", code.stage()),
        "directx": format!("{:?}", code.directx()),
        "blob_offset": code.blob_offset(),
        "blob_size": code.blob_size(),
        "resources": resources(code.resources()),
        "constants": resources(code.constants()),
        "samplers": resources(code.samplers()),
        "textures": resources(code.textures()),
        "uavs": resources(code.uavs()),
        "constant_count": code.constants().len(),
        "sampler_count": code.samplers().len(),
        "texture_count": code.textures().len(),
        "uav_count": code.uavs().len(),
        "truncated": {
            "resources": code.resources().len() > max_items,
            "constants": code.constants().len() > max_items,
            "samplers": code.samplers().len() > max_items,
            "textures": code.textures().len() > max_items,
            "uavs": code.uavs().len() > max_items
        }
    }))
}

fn inspect_scd(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let container = ironworks::file::scd::SoundContainer::read(Cursor::new(bytes.to_vec()))?;
    let entries = container
        .entries()
        .iter()
        .take(max_items)
        .map(|entry| {
            serde_json::json!({
                "slot": entry.slot(),
                "codec": format!("{:?}", entry.format()),
                "channels": entry.channel_count(),
                "sample_rate": entry.sample_rate(),
                "loop_start": entry.loop_start(),
                "loop_end": entry.loop_end(),
                "markers": entry.markers(),
                "bytes": entry.data().len()
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "sound_count": container.sound_count(),
        "track_count": container.track_count(),
        "stream_count": container.entries().len(),
        "truncated": container.entries().len() > max_items,
        "entries": entries
    }))
}

fn layer_transform_json(transform: layer::Transform) -> serde_json::Value {
    serde_json::json!({
        "translation": transform.translation(),
        "rotation": transform.rotation(),
        "scale": transform.scale()
    })
}

fn layer_trigger_json(trigger: layer::TriggerBox) -> serde_json::Value {
    serde_json::json!({
        "shape": format!("{:?}", trigger.shape()),
        "priority": trigger.priority(),
        "enabled": trigger.enabled()
    })
}

fn layer_weapon_json(weapon: layer::WeaponModel) -> serde_json::Value {
    serde_json::json!({
        "skeleton_id": weapon.skeleton_id(),
        "pattern_id": weapon.pattern_id(),
        "image_change_id": weapon.image_change_id(),
        "staining_id": weapon.staining_id()
    })
}

fn layer_instance_json(instance: &layer::Instance) -> serde_json::Value {
    use layer::InstanceData;

    let data = match instance.data() {
        InstanceData::None => serde_json::json!({"name": "None"}),
        InstanceData::BgPart(part) => serde_json::json!({
            "name": "BgPart",
            "asset_path": part.asset_path(),
            "collision_asset_path": part.collision_asset_path(),
            "collision": format!("{:?}", part.collision()),
            "collision_material_mask": part.collision_material_mask(),
            "collision_material_id": part.collision_material_id(),
            "visible": part.visible(),
            "world_light_shadow_mode": format!("{:?}", part.world_light_shadow_mode()),
            "object_light_shadow_mode": format!("{:?}", part.object_light_shadow_mode()),
            "fade_out_distance": part.fade_out_distance(),
            "bounding_sphere_size": part.bounding_sphere_size()
        }),
        InstanceData::Light(light) => {
            let colour = light.colour();
            serde_json::json!({
                "name": "Light",
                "kind": format!("{:?}", light.kind()),
                "attenuation": light.attenuation(),
                "range": light.range(),
                "point_light_kind": format!("{:?}", light.point_light_kind()),
                "attenuation_cone_coefficient": light.attenuation_cone_coefficient(),
                "spot_angle": light.spot_angle(),
                "texture_path": light.texture_path(),
                "colour": {"red": colour.red(), "green": colour.green(), "blue": colour.blue(), "alpha": colour.alpha(), "intensity": colour.intensity()},
                "specular_highlights": light.specular_highlights(),
                "bg_part_shadows": light.bg_part_shadows(),
                "character_shadows": light.character_shadows()
            })
        }
        InstanceData::Vfx(vfx) => {
            let colour = vfx.colour();
            serde_json::json!({
                "name": "Vfx",
                "asset_path": vfx.asset_path(),
                "soft_particle_fade_range": vfx.soft_particle_fade_range(),
                "colour": {"red": colour.red(), "green": colour.green(), "blue": colour.blue(), "alpha": colour.alpha()},
                "auto_play": vfx.auto_play(),
                "no_far_clip": vfx.no_far_clip(),
                "fade_near": vfx.fade_near(),
                "fade_far": vfx.fade_far(),
                "z_correct": vfx.z_correct()
            })
        }
        InstanceData::PositionMarker(marker) => serde_json::json!({
            "name": "PositionMarker",
            "kind": format!("{:?}", marker.kind()),
            "comment_jp_offset": marker.comment_jp_offset(),
            "comment_en_offset": marker.comment_en_offset()
        }),
        InstanceData::SharedGroup(group) => serde_json::json!({
            "name": "SharedGroup",
            "asset_path": group.asset_path(),
            "initial_door_state": format!("{:?}", group.initial_door_state()),
            "initial_rotation_state": format!("{:?}", group.initial_rotation_state()),
            "initial_transform_state": format!("{:?}", group.initial_transform_state()),
            "initial_colour_state": format!("{:?}", group.initial_colour_state()),
            "random_timeline_auto_play": group.random_timeline_auto_play(),
            "random_timeline_loop_playback": group.random_timeline_loop_playback(),
            "collision_controllable_without_event_object": group.collision_controllable_without_event_object(),
            "bound_client_path_instance_id": group.bound_client_path_instance_id(),
            "overrides_bytes": group.overrides().len()
        }),
        InstanceData::Sound(sound) => serde_json::json!({
            "name": "Sound",
            "asset_path": sound.asset_path(),
            "kind": format!("{:?}", sound.kind()),
            "auto_play": sound.auto_play(),
            "no_far_clip": sound.no_far_clip(),
            "point_selection": sound.point_selection(),
            "attenuation": sound.attenuation().map(|attenuation| serde_json::json!({
                "inner_radius": attenuation.inner_radius(),
                "outer_radius": attenuation.outer_radius(),
                "volume_a": attenuation.volume_a(),
                "volume_b": attenuation.volume_b()
            })),
            "binary_bytes": sound.binary().len()
        }),
        InstanceData::HelperObject(helper) => {
            let weapon = helper.weapon();
            serde_json::json!({
                "name": "HelperObject",
                "kind": format!("{:?}", helper.kind()),
                "object_id": helper.object_id(),
                "base_id": helper.base_id(),
                "party_index": helper.party_index(),
                "member_index": helper.member_index(),
                "roster_index": helper.roster_index(),
                "weapon": {"skeleton_id": weapon.skeleton_id(), "pattern_id": weapon.pattern_id(), "image_change_id": weapon.image_change_id(), "staining_id": weapon.staining_id()},
                "height": helper.height(),
                "forces_base_id": helper.forces_base_id(),
                "normal_mode": helper.normal_mode(),
                "nested": helper.nested().map(layer_instance_json),
                "placement": helper.placement().map(|placement| serde_json::json!({
                    "transform": layer_transform_json(placement.transform()),
                    "flags": placement.flags(),
                    "unknown": placement.unknown()
                })),
                "unknown_1": helper.unknown_1(),
                "unknown_2": helper.unknown_2(),
                "unknown_3": helper.unknown_3(),
                "unknown_4": helper.unknown_4(),
                "unknown_5": helper.unknown_5(),
                "unknown_6": helper.unknown_6(),
                "unknown_7": helper.unknown_7(),
                "unknown_8": helper.unknown_8()
            })
        }
        InstanceData::EventNpc(npc) => serde_json::json!({
            "name": "EventNpc",
            "base_id": npc.character().object().base_id(),
            "unknown": npc.character().unknown(),
            "unknown_fields": npc.unknown()
        }),
        InstanceData::Character(character) => serde_json::json!({
            "name": "Character",
            "base_id": character.object().base_id(),
            "unknown": character.unknown()
        }),
        InstanceData::Aetheryte(aetheryte) => serde_json::json!({
            "name": "Aetheryte",
            "base_id": aetheryte.object().base_id(),
            "bound_instance_id": aetheryte.bound_instance_id(),
            "unknown": aetheryte.unknown()
        }),
        InstanceData::EnvSpace(space) => serde_json::json!({
            "name": "EnvSpace",
            "asset_path": space.asset_path(),
            "bound_instance_id": space.bound_instance_id(),
            "shape": format!("{:?}", space.shape()),
            "env_map_shooting_point": space.env_map_shooting_point(),
            "priority": space.priority(),
            "effective_range": space.effective_range(),
            "interpolation_time": space.interpolation_time(),
            "reverb": space.reverb(),
            "filter": space.filter(),
            "sound_asset_path": space.sound_asset_path()
        }),
        InstanceData::Treasure(treasure) => serde_json::json!({
            "name": "Treasure",
            "base_id": treasure.object().base_id()
        }),
        InstanceData::Weapon(weapon) => {
            let model = weapon.model();
            serde_json::json!({
                "name": "Weapon",
                "model": {"skeleton_id": model.skeleton_id(), "pattern_id": model.pattern_id(), "image_change_id": model.image_change_id(), "staining_id": model.staining_id()},
                "visible": weapon.visible()
            })
        }
        InstanceData::PopRange(range) => serde_json::json!({
            "name": "PopRange",
            "kind": format!("{:?}", range.kind()),
            "inner_radius_ratio": range.inner_radius_ratio(),
            "positions": range.positions()
        }),
        InstanceData::ExitRange(range) => serde_json::json!({
            "name": "ExitRange",
            "trigger": layer_trigger_json(range.trigger()),
            "kind": format!("{:?}", range.kind()),
            "zone_id": range.zone_id(),
            "territory_type_id": range.territory_type_id(),
            "index": range.index(),
            "destination_instance_id": range.destination_instance_id(),
            "return_instance_id": range.return_instance_id(),
            "player_running_direction": range.player_running_direction()
        }),
        InstanceData::MapRange(range) => serde_json::json!({
            "name": "MapRange",
            "trigger": layer_trigger_json(range.trigger()),
            "map": range.map(),
            "place_name_block": range.place_name_block(),
            "place_name_spot": range.place_name_spot(),
            "weather": range.weather(),
            "bgm": range.bgm(),
            "housing_block_id": range.housing_block_id(),
            "rest_bonus_effective": range.rest_bonus_effective(),
            "discovery_id": range.discovery_id(),
            "map_enabled": range.map_enabled(),
            "place_name_enabled": range.place_name_enabled(),
            "discovery_enabled": range.discovery_enabled(),
            "bgm_enabled": range.bgm_enabled(),
            "weather_enabled": range.weather_enabled(),
            "rest_bonus_enabled": range.rest_bonus_enabled(),
            "bgm_play_zone_in_only": range.bgm_play_zone_in_only(),
            "lift_enabled": range.lift_enabled(),
            "housing_enabled": range.housing_enabled(),
            "log_flying_height_max_err": range.log_flying_height_max_err(),
            "mounts_and_ornaments_disabled": range.mounts_and_ornaments_disabled(),
            "lalafell_only": range.lalafell_only()
        }),
        InstanceData::EventObject(object) => serde_json::json!({
            "name": "EventObject",
            "base_id": object.object().base_id(),
            "bound_instance_id": object.bound_instance_id(),
            "unknown": object.unknown()
        }),
        InstanceData::EnvLocation(location) => serde_json::json!({
            "name": "EnvLocation",
            "ambient_light_asset_path": location.ambient_light_asset_path(),
            "env_map_asset_path": location.env_map_asset_path()
        }),
        InstanceData::EventRange(range) => serde_json::json!({
            "name": "EventRange",
            "trigger": layer_trigger_json(*range)
        }),
        InstanceData::QuestMarker(marker) => serde_json::json!({
            "name": "QuestMarker",
            "unknown": marker.unknown()
        }),
        InstanceData::CollisionBox(collision) => serde_json::json!({
            "name": "CollisionBox",
            "trigger": layer_trigger_json(collision.trigger()),
            "collision_material_mask": collision.collision_material_mask(),
            "collision_material_id": collision.collision_material_id(),
            "collision_asset_path": collision.collision_asset_path()
        }),
        InstanceData::DoorRange(range) => serde_json::json!({
            "name": "DoorRange",
            "trigger": layer_trigger_json(*range)
        }),
        InstanceData::LineVfx(vfx) => serde_json::json!({
            "name": "LineVfx",
            "style": format!("{:?}", vfx.style())
        }),
        InstanceData::ClientPath(path) => serde_json::json!({
            "name": "ClientPath",
            "control_points": path.control_points().iter().map(|point| serde_json::json!({
                "position": point.position(),
                "id": point.id(),
                "select": point.select()
            })).collect::<Vec<_>>()
        }),
        InstanceData::TargetMarker(marker) => serde_json::json!({
            "name": "TargetMarker",
            "nameplate_offset_y": marker.nameplate_offset_y(),
            "kind": format!("{:?}", marker.kind())
        }),
        InstanceData::ChairMarker(marker) => serde_json::json!({
            "name": "ChairMarker",
            "left": marker.left(),
            "right": marker.right(),
            "back": marker.back(),
            "kind": format!("{:?}", marker.kind())
        }),
        InstanceData::ClickableRange(range) => serde_json::json!({
            "name": "ClickableRange",
            "trigger": layer_trigger_json(*range)
        }),
        InstanceData::PrefetchRange(range) => serde_json::json!({
            "name": "PrefetchRange",
            "trigger": layer_trigger_json(range.trigger()),
            "bound_instance_id": range.bound_instance_id()
        }),
        InstanceData::FateRange(range) => serde_json::json!({
            "name": "FateRange",
            "trigger": layer_trigger_json(range.trigger()),
            "fate_layout_label_id": range.fate_layout_label_id()
        }),
        InstanceData::Decal(decal) => serde_json::json!({
            "name": "Decal",
            "diffuse_path": decal.diffuse_path(),
            "normal_path": decal.normal_path(),
            "specular_path": decal.specular_path()
        }),
        InstanceData::CullingBox(culling) => serde_json::json!({
            "name": "CullingBox",
            "unknown": culling.unknown()
        }),
        InstanceData::Unknown(bytes) => serde_json::json!({
            "name": "Unknown",
            "bytes": bytes.len(),
            "hex": bytes.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" ")
        }),
    };
    serde_json::json!({
        "kind": format!("{:?}", instance.kind()),
        "id": instance.id(),
        "name": instance.name(),
        "transform": layer_transform_json(instance.transform()),
        "data": data
    })
}

fn scene_lane_json(lane: &layer::Lane) -> serde_json::Value {
    serde_json::json!({
        "active": lane.active(),
        "amount": lane.amount(),
        "period": lane.period(),
        "wrap": lane.wrap()
    })
}

fn inspect_sgb(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let file = ironworks::file::sgb::SharedGroupFile::read(Cursor::new(bytes.to_vec()))?;
    let scene = file.scene();
    let groups = scene
        .layer_groups()
        .iter()
        .take(max_items)
        .map(|group| {
            serde_json::json!({
                "id": group.id(),
                "name": group.name(),
                "layer_count": group.layers().len(),
                "truncated": group.layers().len() > max_items,
                "layers": group.layers().iter().take(max_items).map(|layer| {
                    serde_json::json!({
                        "id": layer.id(),
                        "name": layer.name(),
                        "visible": layer.visible(),
                        "festival_id": layer.festival_id(),
                        "festival_phase_id": layer.festival_phase_id(),
                        "instance_count": layer.instances().len(),
                        "truncated": layer.instances().len() > max_items,
                        "instances": layer.instances().iter().take(max_items).map(layer_instance_json).collect::<Vec<_>>()
                    })
                }).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let environments = scene
        .environments()
        .iter()
        .take(max_items)
        .map(|environment| {
            serde_json::json!({
                "index": environment.index(),
                "env_location_instance_id": environment.env_location_instance_id(),
                "asset_path": environment.asset_path(),
                "sound_asset_path": environment.sound_asset_path()
            })
        })
        .collect::<Vec<_>>();
    let timelines = scene
        .timelines()
        .iter()
        .take(max_items)
        .map(|timeline| {
            serde_json::json!({
                "sub_id": timeline.sub_id(),
                "kind": timeline.kind(),
                "auto_play": timeline.auto_play(),
                "looping": timeline.looping(),
                "animated": timeline.animated(),
                "items": timeline.timeline().items().iter().take(max_items).map(tmb_item_json).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let animations = scene
        .animations()
        .iter()
        .take(max_items)
        .map(|animation| {
            serde_json::json!({
                "instances": animation.instances(),
                "translation": scene_lane_json(animation.translation()),
                "rotation": scene_lane_json(animation.rotation()),
                "scale": scene_lane_json(animation.scale())
            })
        })
        .collect::<Vec<_>>();
    let spins = scene
        .spins()
        .iter()
        .take(max_items)
        .map(|spin| {
            serde_json::json!({
                "instance": spin.instance(),
                "axis": spin.axis(),
                "period": spin.period()
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "general": scene.general(),
        "sun_tilt_degrees": scene.sun_tilt_degrees(),
        "group_count": scene.layer_groups().len(),
        "environment_count": scene.environments().len(),
        "filter_count": scene.filters().len(),
        "timeline_count": scene.timelines().len(),
        "animation_count": scene.animations().len(),
        "spin_count": scene.spins().len(),
        "groups": groups,
        "environments": environments,
        "timelines": timelines,
        "animations": animations,
        "spins": spins,
        "truncated": {
            "groups": scene.layer_groups().len() > max_items,
            "environments": scene.environments().len() > max_items,
            "timelines": scene.timelines().len() > max_items,
            "animations": scene.animations().len() > max_items,
            "spins": scene.spins().len() > max_items
        }
    }))
}

fn inspect_lgb(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let file = ironworks::file::lgb::LayerGroupFile::read(Cursor::new(bytes.to_vec()))?;
    let group = file.group();
    let layers = group
        .layers()
        .iter()
        .take(max_items)
        .map(|layer| {
            serde_json::json!({
                "id": layer.id(),
                "name": layer.name(),
                "visible": layer.visible(),
                "festival_id": layer.festival_id(),
                "festival_phase_id": layer.festival_phase_id(),
                "instance_count": layer.instances().len(),
                "truncated": layer.instances().len() > max_items,
                "instances": layer.instances().iter().take(max_items).map(layer_instance_json).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "id": group.id(),
        "name": group.name(),
        "layer_count": group.layers().len(),
        "truncated": group.layers().len() > max_items,
        "layers": layers
    }))
}

fn tmb_condition_json(step: &ironworks::file::tmb::Condition) -> serde_json::Value {
    serde_json::json!({
        "operation": step.operation(),
        "value": step.value(),
        "float": step.float()
    })
}

fn tmb_key_json(key: &ironworks::file::tmb::Key) -> serde_json::Value {
    serde_json::json!({
        "linear": key.linear(),
        "time": key.time(),
        "rate": key.rate(),
        "value": key.value(),
        "slope_in": key.slope_in(),
        "slope_out": key.slope_out()
    })
}

fn tmb_curve_json(curve: &ironworks::file::tmb::Curve) -> serde_json::Value {
    serde_json::json!({
        "tag": curve.tag(),
        "target": curve.target(),
        "role": curve.role(),
        "parent": curve.parent(),
        "channel": curve.channel().map(|channel| format!("{:?}", channel)),
        "keys": curve.keys().iter().map(tmb_key_json).collect::<Vec<_>>()
    })
}

fn tmb_command_kind_json(kind: &ironworks::file::tmb::CommandKind) -> serde_json::Value {
    use ironworks::file::tmb::CommandKind;

    match kind {
        CommandKind::C002(body) => serde_json::json!({"magic": "C002", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "path": body.path()}),
        CommandKind::C004(body) => serde_json::json!({"magic": "C004", "duration": body.duration(), "unknown_1": body.unknown_1(), "curve_id": body.curve_id(), "name": body.name(), "near_plane": body.near_plane(), "far_plane": body.far_plane(), "bindings": body.bindings()}),
        CommandKind::C006(body) => serde_json::json!({"magic": "C006", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C009(body) => serde_json::json!({"magic": "C009", "duration": body.duration(), "unknown_1": body.unknown_1(), "motion": body.motion()}),
        CommandKind::C010(body) => serde_json::json!({"magic": "C010", "duration": body.duration(), "unknown_1": body.unknown_1(), "flags": body.flags(), "animation_start": body.animation_start(), "animation_end": body.animation_end(), "motion": body.motion(), "unknown_2": body.unknown_2()}),
        CommandKind::C011(body) => serde_json::json!({"magic": "C011", "enabled": body.enabled(), "unknown_2": body.unknown_2()}),
        CommandKind::C012(body) => serde_json::json!({"magic": "C012", "duration": body.duration(), "unknown_1": body.unknown_1(), "path": body.path(), "bind_origin_1": body.bind_origin_1(), "bind_type_1": body.bind_type_1(), "bind_id_1": body.bind_id_1(), "bind_origin_2": body.bind_origin_2(), "bind_type_2": body.bind_type_2(), "bind_id_2": body.bind_id_2(), "scale": body.scale(), "rotation": body.rotation(), "position": body.position(), "rgba": body.rgba(), "visibility": body.visibility(), "unknown_3": body.unknown_3()}),
        CommandKind::C013(body) => serde_json::json!({"magic": "C013", "duration": body.duration(), "unknown_2": body.unknown_2(), "curve_id": body.curve_id(), "placement": body.placement()}),
        CommandKind::C014(body) => serde_json::json!({"magic": "C014", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "object_position": body.object_position(), "object_control": body.object_control()}),
        CommandKind::C015(body) => serde_json::json!({"magic": "C015", "duration": body.duration(), "unknown_2": body.unknown_2(), "weapon_size": body.weapon_size(), "object_control": body.object_control()}),
        CommandKind::C018(body) => serde_json::json!({"magic": "C018", "duration": body.duration(), "unknown_1": body.unknown_1(), "translation": body.translation(), "rotation": body.rotation(), "scale": body.scale()}),
        CommandKind::C019(body) => serde_json::json!({"magic": "C019", "duration": body.duration(), "unknown_1": body.unknown_1(), "visibility": body.visibility()}),
        CommandKind::C021(body) => serde_json::json!({"magic": "C021", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4()}),
        CommandKind::C031(body) => serde_json::json!({"magic": "C031", "duration": body.duration(), "unknown_1": body.unknown_1(), "animation": body.animation(), "target_type": body.target_type()}),
        CommandKind::C033(body) => serde_json::json!({"magic": "C033", "enabled": body.enabled(), "unknown_2": body.unknown_2()}),
        CommandKind::C034(body) => serde_json::json!({"magic": "C034", "enabled": body.enabled(), "unknown_2": body.unknown_2()}),
        CommandKind::C040(body) => serde_json::json!({"magic": "C040", "enabled": body.enabled(), "unknown_1": body.unknown_1(), "motion": body.motion(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4()}),
        CommandKind::C042(body) => serde_json::json!({"magic": "C042", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "bind_id": body.bind_id(), "sound_id": body.sound_id()}),
        CommandKind::C043(body) => serde_json::json!({"magic": "C043", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "weapon_id": body.weapon_id(), "body_id": body.body_id(), "variant_id": body.variant_id()}),
        CommandKind::C048(body) => serde_json::json!({"magic": "C048", "enabled": body.enabled(), "unknown_1": body.unknown_1(), "subtitle_type": body.subtitle_type(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5(), "captions": body.captions().iter().map(|caption| serde_json::json!({"enabled": caption.enabled(), "duration": caption.duration(), "unknown_3": caption.unknown_3()})).collect::<Vec<_>>(), "key": body.key(), "unknown_6": body.unknown_6(), "unknown_7": body.unknown_7(), "unknown_8": body.unknown_8(), "unknown_9": body.unknown_9()}),
        CommandKind::C049(body) => serde_json::json!({"magic": "C049", "enabled": body.enabled(), "unknown_1": body.unknown_1(), "curve_id": body.curve_id(), "path": body.path(), "second_object": body.second_object(), "unknown_2": body.unknown_2(), "bind_type_1": body.bind_type_1(), "bind_type_2": body.bind_type_2(), "unknown_3": body.unknown_3(), "bind_id_1": body.bind_id_1(), "bind_id_2": body.bind_id_2(), "unknown_4": body.unknown_4(), "flags": body.flags(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6(), "unknown_7": body.unknown_7()}),
        CommandKind::C053(body) => serde_json::json!({"magic": "C053", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "bind_id": body.bind_id(), "sound_id": body.sound_id(), "unknown_3": body.unknown_3(), "flags": body.flags()}),
        CommandKind::C055(body) => serde_json::json!({"magic": "C055", "duration": body.duration(), "unknown_1": body.unknown_1(), "enabled": body.enabled(), "unknown_3": body.unknown_3()}),
        CommandKind::C056(body) => serde_json::json!({"magic": "C056", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2()}),
        CommandKind::C057(body) => serde_json::json!({"magic": "C057", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2()}),
        CommandKind::C058(body) => serde_json::json!({"magic": "C058", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C059(body) => serde_json::json!({"magic": "C059", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2()}),
        CommandKind::C063(body) => serde_json::json!({"magic": "C063", "loop_duration": body.loop_duration(), "unknown_1": body.unknown_1(), "path": body.path(), "sound_index": body.sound_index(), "position_flags": body.position_flags(), "bind_id": body.bind_id(), "unknown_2": body.unknown_2()}),
        CommandKind::C067(body) => serde_json::json!({"magic": "C067", "enabled": body.enabled(), "unknown_2": body.unknown_2()}),
        CommandKind::C068(body) => serde_json::json!({"magic": "C068", "duration": body.duration(), "unknown_2": body.unknown_2(), "color_1": body.color_1(), "color_2": body.color_2()}),
        CommandKind::C075(body) => serde_json::json!({"magic": "C075", "enabled": body.enabled(), "unknown_1": body.unknown_1(), "shape": body.shape(), "scale": body.scale(), "rotation": body.rotation(), "position": body.position(), "rgba": body.rgba(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4()}),
        CommandKind::C082(body) => serde_json::json!({"magic": "C082", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C083(body) => serde_json::json!({"magic": "C083", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C084(body) => serde_json::json!({"magic": "C084", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C088(body) => serde_json::json!({"magic": "C088", "duration": body.duration(), "unknown_2": body.unknown_2()}),
        CommandKind::C089(body) => serde_json::json!({"magic": "C089", "duration": body.duration(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C090(body) => serde_json::json!({"magic": "C090", "enabled": body.enabled(), "unknown_1": body.unknown_1(), "motion": body.motion(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C093(body) => serde_json::json!({"magic": "C093", "duration": body.duration(), "unknown_1": body.unknown_1(), "color_1": body.color_1(), "color_2": body.color_2(), "unknown_4": body.unknown_4()}),
        CommandKind::C094(body) => serde_json::json!({"magic": "C094", "fade_time": body.fade_time(), "unknown_1": body.unknown_1(), "start_visibility": body.start_visibility(), "end_visibility": body.end_visibility(), "filter": body.filter().map(|filter| serde_json::json!({"enable": filter.enable(), "filter": filter.filter(), "unknown_4": filter.unknown_4(), "unknown_5": filter.unknown_5(), "unknown_6": filter.unknown_6()}))}),
        CommandKind::C095(body) => serde_json::json!({"magic": "C095", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5()}),
        CommandKind::C100(body) => serde_json::json!({"magic": "C100", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "visibility": body.visibility(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5()}),
        CommandKind::C104(body) => serde_json::json!({"magic": "C104", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2()}),
        CommandKind::C107(body) => serde_json::json!({"magic": "C107", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "trigger_row": body.trigger_row(), "unknown_4": body.unknown_4()}),
        CommandKind::C109(body) => serde_json::json!({"magic": "C109", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2()}),
        CommandKind::C110(body) => serde_json::json!({"magic": "C110", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2()}),
        CommandKind::C112(body) => serde_json::json!({"magic": "C112", "duration": body.duration(), "unknown_1": body.unknown_1(), "color": body.color()}),
        CommandKind::C113(body) => serde_json::json!({"magic": "C113", "duration": body.duration(), "unknown_1": body.unknown_1(), "color": body.color()}),
        CommandKind::C114(body) => serde_json::json!({"magic": "C114", "enabled": body.enabled(), "unknown_1": body.unknown_1(), "samples": body.samples(), "pass": body.pass(), "passes": body.passes(), "step": body.step(), "until": body.until()}),
        CommandKind::C117(body) => serde_json::json!({"magic": "C117", "duration": body.duration(), "unknown_2": body.unknown_2(), "curve_id": body.curve_id()}),
        CommandKind::C118(body) => serde_json::json!({"magic": "C118", "transition_time": body.transition_time(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C120(body) => serde_json::json!({"magic": "C120", "duration": body.duration(), "unknown_2": body.unknown_2(), "wave_type": body.wave_type()}),
        CommandKind::C124(body) => serde_json::json!({"magic": "C124", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "targetable": body.targetable()}),
        CommandKind::C125(body) => serde_json::json!({"magic": "C125", "duration": body.duration(), "unknown_1": body.unknown_1()}),
        CommandKind::C131(body) => serde_json::json!({"magic": "C131", "enabled": body.enabled(), "unknown_2": body.unknown_2()}),
        CommandKind::C133(body) => serde_json::json!({"magic": "C133", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C136(body) => serde_json::json!({"magic": "C136", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2()}),
        CommandKind::C139(body) => serde_json::json!({"magic": "C139", "enabled": body.enabled(), "unknown_2": body.unknown_2()}),
        CommandKind::C142(body) => serde_json::json!({"magic": "C142", "duration": body.duration(), "unknown_2": body.unknown_2(), "position": body.position(), "freeze_location": body.freeze_location()}),
        CommandKind::C143(body) => serde_json::json!({"magic": "C143", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "bank_id": body.bank_id()}),
        CommandKind::C144(body) => serde_json::json!({"magic": "C144", "duration": body.duration(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "camera": body.camera(), "nameplate": body.nameplate()}),
        CommandKind::C161(body) => serde_json::json!({"magic": "C161", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "blink": body.blink(), "unknown_4": body.unknown_4()}),
        CommandKind::C168(body) => serde_json::json!({"magic": "C168", "duration": body.duration(), "unknown_2": body.unknown_2(), "curve_id": body.curve_id(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6(), "unknown_7": body.unknown_7(), "unknown_8": body.unknown_8(), "unknown_9": body.unknown_9(), "unknown_10": body.unknown_10(), "unknown_11": body.unknown_11()}),
        CommandKind::C173(body) => serde_json::json!({"magic": "C173", "loop_wait": body.loop_wait(), "unknown_2": body.unknown_2(), "path": body.path(), "bind_origin_1": body.bind_origin_1(), "bind_type_1": body.bind_type_1(), "bind_id_1": body.bind_id_1(), "visibility": body.visibility(), "limit": body.limit(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6(), "unknown_7": body.unknown_7(), "unknown_8": body.unknown_8(), "unknown_9": body.unknown_9(), "unknown_10": body.unknown_10(), "unknown_11": body.unknown_11(), "unknown_12": body.unknown_12()}),
        CommandKind::C174(body) => serde_json::json!({"magic": "C174", "duration": body.duration(), "unknown_2": body.unknown_2(), "object_position": body.object_position(), "object_control": body.object_control(), "final_position": body.final_position(), "position_delay": body.position_delay(), "unknown_6": body.unknown_6()}),
        CommandKind::C175(body) => serde_json::json!({"magic": "C175", "duration": body.duration(), "unknown_2": body.unknown_2(), "object_scale": body.object_scale(), "object_control": body.object_control(), "final_scale": body.final_scale(), "scale_delay": body.scale_delay(), "unknown_7": body.unknown_7()}),
        CommandKind::C176(body) => serde_json::json!({"magic": "C176", "duration": body.duration(), "unknown_2": body.unknown_2(), "curve_id": body.curve_id(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6(), "unknown_7": body.unknown_7()}),
        CommandKind::C177(body) => serde_json::json!({"magic": "C177", "duration": body.duration(), "unknown_2": body.unknown_2(), "curve_id": body.curve_id(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6()}),
        CommandKind::C178(body) => serde_json::json!({"magic": "C178", "duration": body.duration(), "unknown_2": body.unknown_2(), "curve_id": body.curve_id(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6()}),
        CommandKind::C187(body) => serde_json::json!({"magic": "C187", "duration": body.duration(), "unknown_1": body.unknown_1(), "part": body.part(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C188(body) => serde_json::json!({"magic": "C188", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2()}),
        CommandKind::C192(body) => serde_json::json!({"magic": "C192", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6(), "unknown_7": body.unknown_7(), "unknown_8": body.unknown_8(), "unknown_9": body.unknown_9(), "unknown_10": body.unknown_10(), "unknown_11": body.unknown_11()}),
        CommandKind::C194(body) => serde_json::json!({"magic": "C194", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4()}),
        CommandKind::C197(body) => serde_json::json!({"magic": "C197", "fade_time": body.fade_time(), "unknown_2": body.unknown_2(), "voiceline_number": body.voiceline_number(), "bind_point_id": body.bind_point_id(), "speak_type": body.speak_type(), "unknown_6": body.unknown_6()}),
        CommandKind::C198(body) => serde_json::json!({"magic": "C198", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "summon_id": body.summon_id(), "atch_state": body.atch_state(), "unknown_4": body.unknown_4(), "model_id": body.model_id(), "body_id": body.body_id(), "variant": body.variant()}),
        CommandKind::C199(body) => serde_json::json!({"magic": "C199", "enabled": body.enabled(), "unknown_1": body.unknown_1(), "bind_point_id": body.bind_point_id(), "unknown_2": body.unknown_2(), "object_control": body.object_control()}),
        CommandKind::C202(body) => serde_json::json!({"magic": "C202", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6()}),
        CommandKind::C203(body) => serde_json::json!({"magic": "C203", "duration": body.duration(), "unknown_2": body.unknown_2(), "bind_point_id": body.bind_point_id(), "rotation": body.rotation(), "object_control": body.object_control(), "no_follow": body.no_follow(), "scale_enabled": body.scale_enabled(), "unknown_3": body.unknown_3(), "scale": body.scale()}),
        CommandKind::C204(body) => serde_json::json!({"magic": "C204", "duration": body.duration(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4()}),
        CommandKind::C211(body) => serde_json::json!({"magic": "C211", "duration": body.duration(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4()}),
        CommandKind::C212(body) => serde_json::json!({"magic": "C212", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5()}),
        CommandKind::C215(body) => serde_json::json!({"magic": "C215", "duration": body.duration(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4()}),
        CommandKind::C216(body) => serde_json::json!({"magic": "C216", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "subtitle_type": body.subtitle_type(), "text_id": body.text_id(), "speaker_id": body.speaker_id(), "duration": body.duration(), "unknown_7": body.unknown_7(), "unknown_8": body.unknown_8(), "unknown_9": body.unknown_9()}),
        CommandKind::C225(body) => serde_json::json!({"magic": "C225", "duration": body.duration(), "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3()}),
        CommandKind::C230(body) => serde_json::json!({"magic": "C230", "enabled": body.enabled(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "bgm_id": body.bgm_id(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6(), "unknown_7": body.unknown_7()}),
        CommandKind::C234(body) => serde_json::json!({"magic": "C234", "unknown_1": body.unknown_1(), "unknown_2": body.unknown_2(), "unknown_3": body.unknown_3(), "unknown_4": body.unknown_4(), "unknown_5": body.unknown_5(), "unknown_6": body.unknown_6()}),
        CommandKind::Unknown { magic, body } => serde_json::json!({"magic": String::from_utf8_lossy(magic), "unknown": true, "bytes": body.len(), "hex": body.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" ")}),
    }
}

fn tmb_item_json(item: &ironworks::file::tmb::Item) -> serde_json::Value {
    use ironworks::file::tmb::Item;

    match item {
        Item::Header(header) => serde_json::json!({
            "magic": "TMDH",
            "kind": "Header",
            "id": header.id(),
            "unknown_1": header.unknown_1(),
            "duration": header.duration(),
            "unknown_3": header.unknown_3()
        }),
        Item::FaceLibrary(library) => serde_json::json!({
            "magic": "TMPP",
            "kind": "FaceLibrary",
            "path": library.path()
        }),
        Item::ActorList(list) => serde_json::json!({
            "magic": "TMAL",
            "kind": "ActorList",
            "actors": list.actors()
        }),
        Item::Actor(actor) => serde_json::json!({
            "magic": "TMAC",
            "kind": "Actor",
            "id": actor.id(),
            "time": actor.time(),
            "ability_delay": actor.ability_delay(),
            "participant": actor.participant(),
            "tracks": actor.tracks()
        }),
        Item::Track(track) => serde_json::json!({
            "magic": "TMTR",
            "kind": "Track",
            "id": track.id(),
            "time": track.time(),
            "commands": track.commands(),
            "condition": track.condition().iter().map(tmb_condition_json).collect::<Vec<_>>()
        }),
        Item::Curves(curves) => serde_json::json!({
            "magic": "TMFC",
            "kind": "Curves",
            "id": curves.id(),
            "time": curves.time(),
            "targets": curves.targets(),
            "end": curves.end(),
            "unknown_b": curves.unknown_b(),
            "curves": curves.curves().iter().map(tmb_curve_json).collect::<Vec<_>>()
        }),
        Item::Command(command) => serde_json::json!({
            "magic": tmb_command_kind_json(command.kind())["magic"],
            "kind": "Command",
            "id": command.id(),
            "time": command.time(),
            "command": tmb_command_kind_json(command.kind())
        }),
        Item::Unknown(unknown) => serde_json::json!({
            "magic": String::from_utf8_lossy(&unknown.magic()),
            "kind": "Unknown",
            "bytes": unknown.body().len(),
            "hex": unknown.body().iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" ")
        }),
    }
}

fn inspect_cutb(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    use ironworks::file::cutb::{Cutscene, Node};

    let file = Cutscene::read(Cursor::new(bytes.to_vec()))?;
    let mut nodes = Vec::new();
    let mut shots = 0usize;
    for (index, node) in file.nodes().iter().enumerate() {
        let node_json = match node {
            Node::Resources(list) => serde_json::json!({
                "magic": "CTRL",
                "kind": "Resources",
                "resources": list.iter().take(max_items).map(|resource| serde_json::json!({
                    "path": resource.path(),
                    "flag": resource.unknown_1()
                })).collect::<Vec<_>>(),
                "truncated": list.len() > max_items
            }),
            Node::Sheet(sheet) => serde_json::json!({
                "magic": "CTIS",
                "kind": "Sheet",
                "sheet": sheet
            }),
            Node::Scene(scene) => {
                let entries = scene.entries();
                serde_json::json!({
                    "magic": "CTDS",
                    "kind": "Scene",
                    "level": scene.level(),
                    "origin": scene.origin(),
                    "entries": entries.iter().take(max_items).collect::<Vec<_>>(),
                    "truncated": entries.len() > max_items
                })
            }
            Node::Participants(participants) => serde_json::json!({
                "magic": "CTAL",
                "kind": "Participants",
                "participants": participants.iter().take(max_items).map(layer_instance_json).collect::<Vec<_>>(),
                "truncated": participants.len() > max_items
            }),
            Node::Groups(groups) => serde_json::json!({
                "magic": "CTPA",
                "kind": "Groups",
                "groups": groups.iter().take(max_items).map(|group| {
                    let records = group.records();
                    serde_json::json!({
                        "id": group.id(),
                        "record_count": records.len(),
                        "truncated": records.len() > max_items,
                        "records": records.iter().take(max_items).map(|record| serde_json::json!({
                            "bytes": record.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" ")
                        })).collect::<Vec<_>>()
                    })
                }).collect::<Vec<_>>(),
                "truncated": groups.len() > max_items
            }),
            Node::Tracks(tracks) => serde_json::json!({
                "magic": "CTEX",
                "kind": "Tracks",
                "tracks": tracks.iter().take(max_items).map(|track| serde_json::json!({
                    "lane": track.lane(),
                    "id": track.id(),
                    "unknown_1": track.unknown_1(),
                    "unknown_2": track.unknown_2(),
                    "values": track.values(),
                    "value_count": track.values().len(),
                    "truncated": track.values().len() > max_items
                })).collect::<Vec<_>>(),
                "truncated": tracks.len() > max_items
            }),
            Node::Timeline(timeline) => {
                shots += timeline
                    .items()
                    .iter()
                    .filter(|item| {
                        matches!(item, ironworks::file::tmb::Item::Command(command)
                            if matches!(command.kind(), ironworks::file::tmb::CommandKind::C004(_)))
                    })
                    .count();
                let items = timeline.items();
                serde_json::json!({
                    "magic": "CTTL",
                    "kind": "Timeline",
                    "item_count": items.len(),
                    "truncated": items.len() > max_items,
                    "items": items.iter().take(max_items).map(tmb_item_json).collect::<Vec<_>>()
                })
            }
            Node::Unknown(unknown) => {
                let body = unknown.body();
                serde_json::json!({
                    "magic": String::from_utf8_lossy(&unknown.magic()),
                    "kind": "Unknown",
                    "bytes": body.len(),
                    "hex": body.iter().take(max_items * 16).map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" ")
                })
            }
        };
        nodes.push(serde_json::json!({
            "index": index,
            "node": node_json
        }));
    }
    Ok(serde_json::json!({
        "node_count": file.nodes().len(),
        "truncated": file.nodes().len() > max_items,
        "shots": shots,
        "nodes": nodes
    }))
}

fn inspect_tmb(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    use ironworks::file::tmb::Timeline;

    let timeline = Timeline::read(Cursor::new(bytes.to_vec()))?;
    let items = timeline.items();
    let duration = items.iter().find_map(|item| match item {
        ironworks::file::tmb::Item::Header(header) => Some(header.duration()),
        _ => None,
    });
    let mut counts: Vec<(String, usize)> = Vec::new();
    for item in items {
        let magic = tmb_item_json(item)["magic"].as_str().unwrap_or_default().to_owned();
        match counts.iter_mut().find(|(at, _)| *at == magic) {
            Some((_, count)) => *count += 1,
            None => counts.push((magic, 1)),
        }
    }
    let item_list = items.iter().take(max_items).map(tmb_item_json).collect::<Vec<_>>();
    Ok(serde_json::json!({
        "items": items.len(),
        "duration": duration,
        "kinds": counts
            .iter()
            .take(max_items)
            .map(|(magic, count)| serde_json::json!({"magic": magic, "count": count}))
            .collect::<Vec<_>>(),
        "kinds_truncated": counts.len() >= max_items,
        "item_list": item_list,
        "truncated": items.len() > max_items
    }))
}

pub fn inspect(path: &str, bytes: &[u8], max_items: usize) -> anyhow::Result<String> {
    let max_items = max_items.min(MAX_ITEMS);
    let format = magic::sniff(bytes);
    let label = format.map(|format| format.label());
    let viewer = format.map(|format| format.viewer());
    let details = match viewer {
        Some(crate::assets::viewers::Viewer::Texture) => inspect_texture(bytes),
        Some(crate::assets::viewers::Viewer::Image) => inspect_image(bytes),
        Some(crate::assets::viewers::Viewer::Material) => inspect_material(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Font) => inspect_font(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Icons) => inspect_icons(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Uld) => inspect_uld(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Shpk) => inspect_shpk(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Shcd) => inspect_shcd(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Scd) => inspect_scd(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Lgb) => inspect_lgb(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Sgb) => inspect_sgb(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Cutb) => inspect_cutb(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Tmb) => inspect_tmb(bytes, max_items),
        Some(crate::assets::viewers::Viewer::Text) => Ok(serde_json::json!({
            "text": String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BYTES)]),
            "truncated": bytes.len() > MAX_BYTES
        })),
        _ => Ok(serde_json::Value::Null),
    }?;
    Ok(serde_json::json!({
        "path": path,
        "size": bytes.len(),
        "format": label.map(|label| serde_json::json!({"label": label, "viewer": viewer.map(|viewer| viewer.label())})).unwrap_or(serde_json::Value::Null),
        "details": details
    })
    .to_string())
}
