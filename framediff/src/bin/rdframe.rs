//! Takes the presented frame and the camera out of a converted RenderDoc capture.
//!
//!   rdframe <capture.zip.xml> --level=<lvb path> [--time=HH:MM] [--weather=N] --out=<dir>
//!
//! Writes the frame as a PNG, what the capture states as JSON, and a TitleEdit preset that stands
//! this viewer where the game stood. The zone, the hour and the weather are not in the capture in
//! any form this reads, so they are stated here; the camera and the lens come out of its buffers.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use base64::{Engine, prelude::BASE64_STANDARD};
use framediff::capture::Capture;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("rdframe: {why}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let (mut path, mut out, mut level, mut time, mut weather) = (None, None, None, None, 1u32);
    for one in std::env::args().skip(1) {
        match one.strip_prefix("--").and_then(|one| one.split_once('=')) {
            Some(("out", value)) => out = Some(PathBuf::from(value)),
            Some(("level", value)) => level = Some(value.to_owned()),
            Some(("time", value)) => time = Some(clock(value)?),
            Some(("weather", value)) => {
                weather = value.parse().map_err(|_| "--weather wants a number")?;
            }
            Some((name, _)) => return Err(format!("--{name}: no such option")),
            None => path = Some(PathBuf::from(one)),
        }
    }
    let path = path.ok_or("wants a converted capture")?;
    let out = out.ok_or("wants --out")?;
    let held = Capture::open(&path)?;
    fs::create_dir_all(&out).map_err(|why| format!("{}: {why}", out.display()))?;

    let frame = held.frame()?;
    frame
        .save(out.join("frame.png"))
        .map_err(|why| format!("frame.png: {why}"))?;
    println!(
        "frame.png  {}x{} of a {}x{} swapchain",
        frame.width(),
        frame.height(),
        held.extent.0,
        held.extent.1
    );

    let cameras = held.cameras()?;
    for camera in &cameras {
        println!(
            "camera     eye ({:.3}, {:.3}, {:.3})  looking ({:.3}, {:.3}, {:.3})  \
             fov {:.2} vertical  near {}  x{}",
            camera.eye.x,
            camera.eye.y,
            camera.eye.z,
            camera.forward.x,
            camera.forward.y,
            camera.forward.z,
            camera.fov,
            camera.near,
            camera.copies,
        );
    }
    let facts = serde_json::json!({
        "capture": held.name,
        "swapchain": [held.extent.0, held.extent.1],
        "thumbnail": [held.thumbnail.0, held.thumbnail.1],
        "cameras": cameras.iter().map(|camera| serde_json::json!({
            "eye": camera.eye.to_array(),
            "forward": camera.forward.to_array(),
            "fov": camera.fov,
            "aspect": camera.aspect,
            "near": camera.near,
            "copies": camera.copies,
        })).collect::<Vec<_>>(),
    });
    fs::write(
        out.join("capture.json"),
        serde_json::to_string_pretty(&facts).map_err(|why| why.to_string())?,
    )
    .map_err(|why| why.to_string())?;

    let Some(camera) = cameras.first() else {
        return Err("no camera in the frame's own writes: nothing to stand the viewer by".to_owned());
    };
    let Some(level) = level else {
        println!("no --level, so no preset was written");
        return Ok(());
    };
    let toward = camera.eye + camera.forward.normalize_or_zero() * 10.0;
    let preset = serde_json::json!({
        "Name": held.name,
        "TerritoryPath": level.trim_start_matches("bg/").trim_end_matches(".lvb"),
        "CameraPos": { "X": camera.eye.x, "Y": camera.eye.y, "Z": camera.eye.z },
        "FixOnPos": { "X": toward.x, "Y": toward.y, "Z": toward.z },
        "FovY": camera.fov,
        "WeatherId": weather,
        "TimeOffset": time.unwrap_or(1200),
    });
    let text = serde_json::to_string(&preset).map_err(|why| why.to_string())?;
    fs::write(out.join("preset.json"), &text).map_err(|why| why.to_string())?;
    fs::write(
        out.join("preset.te3"),
        format!("TE3{}", BASE64_STANDARD.encode(&text)),
    )
    .map_err(|why| why.to_string())?;
    println!("preset.te3 {level} at {} weather {weather}", stated(time));
    Ok(())
}

fn stated(time: Option<u32>) -> String {
    let held = time.unwrap_or(1200);
    format!("{:02}:{:02}", held / 100, held % 100)
}

/// The `hhmm` offset a preset states an hour by.
fn clock(text: &str) -> Result<u32, String> {
    let (hours, minutes) = text.split_once(':').ok_or("--time wants HH:MM")?;
    let hours: u32 = hours.parse().map_err(|_| "--time wants HH:MM")?;
    let minutes: u32 = minutes.parse().map_err(|_| "--time wants HH:MM")?;
    Ok(hours % 24 * 100 + minutes % 60)
}

