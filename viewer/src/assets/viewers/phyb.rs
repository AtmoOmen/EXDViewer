//! `.phyb` physics: the shapes a skeleton's bones collide with, and the simulators that drive
//! chains of bones under gravity and wind.

use std::io::Cursor;

use anyhow::Result;
use egui::{RichText, ScrollArea, vec2};
use ironworks::file::File;
use ironworks::file::phyb::{Chain, Collision, Name, Physics, Simulator};

use super::{Preview, facts, headers, heading, section};
use crate::assets::Bytes;

/// A name as written, where the game leaves uninitialized bytes past the terminator and a handful
/// of names are Shift-JIS.
fn named(name: Name) -> String {
    name.as_str().unwrap_or("?").to_owned()
}

fn axes(values: [f32; 3]) -> String {
    format!("{:.3}, {:.3}, {:.3}", values[0], values[1], values[2])
}

/// The switches a simulator declares, in flag order.
fn flags(simulator: &Simulator) -> String {
    let flags = simulator.flags();
    let listed = [
        (flags.simulating(), "simulating"),
        (flags.collisions_handled(), "collisions"),
        (flags.continuous_collisions(), "continuous"),
        (flags.using_ground_plane(), "ground plane"),
        (flags.fixed_length(), "fixed length"),
    ]
    .iter()
    .filter(|(set, _)| *set)
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join(", ");
    match listed.is_empty() {
        true => format!("{:#04x}", flags.bits()),
        false => listed,
    }
}

/// A striped table of strings, which every list here is.
fn rows(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    names: &[&str],
    rows: &[Vec<String>],
) {
    egui::Grid::new(id)
        .num_columns(names.len())
        .striped(true)
        .show(ui, |ui| {
            headers(ui, names);
            for row in rows {
                for cell in row {
                    ui.label(RichText::new(cell).monospace());
                }
                ui.allocate_space(vec2(ui.available_width(), 0.0));
                ui.end_row();
            }
        });
}

/// Every collision shape as one table, since a shape is a name, the bones it hangs off and a size.
fn shapes(collision: &Collision) -> Vec<Vec<String>> {
    let capsules = collision.capsules().iter().map(|shape| {
        vec![
            "capsule".to_owned(),
            named(shape.name()),
            format!("{}, {}", named(shape.start_bone()), named(shape.end_bone())),
            format!(
                "{} / {}",
                axes(shape.start_offset()),
                axes(shape.end_offset())
            ),
            format!("radius {:.3}", shape.radius()),
        ]
    });
    let ellipsoids = collision.ellipsoids().iter().map(|shape| {
        vec![
            "ellipsoid".to_owned(),
            named(shape.name()),
            named(shape.bone()),
            shape
                .offsets()
                .iter()
                .map(|offset| axes(*offset))
                .collect::<Vec<_>>()
                .join(" / "),
            format!("radius {:.3}", shape.radius()),
        ]
    });
    let normals = collision.normal_planes().iter().map(|shape| {
        vec![
            "plane".to_owned(),
            named(shape.name()),
            named(shape.bone()),
            format!(
                "{}, normal {}",
                axes(shape.bone_offset()),
                axes(shape.normal())
            ),
            format!("thickness {:.3}", shape.thickness()),
        ]
    });
    let three_point = collision.three_point_planes().iter().map(|shape| {
        vec![
            "3-point plane".to_owned(),
            named(shape.name()),
            named(shape.bone()),
            format!(
                "{}, unknown {} / {}",
                axes(shape.bone_offset()),
                axes(shape.unknown_b()),
                axes(shape.unknown_c())
            ),
            format!("thickness {:.3}", shape.thickness()),
        ]
    });
    let spheres = collision.spheres().iter().map(|shape| {
        vec![
            "sphere".to_owned(),
            named(shape.name()),
            named(shape.bone()),
            axes(shape.bone_offset()),
            format!("thickness {:.3}", shape.thickness()),
        ]
    });

    capsules
        .chain(ellipsoids)
        .chain(normals)
        .chain(three_point)
        .chain(spheres)
        .collect()
}

fn chain_ui(ui: &mut egui::Ui, simulator: usize, index: usize, chain: &Chain) {
    heading(
        ui,
        &format!(
            "Chain {index}: {:?}, {} nodes",
            chain.chain_type(),
            chain.nodes().len()
        ),
    );
    ui.label(
        RichText::new(format!(
            "dampening {:.3}, max speed {:.3}, friction {:.3}, collision dampening {:.3}, \
             repulsion {:.3}, end {}",
            chain.dampening(),
            chain.max_speed(),
            chain.friction(),
            chain.collision_dampening(),
            chain.repulsion_strength(),
            axes(chain.last_bone_offset())
        ))
        .monospace()
        .weak(),
    );

    if !chain.collisions().is_empty() {
        rows(
            ui,
            ("phyb_chain_collisions", simulator, index),
            &["Shape", "Side"],
            &chain
                .collisions()
                .iter()
                .map(|collision| {
                    vec![
                        named(collision.name()),
                        format!("{:?}", collision.collision_type()),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    rows(
        ui,
        ("phyb_nodes", simulator, index),
        &[
            "Bone",
            "Radius",
            "Attract",
            "Wind",
            "Gravity",
            "Cone",
            "Axis offset",
            "Plane normal",
            "Collision",
        ],
        &chain
            .nodes()
            .iter()
            .map(|node| {
                vec![
                    named(node.bone()),
                    format!("{:.3}", node.radius()),
                    format!("{:.3}", node.attract_by_animation()),
                    format!("{:.3}", node.wind_scale()),
                    format!("{:.3}", node.gravity_scale()),
                    format!("{:.3}", node.cone_max_angle()),
                    axes(node.cone_axis_offset()),
                    axes(node.constraint_plane_normal()),
                    format!(
                        "{:#010x} / {:#010x}",
                        node.collision_flags(),
                        node.continuous_collision_flags()
                    ),
                ]
            })
            .collect::<Vec<_>>(),
    );
}

fn simulator_ui(ui: &mut egui::Ui, index: usize, simulator: &Simulator) {
    ui.add_space(8.0);
    ui.separator();
    section(ui, &format!("Simulator {index}"));
    ui.label(
        RichText::new(format!(
            "gravity {}, wind {}, constraint loop {}, collision loop {}, group {}, {}",
            axes(simulator.gravity()),
            axes(simulator.wind()),
            simulator.constraint_loop(),
            simulator.collision_loop(),
            simulator.group(),
            flags(simulator)
        ))
        .monospace()
        .weak(),
    );

    for (kind, objects) in [
        ("Collision objects", simulator.collision_objects()),
        ("Connector collision", simulator.collision_connectors()),
    ] {
        if !objects.is_empty() {
            heading(ui, kind);
            rows(
                ui,
                ("phyb_collision", index, kind),
                &["Shape", "Side"],
                &objects
                    .iter()
                    .map(|collision| {
                        vec![
                            named(collision.name()),
                            format!("{:?}", collision.collision_type()),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }

    for (at, chain) in simulator.chains().iter().enumerate() {
        chain_ui(ui, index, at, chain);
    }

    if !simulator.connectors().is_empty() {
        heading(ui, "Connectors");
        rows(
            ui,
            ("phyb_connectors", index),
            &[
                "Chains",
                "Nodes",
                "Radius",
                "Friction",
                "Dampening",
                "Repulsion",
            ],
            &simulator
                .connectors()
                .iter()
                .map(|connector| {
                    vec![
                        format!("{:?}", connector.chain_ids()),
                        format!("{:?}", connector.node_ids()),
                        format!("{:.3}", connector.collision_radius()),
                        format!("{:.3}", connector.friction()),
                        format!("{:.3}", connector.dampening()),
                        format!("{:.3}", connector.repulsion()),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    if !simulator.attracts().is_empty() {
        heading(ui, "Attracts");
        rows(
            ui,
            ("phyb_attracts", index),
            &["Bone", "Offset", "Chain", "Node", "Stiffness"],
            &simulator
                .attracts()
                .iter()
                .map(|attract| {
                    vec![
                        named(attract.bone()),
                        axes(attract.bone_offset()),
                        attract.chain_id().to_string(),
                        attract.node_id().to_string(),
                        format!("{:.3}", attract.stiffness()),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    if !simulator.pins().is_empty() {
        heading(ui, "Pins");
        rows(
            ui,
            ("phyb_pins", index),
            &["Bone", "Offset", "Chain", "Node"],
            &simulator
                .pins()
                .iter()
                .map(|pin| {
                    vec![
                        named(pin.bone()),
                        axes(pin.bone_offset()),
                        pin.chain_id().to_string(),
                        pin.node_id().to_string(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    if !simulator.springs().is_empty() {
        heading(ui, "Springs");
        rows(
            ui,
            ("phyb_springs", index),
            &["Chains", "Nodes", "Stretch", "Compress"],
            &simulator
                .springs()
                .iter()
                .map(|spring| {
                    vec![
                        format!("{:?}", spring.chain_ids()),
                        format!("{:?}", spring.node_ids()),
                        format!("{:.3}", spring.stretch_stiffness()),
                        format!("{:.3}", spring.compress_stiffness()),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    if !simulator.post_alignments().is_empty() {
        heading(ui, "Post alignments");
        rows(
            ui,
            ("phyb_alignments", index),
            &["Shape", "Chain", "Node"],
            &simulator
                .post_alignments()
                .iter()
                .map(|alignment| {
                    vec![
                        named(alignment.collision_name()),
                        alignment.chain_id().to_string(),
                        alignment.node_id().to_string(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }
}

/// A physics file, decoded and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    file: Physics,
    shapes: Vec<Vec<String>>,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let file = Physics::read(Cursor::new(bytes.to_vec()))?;
    let shapes = file.collision().map(shapes).unwrap_or_default();

    let chains = file
        .simulators()
        .iter()
        .map(|simulator| simulator.chains().len())
        .sum::<usize>();
    let nodes = file
        .simulators()
        .iter()
        .flat_map(Simulator::chains)
        .map(|chain| chain.nodes().len())
        .sum::<usize>();

    let mut identity = vec![
        ("Version", format!("{:#010x}", file.version())),
        (
            "Data type",
            file.data_type()
                .map_or_else(|| "none".to_owned(), |kind| kind.to_string()),
        ),
        ("Collision shapes", shapes.len().to_string()),
        ("Simulators", file.simulators().len().to_string()),
        ("Chains", chains.to_string()),
        ("Nodes", nodes.to_string()),
    ];
    if let Some(extended) = file.extended() {
        identity.push(("Extended physics", Bytes(extended.len()).to_string()));
    }

    log::info!(
        "assets/phyb: {path} {} shapes, {} simulators, {chains} chains",
        shapes.len(),
        file.simulators().len()
    );

    Ok(Preview::Phyb(Box::new(Rendered {
        identity,
        file,
        shapes,
    })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        if !file.shapes.is_empty() {
            section(ui, "Collision");
            rows(
                ui,
                "phyb_shapes",
                &["Kind", "Name", "Bone", "Offset", "Size"],
                &file.shapes,
            );
        }
        for (index, simulator) in file.file.simulators().iter().enumerate() {
            simulator_ui(ui, index, simulator);
        }
    });
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| facts(ui, "phyb_identity", &self.identity));
    }
}
