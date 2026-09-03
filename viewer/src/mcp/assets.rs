use std::{cell::RefCell, io::Cursor};

use base64::{Engine, prelude::BASE64_STANDARD};
use image::GenericImageView;
use ironworks::file::File;
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

pub async fn resolve_path(backend: &Backend, path: &str) -> anyhow::Result<String> {
    let exists = backend
        .files()
        .exists_many(&[path.to_owned()])
        .await?
        .first()
        .copied()
        .unwrap_or(false);
    Ok(
        serde_json::json!({"path": path, "exists": exists, "hashes": path_hashes(path)})
            .to_string(),
    )
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
    Ok(serde_json::json!({
        "width": width,
        "height": height,
        "color_type": format!("{:?}", image.color())
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
    Ok(serde_json::json!({
        "texture_size": [font.texture_width(), font.texture_height()],
        "size": font.size(),
        "line_height": font.line_height(),
        "ascent": font.ascent(),
        "descent": font.descent(),
        "glyph_count": font.glyphs().len(),
        "kerning_count": font.kerning().len(),
        "glyphs": font.glyphs().iter().take(max_items).map(|glyph| serde_json::json!({"character": glyph.character().to_string(), "codepoint": u32::from(glyph.character()), "x": glyph.x(), "y": glyph.y(), "width": glyph.width(), "height": glyph.height(), "texture_file": glyph.texture_file(), "texture_channel": glyph.texture_channel(), "offset_y": glyph.offset_y(), "advance": glyph.advance_width()})).collect::<Vec<_>>()
    }))
}

fn inspect_icons(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let icons = ironworks::file::gfd::FontIcons::read(Cursor::new(bytes.to_vec()))?;
    Ok(serde_json::json!({
        "count": icons.icons().len(),
        "icons": icons.icons().iter().take(max_items).map(|icon| serde_json::json!({"id": icon.id(), "left": icon.left(), "top": icon.top(), "width": icon.width(), "height": icon.height(), "redirect": icon.redirect(), "resolved_id": icons.icon(icon.id()).map(|resolved| resolved.id())})).collect::<Vec<_>>()
    }))
}

fn inspect_uld(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let layout = ironworks::file::uld::UiLayout::read(Cursor::new(bytes.to_vec()))?;
    let part_lists = layout
        .part_lists()
        .iter()
        .take(max_items)
        .map(|list| serde_json::json!({"id": list.id(), "part_count": list.parts().len()}))
        .collect::<Vec<_>>();
    let mut parts = Vec::with_capacity(max_items);
    for list in layout.part_lists() {
        for (index, part) in list.parts().iter().enumerate() {
            if parts.len() == max_items {
                break;
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
        if parts.len() == max_items {
            break;
        }
    }
    let timelines = layout
        .timelines()
        .iter()
        .take(max_items)
        .map(|timeline| {
            serde_json::json!({
                "id": timeline.id(),
                "animation_count": timeline.animations().len(),
                "label_set_count": timeline.label_sets().len()
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
    Ok(serde_json::json!({
        "version": format!("{:?}", layout.version()),
        "textures": layout.textures().iter().take(max_items).map(|texture| serde_json::json!({"id": texture.id(), "path": texture.path(), "icon_id": texture.icon_id(), "theme_bitmask": texture.theme_bitmask()})).collect::<Vec<_>>(),
        "texture_count": layout.textures().len(),
        "part_lists": part_lists,
        "part_list_count": layout.part_lists().len(),
        "parts": parts,
        "components": layout.components().iter().take(max_items).map(|component| serde_json::json!({"id": component.id(), "kind": format!("{:?}", component.kind()), "node_count": component.nodes().len()})).collect::<Vec<_>>(),
        "component_count": layout.components().len(),
        "timelines": timelines,
        "timeline_count": layout.timelines().len(),
        "animations": animations,
        "widgets": layout.widgets().iter().take(max_items).map(|widget| serde_json::json!({"id": widget.id(), "alignment": format!("{:?}", widget.alignment()), "themed_assets": widget.themed_assets(), "x": widget.x(), "y": widget.y(), "node_count": widget.nodes().len()})).collect::<Vec<_>>(),
        "widget_count": layout.widgets().len()
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
    Ok(serde_json::json!({
        "version": code.version(),
        "stage": format!("{:?}", code.stage()),
        "directx": format!("{:?}", code.directx()),
        "blob_offset": code.blob_offset(),
        "blob_size": code.blob_size(),
        "resources": code.resources().iter().take(max_items).map(|resource| resource_json(resource, code.name(resource))).collect::<Vec<_>>(),
        "constant_count": code.constants().len(),
        "sampler_count": code.samplers().len(),
        "texture_count": code.textures().len(),
        "uav_count": code.uavs().len()
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

fn inspect_sgb(bytes: &[u8], max_items: usize) -> anyhow::Result<serde_json::Value> {
    let file = ironworks::file::sgb::SharedGroupFile::read(Cursor::new(bytes.to_vec()))?;
    let scene = file.scene();
    let layers = scene
        .layer_groups()
        .iter()
        .take(max_items)
        .map(|group| {
            serde_json::json!({
                "id": group.id(),
                "name": group.name(),
                "layer_count": group.layers().len(),
                "layers": group.layers().iter().take(max_items).map(|layer| {
                    serde_json::json!({
                        "id": layer.id(),
                        "name": layer.name(),
                        "visible": layer.visible(),
                        "instance_count": layer.instances().len()
                    })
                }).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "group_count": scene.layer_groups().len(),
        "environment_count": scene.environments().len(),
        "filter_count": scene.filters().len(),
        "timeline_count": scene.timelines().len(),
        "animation_count": scene.animations().len(),
        "truncated": scene.layer_groups().len() > max_items,
        "groups": layers
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
                "instances": layer.instances().iter().take(max_items).map(|instance| {
                    let data = instance.data();
                    serde_json::json!({
                        "kind": format!("{:?}", instance.kind()),
                        "id": instance.id(),
                        "name": instance.name(),
                        "data": match data {
                            ironworks::file::layer::InstanceData::SharedGroup(group) => serde_json::json!({"shared_group": group.asset_path()}),
                            ironworks::file::layer::InstanceData::BgPart(part) => serde_json::json!({"bg_part": part.asset_path()}),
                            ironworks::file::layer::InstanceData::Vfx(vfx) => serde_json::json!({"vfx": vfx.asset_path()}),
                            ironworks::file::layer::InstanceData::Sound(sound) => serde_json::json!({"sound": sound.asset_path()}),
                            _ => serde_json::Value::Null
                        }
                    })
                }).collect::<Vec<_>>()
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
