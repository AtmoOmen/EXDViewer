//! A rig: the bones a skeleton names, the tree its parent indices make, and a pose drawn in space.
//!
//! Both `.sklb` and `.pap` end up here, one drawing the reference pose and the other whatever a
//! motion samples to. Bones are drawn as a marker at each joint and a stick back to its parent,
//! which is a rig rather than the character it moves.

use std::cell::Cell;
use std::collections::HashMap;

use egui::{RichText, Sense};
use glam::{Mat4, Quat, Vec3};
use ironworks::file::pap::Binding;
use ironworks::file::sklb::Transform;

use super::{line, placed, table};

/// Space one level of the tree sets its bones in by.
const INDENT: usize = 2;

/// Marker at a joint, and half the width of a bone, both as a fraction of the rig's own extent.
const JOINT: f32 = 0.012;
const BONE: f32 = 0.005;

const BONE_COLOR: [f32; 4] = [0.62, 0.66, 0.72, 1.0];
const JOINT_COLOR: [f32; 4] = [0.90, 0.62, 0.30, 1.0];
const PICKED_COLOR: [f32; 4] = [0.35, 0.85, 0.95, 1.0];

/// Where a bone ended up once every transform above it has been applied.
#[derive(Clone, Copy)]
pub struct Placement {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
}

impl Placement {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// This placement carried by another, which is how a rider hangs off the seat its mount names.
    pub fn carried(&self, by: &Self) -> Self {
        Self {
            translation: by.translation + by.rotation * (by.scale * self.translation),
            rotation: by.rotation * self.rotation,
            scale: by.scale * self.scale,
        }
    }

    pub fn translation(&self) -> Vec3 {
        self.translation
    }

    /// Scaled about its own origin, along its own axes, which is what a proportion slider does to
    /// the one pair of bones it names.
    pub fn scaled(&self, by: Vec3) -> Self {
        Self {
            scale: self.scale * by,
            ..*self
        }
    }
}

fn axes(values: [f32; 4], count: usize) -> String {
    values[..count]
        .iter()
        .map(|value| format!("{value:.3}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A skeleton's bones, ready to list and to draw.
pub struct Rig {
    names: Vec<String>,
    parents: Vec<i16>,
    reference: Vec<Transform>,
    /// Each bone and the depth it hangs at, ordered so a bone follows its parent.
    rows: Vec<(usize, usize)>,
    /// Widths the table pads its cells to, the first sized to the deepest name.
    columns: Vec<(&'static str, usize)>,
    /// How far across the rig reaches, which is what the markers are sized against.
    span: f32,
}

impl Rig {
    pub fn new(bones: &[String], parent_indices: &[i16], reference_pose: &[Transform]) -> Self {
        let names = bones.to_vec();
        let parents = parent_indices.to_vec();
        let reference = reference_pose.to_vec();

        let mut children: Vec<Vec<usize>> = vec![Vec::new(); names.len()];
        let mut roots = Vec::new();
        for bone in 0..names.len() {
            match parent_of(&parents, bone) {
                Some(parent) => children[parent].push(bone),
                None => roots.push(bone),
            }
        }

        // A bone is always written after its parent, so one pass down the list places every one.
        let mut rows = Vec::with_capacity(names.len());
        let mut depths = vec![0usize; names.len()];
        let mut stack = roots;
        stack.reverse();
        while let Some(bone) = stack.pop() {
            let depth = depths[bone];
            rows.push((bone, depth));
            for &child in children[bone].iter().rev() {
                depths[child] = depth + 1;
                stack.push(child);
            }
        }

        let widest = rows
            .iter()
            .map(|(bone, depth)| depth * INDENT + names[*bone].chars().count())
            .max()
            .unwrap_or(0);
        let columns = vec![
            ("Bone", widest + 2),
            ("Index", 6),
            ("Parent", 7),
            ("Translation", 26),
            ("Rotation", 34),
            ("Scale", 26),
        ];

        let mut rig = Self {
            names,
            parents,
            reference,
            rows,
            columns,
            span: 1.0,
        };
        let span = extent(&rig.world(&rig.reference));
        rig.span = span;
        rig
    }

    /// This rig with another skeleton's bones hung off the ones it already names. A name both
    /// carry stays this one's: an extra skeleton states the head where its own file put it rather
    /// than where the body's chain carries it. New bones are appended, so the indices a motion's
    /// tracks name still reach the bones they were authored against.
    pub fn merged(&self, names: &[String], parents: &[i16], reference: &[Transform]) -> Self {
        let mut held = self.names.clone();
        let mut hung = self.parents.clone();
        let mut rest = self.reference.clone();
        let mut at: HashMap<String, usize> = held
            .iter()
            .enumerate()
            .map(|(bone, name)| (name.clone(), bone))
            .collect();
        for (bone, name) in names.iter().enumerate() {
            if at.contains_key(name) {
                continue;
            }
            // A bone whose parent is nowhere to be found would stand at the world origin, which is
            // further from where it belongs than leaving it out entirely.
            let Some(parent) = parent_of(parents, bone).and_then(|parent| at.get(&names[parent]))
            else {
                continue;
            };
            hung.push(*parent as i16);
            rest.push(reference[bone]);
            at.insert(name.clone(), held.len());
            held.push(name.clone());
        }
        Self::new(&held, &hung, &rest)
    }

    pub fn bones(&self) -> usize {
        self.names.len()
    }

    /// What the skeleton calls each of its bones, which is how anything else names one.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn reference(&self) -> &[Transform] {
        &self.reference
    }

    /// Each bone's placement with everything above it applied, `locals` being one transform per
    /// bone in the skeleton's own order.
    pub fn world(&self, locals: &[Transform]) -> Vec<Placement> {
        let mut world: Vec<Placement> = Vec::with_capacity(self.names.len());
        for bone in 0..self.names.len() {
            let local = locals.get(bone).unwrap_or(&IDENTITY);
            let translation = Vec3::from_slice(&local.translation);
            let rotation = Quat::from_array(local.rotation);
            let scale = Vec3::from_slice(&local.scale);
            world.push(
                match parent_of(&self.parents, bone).map(|parent| world[parent]) {
                    Some(parent) => Placement {
                        translation: parent.translation
                            + parent.rotation * (parent.scale * translation),
                        rotation: parent.rotation * rotation,
                        scale: parent.scale * scale,
                    },
                    None => Placement {
                        translation,
                        rotation,
                        scale,
                    },
                },
            );
        }
        world
    }

    /// Where the bones end up with a motion's tracks sampled over the rest pose at `time`.
    ///
    /// A motion's tracks are in its own order, which is not the skeleton's, and a track may name a
    /// bone the skeleton does not have.
    pub fn posed(&self, binding: &Binding, time: f32) -> Vec<Placement> {
        let mut locals = self.reference.clone();
        for (track, transform) in binding.motion().sample(time).into_iter().enumerate() {
            if let Some(bone) = binding.bones().get(track).copied()
                && let Ok(bone) = usize::try_from(bone)
                && bone < locals.len()
            {
                locals[bone] = transform;
            }
        }
        self.world(&locals)
    }

    /// A marker at every joint and a stick from every bone back to its parent. The counts are fixed
    /// by the rig, so a bone of no length keeps its place as a stick of no size.
    pub fn batches(&self, world: &[Placement], picked: Option<usize>) -> Vec<placed::Batch> {
        let joint = (self.span * JOINT).max(f32::EPSILON);
        let bone = (self.span * BONE).max(f32::EPSILON);

        let mut instances = Vec::with_capacity(world.len() * 2);
        for (index, placement) in world.iter().enumerate() {
            instances.push(placed::Instance {
                center: placement.translation.to_array(),
                scale: [joint; 3],
                turn: placement.rotation.to_array(),
                color: match picked == Some(index) {
                    true => PICKED_COLOR,
                    false => JOINT_COLOR,
                },
            });
        }
        for (index, placement) in world.iter().enumerate() {
            let start = match parent_of(&self.parents, index) {
                Some(parent) => world[parent].translation,
                None => placement.translation,
            };
            let along = placement.translation - start;
            let length = along.length();
            instances.push(placed::Instance {
                center: ((start + placement.translation) * 0.5).to_array(),
                scale: [bone, bone, length * 0.5],
                turn: match length > f32::EPSILON {
                    true => Quat::from_rotation_arc(Vec3::Z, along / length).to_array(),
                    false => Quat::IDENTITY.to_array(),
                },
                color: match picked == Some(index) {
                    true => PICKED_COLOR,
                    false => BONE_COLOR,
                },
            });
        }

        vec![placed::Batch {
            shape: placed::Shape::Box,
            instances,
        }]
    }

    /// A view framed on the pose it is built with.
    pub fn view(&self, locals: &[Transform]) -> placed::View {
        placed::View::new(self.batches(&self.world(locals), None))
    }

    /// The bone tree, with the transform each one rests at. Clicking a row picks it out.
    pub fn tree_ui(&self, ui: &mut egui::Ui, locals: &[Transform], picked: &Cell<Option<usize>>) {
        table(ui, &self.columns, self.rows.len(), |ui, row| {
            let (bone, depth) = self.rows[row];
            let local = locals.get(bone).unwrap_or(&IDENTITY);
            let cells = [
                format!(
                    "{:indent$}{}",
                    "",
                    self.names[bone],
                    indent = depth * INDENT
                ),
                bone.to_string(),
                match parent_of(&self.parents, bone) {
                    Some(parent) => parent.to_string(),
                    None => "-".to_owned(),
                },
                axes(local.translation, 3),
                axes(local.rotation, 4),
                axes(local.scale, 3),
            ];
            let text =
                RichText::new(line(&self.columns, cells.iter().map(String::as_str))).monospace();
            let response = ui.add(
                egui::Label::new(match picked.get() == Some(bone) {
                    true => text.color(ui.visuals().hyperlink_color),
                    false => text,
                })
                .sense(Sense::click()),
            );
            if response.clicked() {
                picked.set((picked.get() != Some(bone)).then_some(bone));
            }
        });
    }
}

/// A bone with nothing above it, and the one case a file could get wrong: a parent at or past the
/// bone itself, which the walks here read in order and so could not reach.
fn parent_of(parents: &[i16], bone: usize) -> Option<usize> {
    usize::try_from(*parents.get(bone)?)
        .ok()
        .filter(|parent| *parent < bone)
}

const IDENTITY: Transform = Transform {
    translation: [0.0; 4],
    rotation: [0.0, 0.0, 0.0, 1.0],
    scale: [1.0, 1.0, 1.0, 0.0],
};

/// Where a pose stands and how far the furthest bone reaches from there, which is what a pose is
/// framed and clipped by. `anchor` is the bone the body hangs off; without one this falls back to
/// the middle of every bone, which a long tail drags around each time it swings.
pub fn middle(world: &[Placement], anchor: Option<usize>) -> (Vec3, f32) {
    if world.is_empty() {
        return (Vec3::ZERO, 0.0);
    }
    let center = match anchor.and_then(|bone| world.get(bone)) {
        Some(placement) => placement.translation,
        None => {
            world
                .iter()
                .map(|placement| placement.translation)
                .sum::<Vec3>()
                / world.len() as f32
        }
    };
    let reach = world
        .iter()
        .map(|placement| placement.translation.distance(center))
        .fold(0.0, f32::max);
    (center, reach)
}

/// How far across the pose reaches, which sizes the markers drawn on it.
fn extent(world: &[Placement]) -> f32 {
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    for placement in world {
        low = low.min(placement.translation);
        high = high.max(placement.translation);
    }
    match low.x <= high.x {
        true => (high - low).length().max(1e-3),
        false => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::{Rig, Transform};

    fn transform(translation: [f32; 3]) -> Transform {
        Transform {
            translation: [translation[0], translation[1], translation[2], 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0, 0.0],
        }
    }

    /// A chain of three offset bones, so the world walk has something to accumulate.
    fn rig(parents: Vec<i16>) -> Rig {
        Rig::new(
            &["a".to_owned(), "b".to_owned(), "c".to_owned()],
            &parents,
            &[
                transform([1.0, 0.0, 0.0]),
                transform([0.0, 2.0, 0.0]),
                transform([0.0, 0.0, 3.0]),
            ],
        )
    }

    #[test]
    fn composes_each_bone_onto_its_parent() {
        let rig = rig(vec![-1, 0, 1]);
        let world = rig.world(rig.reference());
        assert_eq!(world[0].translation.to_array(), [1.0, 0.0, 0.0]);
        assert_eq!(world[1].translation.to_array(), [1.0, 2.0, 0.0]);
        assert_eq!(world[2].translation.to_array(), [1.0, 2.0, 3.0]);
    }

    /// A marker per joint and a stick per bone, whether or not the bone has any length.
    #[test]
    fn draws_every_bone_whatever_its_length() {
        let rig = rig(vec![-1, 0, 0]);
        let world = rig.world(rig.reference());
        let batches = rig.batches(&world, None);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].instances.len(), 6);
        assert!(
            batches[0]
                .instances
                .iter()
                .all(|instance| instance.turn.iter().all(|value| value.is_finite())),
            "a bone of no length turned to nowhere"
        );
    }

    /// A parent index at or past its own bone would leave the ordered walks below it unreachable,
    /// so it reads as a root instead.
    #[test]
    fn a_parent_that_is_not_above_its_bone_is_a_root() {
        let rig = rig(vec![-1, 2, 1]);
        let world = rig.world(rig.reference());
        assert_eq!(world[1].translation.to_array(), [0.0, 2.0, 0.0]);
        assert_eq!(world[2].translation.to_array(), [0.0, 2.0, 3.0]);
        assert_eq!(rig.rows.len(), 3);
    }
}
