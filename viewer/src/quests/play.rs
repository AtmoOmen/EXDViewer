//! Plays a quest's scenes the way its script sequences them.

use std::collections::HashMap;

use egui::{
    Align, Button, CentralPanel, Color32, Label, Layout, RichText, ScrollArea, Sense,
    containers::panel::Panel,
};

use crate::quests::{
    Action, Load,
    detail::Detail,
    script::{Arm, Script, Step},
};

/// Frames a second, which is what an animation pack states for a timeline. The script's own `Wait`
/// counts frames but names no rate.
const TICKS: f32 = 30.0;

/// How long a line holds when the playback runs itself. No file states one: in game a line waits
/// for the player.
const HOLD: f32 = 3.0;

pub struct Player {
    node: Option<u32>,
    scene: usize,
    /// Which step of the walked scene is on screen, and how long it has been.
    step: usize,
    elapsed: f32,
    playing: bool,
    hold: f32,
    /// Which arm each branch takes, by the branch's id.
    picks: HashMap<usize, usize>,
    /// Whether the orders a scene gives around its dialogue are on show.
    orders: bool,
    /// Set when a jump wants the step list scrolled to the current step.
    follow: bool,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            node: None,
            scene: 0,
            step: 0,
            elapsed: 0.0,
            playing: false,
            hold: HOLD,
            picks: HashMap::new(),
            orders: true,
            follow: false,
        }
    }
}

/// The steps a scene runs with the branches taken as they stand, each with how deep it sits.
fn walk<'a>(
    steps: &'a [Step],
    picks: &HashMap<usize, usize>,
    depth: usize,
    out: &mut Vec<(&'a Step, usize)>,
) {
    for step in steps {
        out.push((step, depth));
        if let Step::Branch { id, arms } = step {
            let taken = picks.get(id).copied().unwrap_or(0).min(arms.len() - 1);
            walk(&arms[taken].steps, picks, depth + 1, out);
        }
    }
}

/// How long a step holds, in seconds. Only a wait and a line take any time: everything else a scene
/// runs is an order the script gives and moves straight past.
fn dwell(step: &Step, hold: f32) -> f32 {
    match step {
        Step::Wait(frames) => (*frames).max(0) as f32 / TICKS,
        Step::Line { .. } => hold,
        _ => 0.0,
    }
}

fn arm_label(arm: &Arm) -> String {
    match &arm.condition {
        Some(condition) => format!("if {condition}"),
        None => "else".to_owned(),
    }
}

impl Player {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn rewind(&mut self) {
        self.step = 0;
        self.elapsed = 0.0;
        self.follow = true;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, detail: &Detail, node: u32) -> Option<Action> {
        if self.node != Some(node) {
            let hold = self.hold;
            self.reset();
            self.hold = hold;
            self.node = Some(node);
        }

        let script = match detail.script() {
            Load::Idle | Load::Loading(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading the script…");
                });
                return None;
            }
            Load::Failed(error) => {
                ui.colored_label(Color32::RED, error.clone());
                return None;
            }
            Load::Ready(script) => script,
        };
        if script.scenes.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("This script declares no scenes").weak());
            });
            return None;
        }

        self.scene = self.scene.min(script.scenes.len() - 1);
        let mut steps = Vec::new();
        walk(&script.scenes[self.scene].steps, &self.picks, 0, &mut steps);
        if !self.orders {
            steps.retain(|(step, _)| !matches!(step, Step::Other(_) | Step::Fade { .. }));
        }

        let mut action = None;
        Panel::left("quest_scenes")
            .default_size(180.0)
            .show(ui, |ui| self.scenes_ui(ui, script));
        Panel::bottom("quest_transport").show(ui, |ui| {
            ui.add_space(4.0);
            self.transport(ui, &steps);
            ui.add_space(4.0);
        });
        CentralPanel::default().show(ui, |ui| {
            action = self.steps_ui(ui, detail, &steps);
        });
        action
    }

    fn scenes_ui(&mut self, ui: &mut egui::Ui, script: &Script) {
        ScrollArea::vertical()
            .id_salt("quest_scene_list")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
                    for (at, scene) in script.scenes.iter().enumerate() {
                        let (lines, cutscenes) = (scene.lines(), scene.cutscenes());
                        let mut label = format!("Scene {}", scene.number);
                        if lines > 0 {
                            label.push_str(&format!(" · {lines} lines"));
                        }
                        if cutscenes > 0 {
                            label.push_str(&format!(" · {cutscenes} cut"));
                        }
                        if scene.steps.is_empty() {
                            label.push_str(" · empty");
                        }
                        if ui
                            .add(Button::selectable(at == self.scene, label))
                            .clicked()
                        {
                            self.scene = at;
                            self.rewind();
                        }
                    }
                });
            });
    }

    fn transport(&mut self, ui: &mut egui::Ui, steps: &[(&Step, usize)]) {
        let count = steps.len();
        self.step = self.step.min(count.saturating_sub(1));

        if self.playing && count > 0 {
            self.elapsed += ui.input(|input| input.stable_dt).min(0.25);
            while self.step + 1 < count {
                let held = dwell(steps[self.step].0, self.hold);
                if self.elapsed < held {
                    break;
                }
                self.elapsed -= held;
                self.step += 1;
                self.follow = true;
            }
            if self.step + 1 >= count && self.elapsed >= dwell(steps[self.step].0, self.hold) {
                self.playing = false;
            }
            ui.ctx().request_repaint();
        }

        ui.horizontal(|ui| {
            if ui.button("⏮").on_hover_text("Back to the start").clicked() {
                self.rewind();
            }
            if ui
                .add(Button::new(if self.playing { "⏸" } else { "▶" }))
                .clicked()
            {
                self.playing = !self.playing;
                self.elapsed = 0.0;
            }
            if ui
                .add_enabled(self.step + 1 < count, Button::new("⏭"))
                .on_hover_text("Next step")
                .clicked()
            {
                self.step += 1;
                self.elapsed = 0.0;
                self.follow = true;
            }
            ui.label(
                RichText::new(match count {
                    0 => "no steps".to_owned(),
                    count => format!("step {} of {count}", self.step + 1),
                })
                .weak(),
            );

            if ui
                .toggle_value(&mut self.orders, "👁")
                .on_hover_text("Show the orders a scene gives around its dialogue")
                .changed()
            {
                self.rewind();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().slider_width = 120.0;
                ui.add(
                    egui::Slider::new(&mut self.hold, 0.5..=10.0)
                        .suffix(" s")
                        .text("line"),
                )
                .on_hover_text(
                    "How long a line holds. Nothing in the files states one: in game a line waits \
                     for the player.",
                );
            });
        });
    }

    fn steps_ui(
        &mut self,
        ui: &mut egui::Ui,
        detail: &Detail,
        steps: &[(&Step, usize)],
    ) -> Option<Action> {
        let mut action = None;
        let mut jumped = None;
        let follow = std::mem::take(&mut self.follow);

        ScrollArea::vertical()
            .id_salt("quest_steps")
            .auto_shrink(false)
            .show(ui, |ui| {
                for (at, (step, depth)) in steps.iter().enumerate() {
                    let current = at == self.step;
                    let response = ui
                        .horizontal_wrapped(|ui| {
                            ui.add_space(*depth as f32 * ui.spacing().indent);
                            ui.label(RichText::new(if current { "▶" } else { " " }).weak());
                            action = action.take().or(self.step_ui(ui, detail, step, current));
                        })
                        .response;
                    if response.interact(Sense::click()).clicked() {
                        jumped = Some(at);
                    }
                    if current && follow {
                        response.scroll_to_me(Some(Align::Center));
                    }
                }
            });

        if let Some(at) = jumped {
            self.step = at;
            self.elapsed = 0.0;
        }
        action
    }

    fn step_ui(
        &mut self,
        ui: &mut egui::Ui,
        detail: &Detail,
        step: &Step,
        current: bool,
    ) -> Option<Action> {
        match step {
            Step::Line { keys, last } => {
                let held = detail
                    .dialogue_lines()
                    .and_then(|held| keys.iter().find_map(|key| held.line(key)));
                match held {
                    Some(line) => {
                        ui.label(RichText::new(format!("{}:", line.speaker)).strong());
                        let text = super::detail::sestring(ui, &line.text);
                        let text = RichText::new(text);
                        ui.label(match current {
                            true => text.color(ui.visuals().strong_text_color()),
                            false => text,
                        });
                        if !last {
                            ui.label(RichText::new("↩").weak().small())
                                .on_hover_text("The box carries on into the next line");
                        }
                    }
                    None => {
                        ui.label(RichText::new(keys.join(", ")).weak().monospace())
                            .on_hover_text("No row in the quest's text sheet carries this key");
                    }
                }
                None
            }
            Step::Wait(frames) => {
                ui.label(RichText::new(format!("wait {frames} frames")).weak())
                    .on_hover_text(format!(
                        "Played at {TICKS} a second, the rate an animation pack states for a \
                         timeline. The script states none."
                    ));
                None
            }
            Step::Cutscene(param) => {
                ui.label(RichText::new("cutscene").strong());
                asset(ui, detail, param)
            }
            Step::Bgm(param) => {
                ui.label(RichText::new("music").strong());
                asset(ui, detail, param)
            }
            Step::Fade { out } => {
                ui.label(RichText::new(if *out { "fade out" } else { "fade in" }).weak());
                None
            }
            Step::Branch { id, arms } => {
                let taken = self.picks.get(id).copied().unwrap_or(0).min(arms.len() - 1);
                for (at, arm) in arms.iter().enumerate() {
                    if ui
                        .add(Button::selectable(at == taken, arm_label(arm)))
                        .on_hover_text("Play this arm")
                        .clicked()
                    {
                        self.picks.insert(*id, at);
                        self.follow = true;
                    }
                }
                None
            }
            Step::Other(source) => {
                ui.label(RichText::new(source).weak().monospace().small());
                None
            }
        }
    }
}

/// A link to the file a `QuestParams` instruction names, or the instruction where the quest names
/// no file for it.
fn asset(ui: &mut egui::Ui, detail: &Detail, param: &str) -> Option<Action> {
    let Some(path) = detail.links().and_then(|links| links.asset(param)) else {
        ui.label(RichText::new(param).weak().monospace())
            .on_hover_text("No script parameter of this quest names a file for this");
        return None;
    };
    let response = ui
        .add(
            Label::new(RichText::new(path).color(ui.visuals().hyperlink_color))
                .sense(Sense::click()),
        )
        .on_hover_text(param)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response
        .clicked()
        .then(|| Action::Navigate(format!("/assets/{path}")))
}
