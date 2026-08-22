//! What a quest's `.luab` sequences: the scenes it declares, and the steps each one runs.
//!
//! A scene handler reads its dialogue and its assets through `self.NAME`, where the name is either a
//! row key in the quest's own text sheet or a `QuestParams` instruction. Both are resolved against
//! the install rather than here, so a step carries the name it read.

use std::collections::BTreeMap;

use anyhow::Result;
use luadec::{Chunk, Closure, Expr, Stat, Target};

/// A quest script, as the scenes it declares.
pub struct Script {
    pub scenes: Vec<Scene>,
    /// Functions the reading left as bytecode. Their steps read as their disassembly.
    pub disassembled: usize,
    /// Branches the scenes hold, which is how many picks a playback can make.
    pub branches: usize,
}

pub struct Scene {
    /// The number in `OnSceneNNNNN`.
    pub number: u32,
    pub steps: Vec<Step>,
}

impl Scene {
    pub fn lines(&self) -> usize {
        count(&self.steps, &|step| matches!(step, Step::Line { .. }))
    }

    pub fn cutscenes(&self) -> usize {
        count(&self.steps, &|step| matches!(step, Step::Cutscene(_)))
    }
}

fn count(steps: &[Step], want: &impl Fn(&Step) -> bool) -> usize {
    steps
        .iter()
        .map(|step| {
            usize::from(want(step))
                + match step {
                    Step::Branch { arms, .. } => {
                        arms.iter().map(|arm| count(&arm.steps, want)).sum()
                    }
                    _ => 0,
                }
        })
        .sum()
}

/// One step of a scene.
pub enum Step {
    /// A line of dialogue. The call names its row key among its arguments, and which argument that
    /// is varies, so every name it reads is kept and the one that resolves wins.
    Line { keys: Vec<String>, last: bool },

    /// How long the script holds, in frames.
    Wait(i32),

    /// A cutscene, by the `QuestParams` instruction naming its row.
    Cutscene(String),

    /// Background music, the same way.
    Bgm(String),

    /// A fade, which the script follows with `WaitForFade`.
    Fade { out: bool },

    /// A choice the script makes, whose arms hold steps of their own.
    Branch { id: usize, arms: Vec<Arm> },

    /// Anything else, as the source the reading wrote for it.
    Other(String),
}

/// One arm of a branch. The `else` carries no condition.
pub struct Arm {
    pub condition: Option<String>,
    pub steps: Vec<Step>,
}

/// The `t.KEY` an expression reads, where that is all it is.
fn key(held: &Expr) -> Option<&[u8]> {
    match held {
        Expr::Index(_, key) => match key.as_ref() {
            Expr::Str(name) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// Every `t.KEY` an argument list reads, in order.
fn names(arguments: &[Expr]) -> Vec<String> {
    arguments
        .iter()
        .filter_map(key)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

fn source(stat: &Stat) -> String {
    let mut lines = Vec::new();
    luadec::write_block(&mut lines, std::slice::from_ref(stat), 0);
    lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

struct Reader {
    branches: usize,
}

impl Reader {
    fn block(&mut self, block: &[Stat]) -> Vec<Step> {
        let mut steps = Vec::new();
        for stat in block {
            match stat {
                Stat::If(arms, otherwise) => {
                    let id = self.branches;
                    self.branches += 1;
                    let mut held: Vec<Arm> = arms
                        .iter()
                        .map(|(condition, body)| Arm {
                            condition: Some(condition.to_string()),
                            steps: self.block(body),
                        })
                        .collect();
                    if let Some(body) = otherwise {
                        held.push(Arm {
                            condition: None,
                            steps: self.block(body),
                        });
                    }
                    steps.push(Step::Branch { id, arms: held });
                }
                Stat::Do(body) => steps.extend(self.block(body)),
                Stat::Call(Expr::Method(_, name, arguments)) => {
                    steps.push(self.call(stat, name, arguments));
                }
                _ => steps.push(Step::Other(source(stat))),
            }
        }
        steps
    }

    fn call(&self, stat: &Stat, name: &str, arguments: &[Expr]) -> Step {
        let fallback = || Step::Other(source(stat));
        match name {
            "Talk" | "SystemTalk" => match names(arguments) {
                keys if keys.is_empty() => fallback(),
                keys => Step::Line {
                    last: arguments
                        .iter()
                        .find_map(|argument| match argument {
                            Expr::Bool(held) => Some(*held),
                            _ => None,
                        })
                        .unwrap_or(true),
                    keys,
                },
            },
            "Wait" => match arguments.first() {
                Some(Expr::Number(frames)) => Step::Wait(*frames as i32),
                _ => fallback(),
            },
            "PlayCutScene" => names(arguments)
                .into_iter()
                .next()
                .map_or_else(fallback, Step::Cutscene),
            "PlayBGM" => names(arguments)
                .into_iter()
                .next()
                .map_or_else(fallback, Step::Bgm),
            "FadeOut" => Step::Fade { out: true },
            "FadeIn" => Step::Fade { out: false },
            _ => fallback(),
        }
    }
}

/// The `OnSceneNNNNN` handlers a block assigns, by the number in the name. A script that assigns
/// one twice keeps the later handler, as running it would.
fn handlers(block: &[Stat], into: &mut BTreeMap<u32, Vec<Stat>>) {
    for stat in block {
        let Stat::Assign(targets, values) = stat else {
            continue;
        };
        let (Some(Target::Index(_, key)), Some(Expr::Function(closure))) =
            (targets.first(), values.first())
        else {
            continue;
        };
        let Expr::Str(name) = key else { continue };
        let Some(number) = String::from_utf8_lossy(name)
            .strip_prefix("OnScene")
            .and_then(|held| held.parse::<u32>().ok())
        else {
            continue;
        };
        into.insert(number, closure.body.clone());
    }
}

/// Read a script's scenes.
///
/// The game links several compiled files into one chunk, and the handlers sit in the linked units
/// rather than in the wrapper, so every unit is read.
pub fn read(bytes: &[u8]) -> Result<Script> {
    let chunk = Chunk::parse(bytes)?;
    let units: Vec<Closure> = match luadec::units(&chunk) {
        Some(units) => units.iter().map(luadec::read).collect(),
        None => vec![luadec::read(chunk.main())],
    };

    let mut found = BTreeMap::new();
    for unit in &units {
        handlers(&unit.body, &mut found);
    }

    let mut reader = Reader { branches: 0 };
    let scenes = found
        .into_iter()
        .map(|(number, body)| Scene {
            number,
            steps: reader.block(&body),
        })
        .collect();

    Ok(Script {
        scenes,
        disassembled: units
            .iter()
            .map(|unit| raw(&unit.body))
            .sum(),
        branches: reader.branches,
    })
}

fn raw(block: &[Stat]) -> usize {
    block
        .iter()
        .map(|stat| match stat {
            Stat::Raw(_) => 1,
            Stat::Do(body) | Stat::While(_, body) | Stat::Repeat(body, _) => raw(body),
            Stat::If(arms, otherwise) => {
                arms.iter().map(|(_, body)| raw(body)).sum::<usize>()
                    + otherwise.as_deref().map_or(0, raw)
            }
            Stat::NumericFor { body, .. } | Stat::GenericFor { body, .. } => raw(body),
            Stat::Assign(_, values) | Stat::Local(_, values) => values
                .iter()
                .map(|value| match value {
                    Expr::Function(closure) => raw(&closure.body),
                    _ => 0,
                })
                .sum(),
            _ => 0,
        })
        .sum()
}
