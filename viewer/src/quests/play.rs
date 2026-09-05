//! Plays a quest's scenes the way its script sequences them.

use std::collections::HashMap;

use egui::{
    containers::panel::Panel, Align, Button, CentralPanel, Color32, Layout, RichText, ScrollArea,
    Sense,
};

use crate::quests::{
    detail::{Detail, Line},
    dialogue_box,
    script::{Arm, Script, Step},
    Action, Load,
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
        Some(condition) => format!("如果 {condition}"),
        None => "否则".to_owned(),
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
                    ui.label("正在读取脚本…");
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
                ui.label(RichText::new("此脚本未声明任何场景").weak());
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
        self.now_playing_ui(ui, detail, &steps);
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

    /// The current step, boxed the way the game presents it: a line in its dialogue box, or a
    /// branch as a question prompt. Silent for anything else a scene runs.
    fn now_playing_ui(&mut self, ui: &mut egui::Ui, detail: &Detail, steps: &[(&Step, usize)]) {
        let Some((step, _)) = steps.get(self.step) else {
            return;
        };
        match step {
            Step::Line { keys, .. } => {
                let Some(line) = resolve_line(detail, keys) else {
                    return;
                };
                let text = super::detail::sestring(ui, &line.text);
                Panel::top("quest_now_playing").show(ui, |ui| {
                    ui.add_space(4.0);
                    dialogue_box::ui(ui, &line.speaker, RichText::new(text));
                    ui.add_space(4.0);
                });
            }
            Step::Branch { id, arms } => {
                let id = *id;
                let taken = self
                    .picks
                    .get(&id)
                    .copied()
                    .unwrap_or(0)
                    .min(arms.len() - 1);
                let labels: Vec<String> = arms.iter().map(arm_label).collect();
                Panel::top("quest_now_playing").show(ui, |ui| {
                    ui.add_space(4.0);
                    if let Some(at) = dialogue_box::options_ui(ui, &labels, taken) {
                        self.pick_arm(id, at);
                    }
                    ui.add_space(4.0);
                });
            }
            _ => {}
        }
    }

    fn pick_arm(&mut self, id: usize, at: usize) {
        self.picks.insert(id, at);
        self.follow = true;
    }

    fn scenes_ui(&mut self, ui: &mut egui::Ui, script: &Script) {
        ScrollArea::vertical()
            .id_salt("quest_scene_list")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
                    for (at, scene) in script.scenes.iter().enumerate() {
                        let (lines, cutscenes) = (scene.lines(), scene.cutscenes());
                        let mut label = format!("场景 {}", scene.number);
                        if lines > 0 {
                            label.push_str(&format!(" · {lines} 行"));
                        }
                        if cutscenes > 0 {
                            label.push_str(&format!(" · {cutscenes} 段过场"));
                        }
                        if scene.steps.is_empty() {
                            label.push_str(" · 空");
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

        ui.horizontal_wrapped(|ui| {
            if ui.button("⏮").on_hover_text("回到开头").clicked() {
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
                .on_hover_text("下一步")
                .clicked()
            {
                self.step += 1;
                self.elapsed = 0.0;
                self.follow = true;
            }
            ui.label(
                RichText::new(match count {
                    0 => "无步骤".to_owned(),
                    count => format!("第 {} 步，共 {count} 步", self.step + 1),
                })
                .weak(),
            );

            if ui
                .toggle_value(&mut self.orders, "👁")
                .on_hover_text("显示场景在对话前后给出的指令")
                .changed()
            {
                self.rewind();
            }

            ui.spacing_mut().slider_width = 120.0;
            ui.add(
                egui::Slider::new(&mut self.hold, 0.5..=10.0)
                    .suffix(" s")
                    .text("台词"),
            )
            .on_hover_text(
                "一行台词停留的时长。文件中没有规定该值：游戏中台词会等待 \
                 玩家。",
            );
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
                match resolve_line(detail, keys) {
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
                                .on_hover_text("对话框延续到下一行");
                        }
                    }
                    None => {
                        ui.label(RichText::new(keys.join(", ")).weak().monospace())
                            .on_hover_text("任务文本表中没有承载此键值的行");
                    }
                }
                None
            }
            Step::Wait(frames) => {
                ui.label(RichText::new(format!("等待 {frames} 帧")).weak())
                    .on_hover_text(format!(
                        "以每秒 {TICKS} 帧播放，即动画包为时间轴声明的速率。脚本未声明 \
                         任何速率。"
                    ));
                None
            }
            Step::Cutscene(param) => {
                ui.label(RichText::new("过场").strong());
                asset(ui, detail, param)
            }
            Step::Bgm(param) => {
                ui.label(RichText::new("音乐").strong());
                asset(ui, detail, param)
            }
            Step::Fade { out } => {
                ui.label(RichText::new(if *out { "淡出" } else { "淡入" }).weak());
                None
            }
            Step::Branch { id, arms } => {
                let taken = self.picks.get(id).copied().unwrap_or(0).min(arms.len() - 1);
                for (at, arm) in arms.iter().enumerate() {
                    if ui
                        .add(Button::selectable(at == taken, arm_label(arm)))
                        .on_hover_text("播放此段")
                        .clicked()
                    {
                        self.pick_arm(*id, at);
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

/// The dialogue line a `Step::Line`'s keys resolve to, trying each until one lands.
fn resolve_line<'a>(detail: &'a Detail, keys: &[String]) -> Option<&'a Line> {
    detail
        .dialogue_lines()
        .and_then(|held| keys.iter().find_map(|key| held.line(key)))
}

/// A link to the file a `QuestParams` instruction names, or the instruction where the quest names
/// no file for it.
fn asset(ui: &mut egui::Ui, detail: &Detail, param: &str) -> Option<Action> {
    let Some(path) = detail.links().and_then(|links| links.asset(param)) else {
        ui.label(RichText::new(param).weak().monospace())
            .on_hover_text("此任务没有脚本参数为其指定文件");
        return None;
    };
    let clicked = super::detail::path_link(ui, path);
    ui.label(RichText::new(param).weak().small());
    clicked.then(|| Action::Navigate(format!("/assets/{path}")))
}
