//! Dumps a `.uld` widget's node tree: position, size, and a text node's sheet row and colors.
//!
//! `uld_box <uld path>`

use ironworks::file::uld::{NodeKind, UiLayout};
use ironworks::file::File;
use ironworks::{
    sqpack::{Install, SqPack},
    Ironworks,
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn color(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    let path = std::env::args().nth(1).expect("a uld path");
    let bytes = ironworks.file::<Vec<u8>>(&path).expect("the layout");
    let layout = UiLayout::read(std::io::Cursor::new(bytes)).expect("a layout");

    println!("{path}");
    for widget in layout.widgets() {
        println!("widget {}", widget.id());
        for root in widget.roots() {
            dump(&layout, widget, root, 0);
        }
    }
    for component in layout.components() {
        println!("component {}", component.id());
    }
}

fn dump(
    layout: &UiLayout,
    widget: &ironworks::file::uld::Widget,
    node: &ironworks::file::uld::Node,
    depth: usize,
) {
    let indent = "  ".repeat(depth + 1);
    print!(
        "{indent}#{} {:+}/{:+} {}x{} a={} vis={:#x}",
        node.id(),
        node.x(),
        node.y(),
        node.width(),
        node.height(),
        node.alpha(),
        node.flags().bits(),
    );
    match node.kind() {
        NodeKind::Text(text) => {
            let fill = color(text.color);
            let edge = color(text.edge_color);
            println!(
                " TEXT sheet={} row={} align={} font={:?} size={} flags2={:#x} fill={fill:?} edge={edge:?}",
                text.sheet_type, text.text_id, text.alignment, text.font, text.font_size, text.flags2,
            );
        }
        NodeKind::NineGrid(nine) => println!(" NINEGRID {nine:?}"),
        NodeKind::Image(image) => println!(" IMAGE {image:?}"),
        NodeKind::Res => println!(" RES"),
        NodeKind::Component { component_id, .. } => {
            println!(" COMPONENT {component_id}");
            if let Some(component) = layout.component(*component_id) {
                println!(
                    "{indent}  [component {} kind {:?}]",
                    component.id(),
                    component.kind()
                );
                for croot in component.roots() {
                    dump_component(layout, component, croot, depth + 2);
                }
            }
        }
        other => println!(" {other:?}"),
    }
    for child in widget.children(node.id()) {
        dump(layout, widget, child, depth + 1);
    }
}

fn dump_component(
    layout: &UiLayout,
    component: &ironworks::file::uld::Component,
    node: &ironworks::file::uld::Node,
    depth: usize,
) {
    let indent = "  ".repeat(depth + 1);
    print!(
        "{indent}#{} {:+}/{:+} {}x{} a={} vis={:#x}",
        node.id(),
        node.x(),
        node.y(),
        node.width(),
        node.height(),
        node.alpha(),
        node.flags().bits(),
    );
    match node.kind() {
        NodeKind::Text(text) => println!(
            " TEXT sheet={} row={} align={} font={:?} size={} flags2={:#x} fill={:?} edge={:?}",
            text.sheet_type,
            text.text_id,
            text.alignment,
            text.font,
            text.font_size,
            text.flags2,
            color(text.color),
            color(text.edge_color),
        ),
        NodeKind::NineGrid(nine) => println!(" NINEGRID {nine:?}"),
        NodeKind::Image(image) => println!(" IMAGE {image:?}"),
        NodeKind::Res => println!(" RES"),
        NodeKind::Component { component_id, .. } => {
            println!(" COMPONENT {component_id}");
            if let Some(inner) = layout.component(*component_id) {
                for croot in inner.roots() {
                    dump_component(layout, inner, croot, depth + 1);
                }
            }
        }
        other => println!(" {other:?}"),
    }
    for child in component.children(node.id()) {
        dump_component(layout, component, child, depth + 1);
    }
}
