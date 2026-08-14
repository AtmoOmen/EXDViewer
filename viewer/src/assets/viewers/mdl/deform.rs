//! `human.pbd`, which is what a body built on another one differs from it by.
//!
//! The file states a tree over the character codes and, at each node, a transform per bone naming
//! what that body moves of the one it is built on. A garment is modelled once for a body near the
//! root and worn by everything below it: the file it is worn from is the nearest code that ships
//! one, and the bones between that code and the wearer are what shape it to them.

use std::collections::BTreeMap;

use anyhow::Result;
use glam::{Mat3, Mat4, Vec3, Vec4};
use ironworks::file::{File, pbd::PreBoneDeformer};

use super::Vertex;

/// How many bones one vertex is weighted to, as the file packs them: four to a lane, the first four
/// in the low bytes and the rest in the high ones.
const INFLUENCES: usize = 8;

/// Every body the deformers name, and what each moves of the one it is built on.
pub struct Deformers {
    built_on: BTreeMap<u16, u16>,
    moves: BTreeMap<u16, BTreeMap<String, Mat4>>,
}

impl Deformers {
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let file = PreBoneDeformer::read(std::io::Cursor::new(bytes.to_vec()))?;
        let mut built_on = BTreeMap::new();
        let mut moves = BTreeMap::new();
        for deformer in file.deformers() {
            let code = deformer.id();
            if let Some(parent) = deformer.node().parent() {
                built_on.insert(code, parent.deformer().id());
            }
            let bones = deformer
                .bones()
                .unwrap_or_default()
                .iter()
                .map(|(name, rows)| (name.clone(), matrix(rows)))
                .collect();
            moves.insert(code, bones);
        }
        Ok(Self { built_on, moves })
    }

    /// This body and every one it is built on, nearest first.
    pub fn lineage(&self, code: u16) -> impl Iterator<Item = u16> + '_ {
        std::iter::successors(Some(code), |code| self.built_on.get(code).copied())
    }

    /// What moves a model made for one body onto a body built on it, or nothing where the two are
    /// the same body or neither is built on the other.
    pub fn between(&self, from: u16, to: u16) -> Option<Deform> {
        let mut steps: Vec<&BTreeMap<String, Mat4>> = Vec::new();
        for code in self.lineage(to) {
            if code == from {
                let mut moved: BTreeMap<String, Mat4> = BTreeMap::new();
                for step in steps.into_iter().rev() {
                    for (bone, matrix) in step {
                        let held = moved.entry(bone.clone()).or_insert(Mat4::IDENTITY);
                        *held = *matrix * *held;
                    }
                }
                return (!moved.is_empty()).then_some(Deform(moved));
            }
            steps.push(self.moves.get(&code)?);
        }
        None
    }
}

/// One body's difference from another, by the bone each vertex is weighted to.
pub struct Deform(BTreeMap<String, Mat4>);

impl Deform {
    /// Moves a mesh's vertices onto the body this was read for. Each vertex is moved by the bones
    /// it is weighted to, the same blend the skinning does, which is how the game shapes a garment
    /// it has no model of its own for.
    pub fn apply(&self, vertices: &mut [Vertex], table: &[String]) {
        let moved: Vec<Option<(Mat4, Mat3)>> = table
            .iter()
            .map(|bone| {
                self.0
                    .get(bone)
                    .map(|matrix| (*matrix, Mat3::from_mat4(*matrix).inverse().transpose()))
            })
            .collect();
        if moved.iter().all(Option::is_none) {
            return;
        }
        for vertex in vertices {
            let mut weight = 0.0;
            let mut position = Vec3::ZERO;
            let mut normal = Vec3::ZERO;
            let mut tangent = Vec3::ZERO;
            let mut bitangent = Vec3::ZERO;
            let (held, frame) = (Vec3::from(vertex.position), Vec3::from(vertex.normal));
            let (along, across) = (unbias(sides(vertex.tangent)), unbias(sides(vertex.bitangent)));
            for influence in 0..INFLUENCES {
                let (lane, half) = (influence % 4, influence / 4);
                let share = f32::from(vertex.weights[lane].to_le_bytes()[half]) / 255.0;
                if share == 0.0 {
                    continue;
                }
                let bone = usize::from(vertex.bones[lane].to_le_bytes()[half]);
                weight += share;
                match moved.get(bone).copied().flatten() {
                    Some((matrix, rotation)) => {
                        position += share * matrix.transform_point3(held);
                        normal += share * (rotation * frame);
                        tangent += share * (rotation * along);
                        bitangent += share * (rotation * across);
                    }
                    None => {
                        position += share * held;
                        normal += share * frame;
                        tangent += share * along;
                        bitangent += share * across;
                    }
                }
            }
            if weight == 0.0 {
                continue;
            }
            vertex.position = (position / weight).into();
            vertex.normal = normal.try_normalize().unwrap_or(frame).into();
            vertex.tangent = bias(tangent.try_normalize().unwrap_or(along), vertex.tangent[3]);
            vertex.bitangent = bias(bitangent.try_normalize().unwrap_or(across), vertex.bitangent[3]);
        }
    }
}

/// A deformer's transform, which the file writes as the three rows a fourth of nought and one would
/// follow.
fn matrix(rows: &[[f32; 4]; 3]) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(rows[0][0], rows[1][0], rows[2][0], 0.0),
        Vec4::new(rows[0][1], rows[1][1], rows[2][1], 0.0),
        Vec4::new(rows[0][2], rows[1][2], rows[2][2], 0.0),
        Vec4::new(rows[0][3], rows[1][3], rows[2][3], 1.0),
    )
}

/// A tangent frame is held the way the shaders read it, scaled to nought and one.
fn sides(frame: [f32; 4]) -> [f32; 3] {
    [frame[0], frame[1], frame[2]]
}

fn unbias(frame: [f32; 3]) -> Vec3 {
    Vec3::from(frame) * 2.0 - 1.0
}

fn bias(frame: Vec3, handedness: f32) -> [f32; 4] {
    let frame = frame * 0.5 + 0.5;
    [frame.x, frame.y, frame.z, handedness]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Midlander woman built on the man at half his size, and a Roegadyn woman built on her at
    /// three times hers, which is the shape of the tree the file states.
    fn deformers() -> Deformers {
        let moves = |bone: &str, scale: f32| {
            BTreeMap::from([(bone.to_owned(), Mat4::from_scale(Vec3::splat(scale)))])
        };
        Deformers {
            built_on: BTreeMap::from([(201, 101), (1001, 201)]),
            moves: BTreeMap::from([
                (101, BTreeMap::new()),
                (201, moves("j_sebo_a", 0.5)),
                (1001, moves("j_sebo_a", 3.0)),
            ]),
        }
    }

    #[test]
    fn walks_from_a_body_to_the_one_it_is_built_on() {
        let held = deformers();
        assert_eq!(held.lineage(1001).collect::<Vec<_>>(), [1001, 201, 101]);
        assert_eq!(held.lineage(101).collect::<Vec<_>>(), [101]);
    }

    #[test]
    fn moves_by_every_body_between_the_two() {
        let held = deformers();
        let bone = held.between(101, 1001).unwrap().0["j_sebo_a"];
        assert_eq!(bone.transform_point3(Vec3::X), Vec3::X * 1.5);
        // A body wears its own model as it is, and one it is built on cannot wear its.
        assert!(held.between(1001, 1001).is_none());
        assert!(held.between(1001, 101).is_none());
    }

    #[test]
    fn moves_a_vertex_by_the_bones_it_is_weighted_to() {
        let held = deformers();
        let deform = held.between(201, 1001).unwrap();
        let mut vertices = vec![Vertex {
            position: [1.0, 0.0, 0.0],
            normal: [1.0, 0.0, 0.0],
            tangent: [0.5; 4],
            bitangent: [0.5; 4],
            uv: [0.0; 4],
            uv1: [0.0; 4],
            color: [255; 4],
            color1: [255; 4],
            weights: [128, 127, 0, 0],
            bones: [0, 1, 0, 0],
        }];
        // Half weighted to the bone that moves and half to one that does not, so the vertex lands
        // between the two rather than at either.
        deform.apply(&mut vertices, &["j_sebo_a".to_owned(), "n_root".to_owned()]);
        assert!((vertices[0].position[0] - 2.0).abs() < 0.01);
        assert!(Vec3::from(vertices[0].normal).distance(Vec3::X) < 0.001);
    }
}
