use crate::router::path::Path;
use anyhow::{anyhow, bail};
use egui::{Id, util::IdTypeMap};

use super::History;

pub struct MemoryHistory {
    ctx: egui::Context,
}

impl MemoryHistory {
    fn history(d: &mut IdTypeMap) -> &mut Vec<Path> {
        d.get_persisted_mut_or_insert_with(Id::new("memory_history"), || vec!["/".into()])
    }

    fn position(d: &mut IdTypeMap) -> &mut usize {
        d.get_persisted_mut_or_insert_with(Id::new("memory_history_position"), || 0)
    }
}

impl History for MemoryHistory {
    fn new(ctx: egui::Context) -> Self {
        Self { ctx }
    }

    fn set_title(&mut self, title: String) {
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn base_url(&self) -> String {
        String::new()
    }

    fn active_route(&self) -> Path {
        self.ctx
            .data_mut(|d| {
                let position = {
                    let history_len = Self::history(d).len();
                    let position = Self::position(d);
                    if *position >= history_len {
                        log::warn!(
                            "位置 {position} 超出历史记录长度 {history_len} 的范围"
                        );
                        *position = history_len - 1;
                    }
                    *position
                };
                Self::history(d).get(position).cloned()
            })
            .unwrap()
    }

    fn push(&mut self, location: Path) -> anyhow::Result<()> {
        self.ctx.data_mut(|d| {
            let position = *Self::position(d);
            let history = Self::history(d);
            history.drain(position + 1..);
            history.push(location);
            *Self::position(d) += 1;
        });
        Ok(())
    }

    fn replace(&mut self, location: Path) -> anyhow::Result<()> {
        self.ctx.data_mut(|d| {
            let position = *Self::position(d);
            *Self::history(d)
                .get_mut(position)
                .ok_or_else(|| anyhow!("历史记录位置无效"))? = location;
            Ok(())
        })
    }

    fn back(&mut self) -> anyhow::Result<()> {
        self.ctx.data_mut(|d| {
            let position = Self::position(d);
            if *position == 0 {
                bail!("已到达第一条记录，无法后退");
            }
            *position -= 1;
            Ok(())
        })
    }

    fn forward(&mut self) -> anyhow::Result<()> {
        self.ctx.data_mut(|d| {
            let history_len = Self::history(d).len();
            let position = Self::position(d);
            if *position >= history_len - 1 {
                bail!("已到达最后一条记录，无法前进");
            }
            *position += 1;
            Ok(())
        })
    }
}
