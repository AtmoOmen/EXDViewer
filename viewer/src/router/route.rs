use matchit::Params;

use super::path::Path;

/// A route's entry point returns where to go instead, or [`None`] to stay.
pub type Redirect = Option<Path>;

type RouteStartFn<T> = dyn Fn(&mut T, &mut egui::Ui, &Path, &Params<'_, '_>) -> Redirect;
type RouteRenderFn<T> = dyn Fn(&mut T, &mut egui::Ui, &Path, &Params<'_, '_>);
type RouteTitleFn<T> = dyn Fn(&T, &Path, &Params<'_, '_>) -> String;

pub struct Route<T> {
    on_start: Box<RouteStartFn<T>>,
    on_render: Box<RouteRenderFn<T>>,
    on_title: Box<RouteTitleFn<T>>,
}

/// Title for a route that always names the same thing.
pub fn static_title<T>(title: &'static str) -> impl Fn(&T, &Path, &Params<'_, '_>) -> String {
    move |_, _, _| title.to_string()
}

impl<T> Route<T> {
    pub fn new(
        on_start: impl Fn(&mut T, &mut egui::Ui, &Path, &Params<'_, '_>) -> Redirect + 'static,
        on_render: impl Fn(&mut T, &mut egui::Ui, &Path, &Params<'_, '_>) + 'static,
        on_title: impl Fn(&T, &Path, &Params<'_, '_>) -> String + 'static,
    ) -> Self {
        Self {
            on_start: Box::new(on_start),
            on_render: Box::new(on_render),
            on_title: Box::new(on_title),
        }
    }

    pub fn unmatched() -> Self {
        Self::new(
            |_, _, _, _| None,
            |_, ui, _, _| {
                ui.vertical_centered_justified(|ui| {
                    ui.heading("未找到页面");
                    ui.label("请求的页面不存在");
                    ui.label("请检查链接后重试");
                });
            },
|_, _, _| "未找到页面".to_string(),
        )
    }

    pub fn start(
        &self,
        state: &mut T,
        ui: &mut egui::Ui,
        path: &Path,
        params: &Params<'_, '_>,
    ) -> Redirect {
        (self.on_start)(state, ui, path, params)
    }

    pub fn render(&self, state: &mut T, ui: &mut egui::Ui, path: &Path, params: &Params<'_, '_>) {
        (self.on_render)(state, ui, path, params);
    }

    /// Evaluated every frame, not once on entry, so a title can follow something that loads later or
    /// survives the route being re-entered. A route with nothing selected names its tab; leaving the
    /// last route's title in place is not an option, which is what kept stale names in the tab bar.
    pub fn title(&self, state: &T, path: &Path, params: &Params<'_, '_>) -> String {
        (self.on_title)(state, path, params)
    }
}
