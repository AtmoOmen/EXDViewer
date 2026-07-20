use matchit::Params;

use super::path::Path;

type RouteStartFn<T> = dyn Fn(&mut T, &mut egui::Ui, &Path, &Params<'_, '_>) -> RouteResponse;
type RouteRenderFn<T> = dyn Fn(&mut T, &mut egui::Ui, &Path, &Params<'_, '_>);

pub enum RouteResponse {
    Title(String),
    Redirect(Path),
}

pub struct Route<T> {
    on_start: Box<RouteStartFn<T>>,
    on_render: Box<RouteRenderFn<T>>,
}

impl<T> Route<T> {
    pub fn new(
        on_start: impl Fn(&mut T, &mut egui::Ui, &Path, &Params<'_, '_>) -> RouteResponse + 'static,
        on_render: impl Fn(&mut T, &mut egui::Ui, &Path, &Params<'_, '_>) + 'static,
    ) -> Self {
        Self {
            on_start: Box::new(on_start),
            on_render: Box::new(on_render),
        }
    }

    pub fn unmatched() -> Self {
        Self::new(
            |_, _, _, _| RouteResponse::Title("未找到页面".to_string()),
            |_, ui, _, _| {
                ui.vertical_centered_justified(|ui| {
                    ui.heading("未找到页面");
                    ui.label("请求的页面不存在");
                    ui.label("请检查链接后重试");
                });
            },
        )
    }

    pub fn start(
        &self,
        state: &mut T,
        ui: &mut egui::Ui,
        path: &Path,
        params: &Params<'_, '_>,
    ) -> RouteResponse {
        (self.on_start)(state, ui, path, params)
    }

    pub fn render(&self, state: &mut T, ui: &mut egui::Ui, path: &Path, params: &Params<'_, '_>) {
        (self.on_render)(state, ui, path, params);
    }
}
