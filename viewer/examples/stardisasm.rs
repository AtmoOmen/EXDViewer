//! Scratch tool: disassemble a .dxbc file dumped by starfind and print its reflection + SM5 text.
//!
//! `stardisasm <file.dxbc>`

use dxbc::chunks::ChunkData;

fn main() {
    let path = std::env::args().nth(1).expect("a .dxbc path");
    let bytes = std::fs::read(&path).unwrap();
    for container in dxbc::scan_dxbc(&bytes) {
        for chunk in &container.chunks {
            match chunk.parse() {
                ChunkData::Rdef(rdef) => {
                    println!("== RDEF ==");
                    for cb in &rdef.constant_buffers {
                        println!("cbuffer {} ({} bytes)", cb.name, cb.size);
                        for v in &cb.variables {
                            println!("  {} +{} ({} B)", v.name, v.offset, v.size);
                        }
                    }
                    for r in &rdef.bindings {
                        println!(
                            "resource {} type={} slot={}",
                            r.name, r.input_type, r.bind_point
                        );
                    }
                }
                ChunkData::InputSignature(sig) => {
                    println!("== ISGN ==");
                    for e in &sig.elements {
                        println!(
                            "  {} idx={} reg={} mask={:#x}",
                            e.semantic_name, e.semantic_index, e.register, e.mask
                        );
                    }
                }
                ChunkData::OutputSignature(sig) => {
                    println!("== OSGN ==");
                    for e in &sig.elements {
                        println!(
                            "  {} idx={} reg={} mask={:#x}",
                            e.semantic_name, e.semantic_index, e.register, e.mask
                        );
                    }
                }
                ChunkData::Shader(program) => {
                    println!("== SHEX ==");
                    println!("{}", dxbc::shex::format_program(&program));
                }
                _ => {}
            }
        }
    }
}
