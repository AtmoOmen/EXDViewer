//! `.luab` compiled Lua: the game's quest and event scripts, read back as source.

use anyhow::Result;
use egui::{RichText, ScrollArea};
use luadec::Chunk;

use super::shader::code::listing;
use super::{Preview, facts};
use crate::utils::export;

/// A chunk, read and ready to draw.
pub struct Rendered {
    identity: Vec<(&'static str, String)>,
    source: Vec<String>,
    assembly: Vec<String>,
    /// Statements the reading recovered, which a chunk compiled from an empty file has none of.
    statements: usize,
    /// Functions left as commented instructions, each under a line saying why.
    commented: usize,
    /// Which reading is on show, kept per file the way the shader viewers keep theirs.
    state: egui::Id,
}

pub fn decode(path: &str, bytes: &[u8]) -> Result<Preview> {
    let chunk = Chunk::parse(bytes)?;
    let header = chunk.header();
    let read = luadec::decompile(&chunk);
    let units = luadec::units(&chunk).map_or(1, <[_]>::len);

    let main = chunk.main();
    let mut identity = vec![
        (
            "版本",
            format!("Lua {:X}.{:X}", header.version >> 4, header.version & 0xF),
        ),
        (
            "布局",
            format!(
                "{} 位{}端，{}",
                u16::from(header.size_size) * 8,
                match header.little_endian {
                    0 => "大",
                    _ => "小",
                },
                match (header.integral, header.size_number) {
                    (0, 8) => "双精度".to_owned(),
                    (0, size) => format!("{} 位浮点", u16::from(size) * 8),
                    (_, size) => format!("{} 位整数", u16::from(size) * 8),
                },
            ),
        ),
        ("单位", units.to_string()),
        (
            "函数",
            format!(
                "{} 个还原为源码，{} 个保留为字节码",
                read.functions, read.disassembled
            ),
        ),
        (
            "调试信息",
            match main.lines().is_empty() && main.locals().is_empty() {
                true => "已剥离".to_owned(),
                false => "已保留".to_owned(),
            },
        ),
    ];
    // Only a chunk that kept its debug info says where it was compiled from.
    if let Some(source) = main.source() {
        identity.push(("源码", String::from_utf8_lossy(source).into_owned()));
    }

    Ok(Preview::Luab(Box::new(Rendered {
        identity,
        source: read.lines,
        assembly: luadec::disassemble(&chunk),
        statements: read.statements,
        commented: read.disassembled,
        state: egui::Id::new(("luab code", path)),
    })))
}

pub fn ui(ui: &mut egui::Ui, file: &Rendered) {
    let slot = file.state.with("source");
    let mut source = ui.data(|data| data.get_temp::<bool>(slot)).unwrap_or(true);
    let lines = match source {
        true => &file.source,
        false => &file.assembly,
    };

    ui.horizontal(|ui| {
        ui.selectable_value(&mut source, true, "Lua");
        ui.selectable_value(&mut source, false, "字节码");
        ui.label(
            RichText::new(format!("{} 行", lines.len()))
                .weak()
                .small(),
        );
        if ui.small_button("复制").clicked() {
            ui.ctx().copy_text(lines.join("\n"));
        }
        if source {
            ui.label(
                RichText::new(match file.commented {
                    0 => "可编译，但还原不保证完美".to_owned(),
                    1 => "下方有 1 个函数以注释字节码显示".to_owned(),
                    held => format!("下方有 {held} 个函数以注释字节码显示"),
                })
                .weak()
                .small(),
            );
        }
    });
    ui.data_mut(|data| data.insert_temp(slot, source));

    // A chunk compiled from a file holding only comments has nothing to show, which is worth saying
    // rather than leaving the page blank.
    if source && file.statements == 0 {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("由不含语句的文件编译而来。" ).weak());
        });
        return;
    }

    listing(
        ui,
        "luab_code",
        lines,
        0,
        match source {
            true => "Lua",
            false => "Lua 字节码",
        },
    );
}

/// Beyond the raw file: the same reading the two toggles above already hold, so a save always
/// matches what is on screen.
pub fn export_choices(file: &Rendered) -> Vec<export::Choice<'_>> {
    vec![
        export::Choice::bytes("作为 Lua", "script.lua", move || {
            Ok(file.source.join("\n").into_bytes())
        })
        .filter("Lua 源码", &["lua"]),
        export::Choice::bytes("反汇编", "script.luadis.txt", move || {
            Ok(file.assembly.join("\n").into_bytes())
        })
        .filter("文本", &["txt"]),
    ]
}

impl Rendered {
    pub fn details_ui(&self, ui: &mut egui::Ui) {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            facts(ui, "luab_identity", &self.identity);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    /// A real quest script, run manually against the local install (`cargo test -p viewer --lib
    /// -- --ignored luab::tests --nocapture`): what the "As Lua" choice would write is handed to
    /// the system's own Lua, since a decompiler is only as good as what actually parses.
    #[test]
    #[ignore = "reads the real local FFXIV install and shells out to lua5.1"]
    fn the_lua_choice_parses_under_the_real_interpreter() {
        use ironworks::sqpack::{Install, SqPack};
        use std::io::Read;

        let path = "game_script/quest/044/AktKmg115_04464.luab";
        let pack = SqPack::new(Install::at_sqpack("/home/asriel/.xlcore/ffxiv/game/sqpack"));
        let mut stream = pack.file(path).expect("the quest script is in the local install");
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();

        let preview = super::decode(path, &bytes).expect("a real quest script decodes");
        let super::Preview::Luab(rendered) = preview else {
            panic!("decode() of a .luab did not return Preview::Luab");
        };
        let lua = rendered.source.join("\n");
        println!("{} lines of Lua, {} statements", rendered.source.len(), rendered.statements);

        let dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| ".".to_owned());
        let out = std::path::Path::new(&dir).join("luab_export_check.lua");
        std::fs::File::create(&out).unwrap().write_all(lua.as_bytes()).unwrap();

        let check = std::process::Command::new("luac5.1")
            .arg("-p")
            .arg(&out)
            .output()
            .expect("luac5.1 must be on PATH to run this check");
        assert!(
            check.status.success(),
            "luac5.1 rejected the exported source:\n{}",
            String::from_utf8_lossy(&check.stderr)
        );
    }
}
