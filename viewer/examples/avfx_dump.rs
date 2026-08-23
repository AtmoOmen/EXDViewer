//! Dump the full structure of an avfx file for manual inspection.

use ironworks::{
    Ironworks,
    file::File,
    file::avfx::{Avfx, Block, Payload},
    sqpack::{Install, SqPack},
};

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

fn dump_block(block: &Block, depth: usize) {
    let indent = "  ".repeat(depth);
    match block.payload() {
        Payload::Blocks(children) => {
            println!("{indent}{} ({} children)", block.name(), children.len());
            for child in children {
                dump_block(child, depth + 1);
            }
        }
        Payload::Bytes(bytes) => {
            let as_i32 = block.i32();
            let as_f32 = block.f32();
            let as_text = block.text();
            println!(
                "{indent}{} [{} bytes] i32={as_i32:?} f32={as_f32:?} text={as_text:?} raw={:02x?}",
                block.name(),
                bytes.len(),
                &bytes[..bytes.len().min(32)]
            );
        }
        Payload::Keys(keys) => {
            println!("{indent}{} ({} keys)", block.name(), keys.len());
            for key in keys {
                println!(
                    "{indent}  time={} kind={:?} data={:?} value={}",
                    key.time(),
                    key.kind(),
                    key.data(),
                    key.value()
                );
            }
        }
    }
}

fn main() {
    let sqpack = std::env::var("SQPACK").unwrap_or_else(|_| SQPACK.to_owned());
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(sqpack)));
    let path = std::env::args().nth(1).expect("path");
    let bytes = ironworks.file::<Vec<u8>>(&path).expect("read");
    let avfx = Avfx::read(std::io::Cursor::new(bytes)).expect("parse");

    println!("version = {:#x}", avfx.version());
    println!();
    println!("=== properties ({}) ===", avfx.properties().len());
    for block in avfx.properties() {
        dump_block(block, 0);
    }

    println!();
    println!("=== schedulers ({}) ===", avfx.schedulers().len());
    for (i, sched) in avfx.schedulers().iter().enumerate() {
        println!("scheduler[{i}]:");
        for block in sched.properties() {
            dump_block(block, 1);
        }
        for (j, item) in sched.items().iter().enumerate() {
            println!("  item[{j}]:");
            for block in item.blocks() {
                dump_block(block, 2);
            }
        }
        for (j, trigger) in sched.triggers().iter().enumerate() {
            println!("  trigger[{j}]:");
            for block in trigger.blocks() {
                dump_block(block, 2);
            }
        }
    }

    println!();
    println!("=== timelines ({}) ===", avfx.timelines().len());
    for (i, tl) in avfx.timelines().iter().enumerate() {
        println!("timeline[{i}]:");
        for block in tl.properties() {
            dump_block(block, 1);
        }
        for (j, item) in tl.items().iter().enumerate() {
            println!("  item[{j}]:");
            for block in item.blocks() {
                dump_block(block, 2);
            }
        }
        for (j, clip) in tl.clips().iter().enumerate() {
            println!(
                "  clip[{j}]: kind={:?} ints={:?} floats={:?}",
                clip.kind(),
                clip.integers(),
                clip.floats()
            );
        }
    }

    println!();
    println!("=== emitters ({}) ===", avfx.emitters().len());
    for (i, em) in avfx.emitters().iter().enumerate() {
        println!("emitter[{i}]:");
        for block in em.properties() {
            dump_block(block, 1);
        }
        for (j, item) in em.particles().iter().enumerate() {
            println!("  particle-item[{j}]:");
            for block in item.blocks() {
                dump_block(block, 2);
            }
        }
        for (j, item) in em.emitters().iter().enumerate() {
            println!("  emitter-item[{j}]:");
            for block in item.blocks() {
                dump_block(block, 2);
            }
        }
    }

    println!();
    println!("=== particles ({}) ===", avfx.particles().len());
    for (i, ptcl) in avfx.particles().iter().enumerate() {
        println!("particle[{i}]:");
        for block in ptcl.blocks() {
            dump_block(block, 1);
        }
    }

    println!();
    println!("=== effectors ({}) ===", avfx.effectors().len());
    for (i, ef) in avfx.effectors().iter().enumerate() {
        println!("effector[{i}]:");
        for block in ef.blocks() {
            dump_block(block, 1);
        }
    }

    println!();
    println!("=== binders ({}) ===", avfx.binders().len());
    for (i, bd) in avfx.binders().iter().enumerate() {
        println!("binder[{i}]:");
        for block in bd.blocks() {
            dump_block(block, 1);
        }
    }

    println!();
    println!("=== textures ({}) ===", avfx.textures().len());
    for (i, tex) in avfx.textures().iter().enumerate() {
        println!("texture[{i}]: {tex}");
    }

    println!();
    println!("=== models ({}) ===", avfx.models().len());
}
