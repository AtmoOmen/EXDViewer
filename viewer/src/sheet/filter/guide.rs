use egui::{
    Align, Color32, FontId, Label, Layout, Margin, RichText, TextStyle, TextWrapMode, Vec2, Window,
};

use crate::settings::FILTER_GUIDE_VISIBLE;

const SECTIONS: &[(&str, &[[&str; 3]])] = &[
    (
        "示例",
        &[
            ["Name = potion", "列、比较符、值", ""],
            ["Name", "列: 要读取的列", ""],
            ["=", "比较符: 比较方式", ""],
            ["potion", "值: 要比较的内容", ""],
        ],
    ),
    (
        "比较符",
        &[
            ["=", "等于", "Name = potion"],
            ["^=, $=, *=", "开头为、结尾为、包含", "Name *= potion"],
            ["~=", "模糊匹配, 按匹配分数排序", "Name ~= ptn"],
            ["?=", "通配符", "Name ?= \"*potion?\""],
            ["/=", "正则表达式", "Name /= /^potion/i"],
            ["|=", "在范围内", "Icon |= 100..200"],
            [">, >=, <, <=", "数值比较", "Icon > 0"],
            ["!=, not ^=", "对任意比较取反", "Name != potion"],
            ["(cmp)=", "追加 = 可要求所有列均匹配", "Item[*] $== potion"],
        ],
    ),
    (
        "值",
        &[
            ["potion", "字母、数字、_ - . /", "Name = potion"],
            [
                "\"a potion\", 'a potion'",
                "其他内容需要引号",
                "Name = \"a potion\"",
            ],
            [
                "\\\" \\\\ \\n \\r \\t",
                "引号内的转义字符",
                "Name = \"say \\\"hi\\\"\"",
            ],
            ["-12", "整数, 不能有前导零", "Icon = -12"],
            ["10..20, 10.., ..20", "包含边界的范围", "Icon |= 10.."],
            [
                "/pattern/flags",
                "正则表达式, 标志: i m s U x R u",
                "Name /= /^a.*b$/i",
            ],
        ],
    ),
    (
        "列",
        &[
            ["Name", "按名称选择列", "Name = potion"],
            ["Text[3]", "一个数组元素", "Text[3] *= hi"],
            ["Item[0].Name", "数组元素中的字段", "Item[0].Name = a"],
            ["*", "通配符 (任意字符)", "Text[*] = potion"],
            ["?", "通配符 (单个字符)", "Text? = potion"],
            ["*", "任意列", "* = potion"],
            ["#", "行 ID", "# = 42"],
        ],
    ),
    (
        "任意列或所有列",
        &[
            ["=", "任意一列匹配", "Text* = potion"],
            ["==", "所有列都必须匹配", "Text* == potion"],
        ],
    ),
    (
        "组合条件",
        &[
            ["and, &&", "两侧都匹配", "Name = a and Icon > 0"],
            ["or, ||", "任意一侧匹配", "Name = a || Name = b"],
            ["not, !", "对后续条件取反", "not Name = a"],
            ["( )", "将条件组合在一起", "(a or b) and c"],
            [
                "a or b and c",
                "优先级依次为 not、and、or",
                "a or (b and c)",
            ],
        ],
    ),
];

const NOTES: &[&str] = &[
    "~= 会按匹配分数重新排列行, 因此行 ID 不再有序",
    "子行 ID 是文本, 例如 \"12.3\", 因此 # > 5 不会匹配任何内容",
    "正则表达式不能使用环视或反向引用",
    "筛选条件未完成时输入框会变红, 并保留上次结果",
];

pub fn draw(ctx: &egui::Context) {
    let visible = FILTER_GUIDE_VISIBLE.get(ctx);
    let mut open = visible;
    Window::new("筛选指南")
        .open(&mut open)
        .default_height(620.0)
        .vscroll(true)
        .show(ctx, draw_contents);
    if open != visible {
        FILTER_GUIDE_VISIBLE.set(ctx, open);
    }
}

fn draw_contents(ui: &mut egui::Ui) {
    let widths = column_widths(ui);
    let width = widths.iter().sum::<f32>() + ui.spacing().item_spacing.x * 2.0;
    ui.set_min_width(width);

    for (title, rows) in SECTIONS {
        header(ui, title, width);
        for row in *rows {
            ui.horizontal(|ui| {
                for (column, text) in row.iter().enumerate() {
                    cell(ui, text, column, widths[column]);
                }
            });
        }
    }

    header(ui, "注意事项", width);
    for note in NOTES {
        ui.label(*note);
    }
}

fn header(ui: &mut egui::Ui, title: &str, width: f32) {
    ui.add_space(8.0);
    egui::Frame::NONE
        .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.4))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.set_min_width(width - 12.0);
            ui.label(RichText::new(title).strong());
        });
    ui.add_space(2.0);
}

fn cell(ui: &mut egui::Ui, text: &str, column: usize, width: f32) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, 0.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_width(width);
            if !text.is_empty() {
                let text = RichText::new(text).font(font(ui, text, column));
                let text = if column == 0 { text.strong() } else { text };
                ui.add(Label::new(text).wrap_mode(TextWrapMode::Extend));
            }
        },
    );
}

fn font(ui: &egui::Ui, text: &str, column: usize) -> FontId {
    if column == 1 || !text.is_ascii() {
        TextStyle::Body.resolve(ui.style())
    } else {
        TextStyle::Monospace.resolve(ui.style())
    }
}

fn column_widths(ui: &egui::Ui) -> [f32; 3] {
    let mut widths = [0.0f32; 3];
    for (_, rows) in SECTIONS {
        for row in *rows {
            for (column, text) in row.iter().enumerate() {
                if text.is_empty() {
                    continue;
                }
                let font = font(ui, text, column);
                let galley = ui.fonts_mut(|fonts| {
                    fonts.layout_no_wrap((*text).to_owned(), font, Color32::PLACEHOLDER)
                });
                widths[column] = widths[column].max(galley.size().x);
            }
        }
    }
    widths
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders() {
        let ctx = egui::Context::default();
        crate::settings::FILTER_GUIDE_VISIBLE.set(&ctx, true);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| super::draw(ui.ctx()));
        assert!(!output.shapes.is_empty());
    }
}
