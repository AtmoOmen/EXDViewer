#!/usr/bin/env bun
// Drives the real wasm build in a real browser and fails on GL errors, panics and ERROR logs.
// Run it through smoke/run.sh, which resolves chromium and builds the app first.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import zlib from "node:zlib";

import { Cdp } from "./cdp.ts";

const here = dirname(new URL(import.meta.url).pathname);
const root = resolve(here, "..");
const dist = resolve(root, "viewer/dist");

const args = new Set(Bun.argv.slice(2));
const shots = args.has("--shots") || args.has("--explore");
const explore = args.has("--explore");
const modelOnly = explore || args.has("--model-only");
const effectsOnly = args.has("--avfx-only");
const orbit = args.has("--orbit");
const views = args.has("--views");
const shotDir = join(root, "smoke/shots");

function flag(name: string, fallback: string): string {
    const held = Bun.argv.find((argument) => argument.startsWith(`--${name}=`));
    return held ? held.slice(name.length + 3) : fallback;
}

const MODEL = flag("model", "bg/ex1/01_roc_r2/dun/r2d1/bgparts/r2d1_u1_yam04.mdl");
const SCENE = flag("scene", "bg/ex1/01_roc_r2/dun/r2d1/level/bg.lgb");
// A level names the layer groups a zone is built from and the environment it stands under. It
// opens through the Zones tab, which places it as soon as it decodes rather than behind a click.
const LEVEL = flag("level", "bg/ex1/01_roc_r2/dun/r2d1/level/r2d1.lvb");

// How long a scene is left drawing before it is shot. The default is enough to have loaded
// something, which is what the gate asks; a frame whose exposure or fog is being judged wants the
// zone actually filled, and under a software renderer that takes far longer.
const SETTLE = Number(flag("settle", "8000"));

// A spread of effects picked off the corpus: quad sprites only, models only under each of the two
// model kinds, files whose keys reach no node, a powder file, and one that spawns nothing.
const EFFECTS = flag(
    "avfx",
    [
        "vfx/common/eff/m0617_stlp_atkup_c0p.avfx",
        "vfx/common/eff/astro_mk0f.avfx",
        "vfx/common/eff/m0920_cast05_c0k1.avfx",
        "vfx/common/eff/mks_pet_en0t.avfx",
        "vfx/common/eff/ab_2kt024_zetsu_c0x.avfx",
        "vfx/common/eff/ab_chk012c0c.avfx",
        "vfx/common/eff/b1271bom01_o.avfx",
        "vfx/common/eff/ab_2sw031depop0t.avfx",
        "vfx/common/eff/rrp_soulbuff_c0x.avfx",
    ].join(","),
).split(",");

const WIDTH = 1600;
const HEIGHT = 1000;

// The control row sits at the top of the viewer pane. `ROW_LEFT` is where "Game shaders" is, and the
// sweep walks right from `SWEEP_FROM` across the channel labels that appear beside it. "Reset view"
// follows immediately after the last channel and is a plain button rather than a selectable label,
// so a sweep that runs past the channels clicks it, moves the camera, and fails the preview-frame
// comparison later. Where it sits moves with the number of targets the model's program writes, so
// the sweep stops on coverage rather than on an x, and `SWEEP_TO` is only a backstop. Recalibrate
// with `--explore`, which writes screenshots to smoke/shots and stops before any click.
const ROW_Y = 116;
const ROW_LEFT = 268;
const SWEEP_FROM = 325;
const SWEEP_TO = 900;
const SWEEP_STEP = 16;

// The lgb viewer opens on its Tree tab; the 3D scene is the tab beside it.
const SCENE_TAB = { x: 287, y: 116 };

// The effect viewer's playback bar sits on the same row. Clicking its slider both pauses and seeks,
// which is what makes an effect shot land on the same frame every run. `PREVIEW` is the pane above
// the tree, which is the only part of the window worth looking at.
const SEEK = { y: 116, from: 415, to: 1085 };
const PREVIEW = { x: 212, y: 132, width: 1020, height: 495 };

// The model viewport, clear of the control row above it: the row itself carries different labels
// with game shaders on than with them off, so a comparison of the frame has to leave it out.
const VIEWPORT = { x: 215, y: 140, width: 1080, height: 840 };

// Where a drag of the viewport starts, well clear of the control row.
const ORBIT_FROM = { x: 800, y: 600 };
const ORBIT_ANGLES = [220, 220, 220, 220];

// A whole turn in eighths, for the effects. The viewer turns a hundredth of a radian per pixel, and
// a quad lying in a world plane loses its coverage as the camera swings into that plane, so the step
// has to be small enough to land near the minimum.
const SWEEP = { steps: 8, by: 79 };

// The preview path's own debug row, which stands where the channel row does once game shaders are
// on. Recalibrate these with --explore alongside the ones above.
const VIEWS: [string, number][] = [
    ["normals", 349],
    ["geometric", 420],
    ["tangents", 492],
    ["bitangents", 566],
    ["handedness", 648],
];

// SV_Target, SV_Target1..4 and Lit.
const CHANNELS = 6;

type Message = { where: string; source: string; level: string; text: string };

const failures: Message[] = [];
const muted: Message[] = [];
let phase = "startup";

const MUTED_TEXT = [
    /Failed to load resource.*favicon/i,
    /manifest\.json/i,
    /Automatic fallback to software WebGL/i,
    /WEBGL_debug_renderer_info is deprecated/i,
    /GPU stall due to ReadPixels/i,
    // The app asks GitHub for its own release list on startup. Unauthenticated, that runs out of
    // rate limit after a handful of runs, and it says nothing about what the app draws.
    /Error fetching versions/i,
];

// eframe's WebLogger maps Rust's Error level onto console.warn with an "ERROR:" prefix rather than
// console.error, so watching console.error alone would miss every egui_glow GL error.
const FATAL_TEXT = [
    /\bpanicked at\b/,
    /\bERROR:/,
    /GL_INVALID/,
    /GL_OUT_OF_MEMORY/,
    /\bGL error\b/,
    /INVALID_FRAMEBUFFER_OPERATION/,
    /unreachable executed/,
    /RuntimeError: /,
    /The app has crashed/,
];

const noted: Message[] = [];

// The router titles the page the moment it navigates, which is long before the path list has landed
// and a viewer has anything on screen. What says one is up is the line the app logs when it decodes
// the file: a click sent against a title alone lands on the empty panel and is gone.
let decoded = 0;

function record(where: string, source: string, level: string, text: string) {
    const message: Message = { where: phase, source, level, text };
    if (/assets\/avfx:/.test(text)) noted.push(message);
    // The Zones tab decodes a level straight through the layer viewer rather than through the
    // Assets tab's preview wrapper, so it logs under its own line instead of "assets/preview:".
    if (/assets\/(preview: |layer: )/.test(text)) decoded += 1;
    if (MUTED_TEXT.some((pattern) => pattern.test(text))) {
        muted.push(message);
        return;
    }
    const fatal =
        FATAL_TEXT.some((pattern) => pattern.test(text)) ||
        where === "exception" ||
        (level === "error" && source !== "network");
    if (fatal) {
        failures.push(message);
        console.log(`  [fail:${phase}] ${text.split("\n")[0].slice(0, 240)}`);
    } else {
        muted.push(message);
    }
}

const sleep = (ms: number) => new Promise((ok) => setTimeout(ok, ms));

async function waitFor(what: string, timeoutMs: number, probe: () => Promise<boolean>) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
        if (await probe()) return;
        await sleep(250);
    }
    throw new Error(`timed out after ${timeoutMs}ms waiting for ${what}`);
}

function serve() {
    return Bun.serve({
        port: 0,
        async fetch(request) {
            const url = new URL(request.url);
            const asked = join(dist, decodeURIComponent(url.pathname));
            if (asked.startsWith(dist) && !asked.endsWith("/")) {
                const file = Bun.file(asked);
                if (await file.exists()) return new Response(file);
            }
            // Every unknown path is a client route, so hand back the shell.
            return new Response(Bun.file(join(dist, "index.html")), {
                headers: { "content-type": "text/html; charset=utf-8" },
            });
        },
    });
}

async function launch(profile: string) {
    const chromium = process.env.CHROMIUM ?? "chromium";
    const child = Bun.spawn(
        [
            chromium,
            "--headless",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            // Software WebGL2 through ANGLE/SwiftShader. Never pass --disable-gpu here: it takes
            // WebGL away and the renderer silently stops being tested.
            "--enable-unsafe-swiftshader",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-component-extensions-with-background-pages",
            "--hide-scrollbars",
            "--mute-audio",
            `--window-size=${WIDTH},${HEIGHT}`,
            `--user-data-dir=${profile}`,
            "--remote-debugging-port=0",
            "about:blank",
        ],
        { stdout: "pipe", stderr: "pipe" },
    );

    const portFile = join(profile, "DevToolsActivePort");
    await waitFor("chromium's debugging port", 60_000, async () => existsSync(portFile));
    await sleep(300);
    const port = readFileSync(portFile, "utf8").split("\n")[0].trim();
    return { child, port };
}

async function page(port: string) {
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const target = targets.find((t: any) => t.type === "page");
    if (!target) throw new Error("chromium exposed no page target");
    return Cdp.connect(target.webSocketDebuggerUrl);
}

function text(argument: any): string {
    if (argument === undefined || argument === null) return "";
    if (argument.value !== undefined) return String(argument.value);
    if (argument.description !== undefined) return String(argument.description);
    if (argument.unserializableValue !== undefined) return String(argument.unserializableValue);
    return argument.type ?? "";
}

async function screenshot(cdp: Cdp, clip?: any): Promise<Buffer> {
    const params: Record<string, unknown> = { format: "png" };
    if (clip) params.clip = { ...clip, scale: 1 };
    const result = await cdp.send("Page.captureScreenshot", params);
    return Buffer.from(result.data, "base64");
}

async function shot(cdp: Cdp, name: string, clip?: any): Promise<string> {
    const data = await screenshot(cdp, clip);
    if (shots) {
        mkdirSync(shotDir, { recursive: true });
        writeFileSync(join(shotDir, `${name}.png`), data);
    }
    return Bun.hash(data).toString(16);
}

/// Chromium's own screenshot encoder, confirmed against its actual output: 8-bit, non-interlaced,
/// color type 2 (RGB) or 6 (RGBA). Anything else is a chromium change this needs to know about.
function decodePng(png: Buffer): { width: number; height: number; channels: number; pixels: Buffer } {
    let offset = 8;
    let width = 0;
    let height = 0;
    let bitDepth = 0;
    let colorType = 0;
    const idat: Buffer[] = [];
    while (offset < png.length) {
        const length = png.readUInt32BE(offset);
        const kind = png.toString("ascii", offset + 4, offset + 8);
        const data = png.subarray(offset + 8, offset + 8 + length);
        if (kind === "IHDR") {
            width = data.readUInt32BE(0);
            height = data.readUInt32BE(4);
            bitDepth = data.readUInt8(8);
            colorType = data.readUInt8(9);
        } else if (kind === "IDAT") {
            idat.push(data);
        } else if (kind === "IEND") {
            break;
        }
        offset += 12 + length;
    }
    if (bitDepth !== 8 || (colorType !== 2 && colorType !== 6)) {
        throw new Error(`unsupported screenshot PNG: bit depth ${bitDepth}, color type ${colorType}`);
    }
    const channels = colorType === 6 ? 4 : 3;
    const raw = zlib.inflateSync(Buffer.concat(idat));
    const stride = width * channels;
    const pixels = Buffer.alloc(height * stride);
    let pos = 0;
    for (let y = 0; y < height; y++) {
        const filterType = raw[pos++];
        const row = raw.subarray(pos, pos + stride);
        pos += stride;
        const out = pixels.subarray(y * stride, (y + 1) * stride);
        const prior = y > 0 ? pixels.subarray((y - 1) * stride, y * stride) : undefined;
        for (let x = 0; x < stride; x++) {
            const a = x >= channels ? out[x - channels] : 0;
            const b = prior ? prior[x] : 0;
            const c = prior && x >= channels ? prior[x - channels] : 0;
            let value = row[x];
            switch (filterType) {
                case 1:
                    value += a;
                    break;
                case 2:
                    value += b;
                    break;
                case 3:
                    value += (a + b) >> 1;
                    break;
                case 4: {
                    const p = a + b - c;
                    const pa = Math.abs(p - a);
                    const pb = Math.abs(p - b);
                    const pc = Math.abs(p - c);
                    value += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
                    break;
                }
            }
            out[x] = value & 0xff;
        }
    }
    return { width, height, channels, pixels };
}

// How far a channel can move before a pixel counts as changed, and how much of the frame can
// change that way before it is a regression rather than an idle animation's own silhouette moving
// against a fixed camera and a fixed background.
const CHANNEL_TOLERANCE = 24;
const CHANGED_TOLERANCE = 0.12;

/// The share of pixels that moved more than a small per-channel amount between two shots of the
/// same clip. Used only once a model never held still to begin with, since an idle animation never
/// stops moving and an exact match is not the thing to ask of it.
function changedFraction(a: Buffer, b: Buffer): number {
    const left = decodePng(a);
    const right = decodePng(b);
    if (left.width !== right.width || left.height !== right.height) {
        return 1;
    }
    const pixels = left.width * left.height;
    let changed = 0;
    for (let at = 0; at < pixels; at++) {
        const lo = at * left.channels;
        const ro = at * right.channels;
        let worst = 0;
        for (let c = 0; c < 3; c++) {
            worst = Math.max(worst, Math.abs(left.pixels[lo + c] - right.pixels[ro + c]));
        }
        if (worst > CHANNEL_TOLERANCE) changed++;
    }
    return changed / pixels;
}

/// The preview frame, once it has held still across two intervals rather than one, and whether it
/// ever did. A model's textures, its color table and its imc arrive on requests of their own over
/// several seconds and each lands on geometry already on screen, so a frame that has only matched
/// once can still be waiting on the last of them and is not what anything should be compared
/// against. A character model plays an idle animation that never holds still at all: `converged`
/// says which case this run is in, so the caller can ask an exact match only where one is possible.
async function settled(cdp: Cdp): Promise<{ data: Buffer; hash: string; converged: boolean }> {
    const save = (data: Buffer) => {
        if (shots) {
            mkdirSync(shotDir, { recursive: true });
            writeFileSync(join(shotDir, "01-preview.png"), data);
        }
    };
    let held = await screenshot(cdp, VIEWPORT);
    let hash = Bun.hash(held).toString(16);
    save(held);
    let same = 0;
    for (let at = 0; at < 20; at++) {
        await sleep(1500);
        const next = await screenshot(cdp, VIEWPORT);
        const nextHash = Bun.hash(next).toString(16);
        same = nextHash === hash ? same + 1 : 0;
        held = next;
        hash = nextHash;
        save(held);
        if (same >= 2) {
            return { data: held, hash, converged: true };
        }
    }
    return { data: held, hash, converged: false };
}

async function counters(cdp: Cdp) {
    return await cdp.eval("JSON.parse(JSON.stringify(window.__smoke ?? {}))");
}

async function click(cdp: Cdp, x: number, y: number) {
    const base = { x, y, button: "left", clickCount: 1, buttons: 1 };
    await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseMoved", buttons: 0 });
    await sleep(60);
    await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mousePressed" });
    await sleep(40);
    await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseReleased", buttons: 0 });
    await sleep(160);
}

/// Drags the viewport, which is how the camera is turned. The move is stepped: the viewer reads a
/// pointer delta per frame, and one jump would be a single frame's worth of turn.
async function drag(cdp: Cdp, by: number) {
    const base = { button: "left", clickCount: 1 };
    await cdp.send("Input.dispatchMouseEvent", {
        ...base,
        type: "mouseMoved",
        ...ORBIT_FROM,
        buttons: 0,
    });
    await cdp.send("Input.dispatchMouseEvent", {
        ...base,
        type: "mousePressed",
        ...ORBIT_FROM,
        buttons: 1,
    });
    for (let step = 1; step <= 10; step++) {
        await cdp.send("Input.dispatchMouseEvent", {
            ...base,
            type: "mouseMoved",
            x: ORBIT_FROM.x + (by * step) / 10,
            y: ORBIT_FROM.y,
            buttons: 1,
        });
        await sleep(40);
    }
    await cdp.send("Input.dispatchMouseEvent", {
        ...base,
        type: "mouseReleased",
        x: ORBIT_FROM.x + by,
        y: ORBIT_FROM.y,
        buttons: 0,
    });
    await sleep(400);
}

async function main() {
    if (!existsSync(join(dist, "index.html"))) {
        throw new Error(`no build at ${dist} (run smoke/run.sh, which builds first)`);
    }

    const server = serve();
    const origin = `http://127.0.0.1:${server.port}`;
    const profile = mkdtempSync(join(tmpdir(), "xiviewer-smoke-"));
    const { child, port } = await launch(profile);
    const cdp = await page(port);

    let crashed = false;
    cdp.on("Runtime.consoleAPICalled", (p) =>
        record("console", "console", p.type, p.args.map(text).join(" ")),
    );
    cdp.on("Log.entryAdded", (p) => record("log", p.entry.source, p.entry.level, p.entry.text));
    cdp.on("Runtime.exceptionThrown", (p) =>
        record(
            "exception",
            "exception",
            "error",
            p.exceptionDetails.exception?.description ?? p.exceptionDetails.text,
        ),
    );
    cdp.on("Inspector.targetCrashed", () => {
        crashed = true;
    });

    await cdp.send("Runtime.enable");
    await cdp.send("Log.enable");
    await cdp.send("Page.enable");
    await cdp.send("Inspector.enable");
    await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: WIDTH,
        height: HEIGHT,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
        source: readFileSync(join(here, "instrument.js"), "utf8"),
    });

    const report: Record<string, unknown> = {};

    // One effect: opened by URL, given time to fetch its textures and packages, then shot whole. The
    // preview draws on its own, so nothing here clicks.
    async function effect(path: string, index: number) {
        const name = path.split("/").pop() ?? path;
        phase = `avfx:${name}`;
        console.log(`\n== effect: ${path}`);
        const opened = decoded;
        await cdp.send("Page.navigate", { url: `${origin}/assets/${path}` });
        // eframe writes egui's memory out as the page unloads, and the details panel's width is in
        // it: an earlier phase leaves the panel wide enough to move the playback bar out from under
        // `SEEK`. The store is gone by the time the wasm has loaded and read it.
        await cdp.eval("localStorage.clear()").catch(() => {});
        await waitFor("the effect to be titled", 180_000, async () => {
            const title = await cdp.eval<string>("document.title").catch(() => "");
            return title.includes(name);
        });
        await waitFor("the effect to be decoded", 180_000, async () => decoded > opened);
        // Long enough for the two apricot packages, which are 20 and 40 MiB, and for the textures.
        await sleep(12000);
        const held = `05-avfx-${String(index).padStart(2, "0")}-${name.replace(/\.avfx$/, "")}`;
        const seen: string[] = [];
        for (const part of [0.3, 0.6]) {
            await click(cdp, SEEK.from + (SEEK.to - SEEK.from) * part, SEEK.y);
            await sleep(1500);
            seen.push(await shot(cdp, `${held}-${part}`, PREVIEW));
            // The clip is what the comparison runs on; the whole window is what says where the
            // playback bar actually is when the clip stops moving.
            if (shots) await shot(cdp, `${held}-${part}-window`);
        }
        // Turned after the seek, so the run is paused and every shot is of the same frame from a
        // different angle.
        if (orbit) {
            for (let at = 0; at < SWEEP.steps; at++) {
                await drag(cdp, SWEEP.by);
                await shot(cdp, `${held}-turn-${at}`, PREVIEW);
            }
        }
        // A navigation resets the counters, so what is drawn is the absolute count, not a delta.
        const after = await counters(cdp);
        console.log(`   draws ${after.draws} links ${after.links}`);
        for (const message of noted.filter((one) => one.where === phase)) {
            console.log(`   ${message.text.split("\n")[0].slice(0, 200)}`);
        }
        if (!after.draws) {
            throw new Error(`${path} drew nothing at all, so this run proves nothing about it`);
        }
        return { path, draws: after.draws, links: after.links, frames: seen };
    }

    /// Every effect in turn. The two shots of one are taken at different points of its own timeline,
    /// so a run where none of them ever differ is one where the seek missed the slider.
    async function effects() {
        const held = [];
        for (const [index, path] of EFFECTS.entries()) {
            held.push(await effect(path, index));
        }
        report.effects = held;
        if (!held.some((one) => one.frames[0] !== one.frames[1])) {
            throw new Error(
                "every effect looked identical at both points of its timeline; the seek never " +
                    "landed on the slider, so these shots are of an arbitrary frame",
            );
        }
    }

    try {
        phase = "boot";
        const first = effectsOnly ? `${origin}/assets/${EFFECTS[0]}` : `${origin}/assets/${MODEL}`;
        console.log(`\n== boot: ${first}`);
        await cdp.send("Page.navigate", { url: first });

        await waitFor("the wasm app to take a GL context", 180_000, async () => {
            const c = await counters(cdp).catch(() => ({}) as any);
            return (c.contexts ?? 0) > 0 && (c.draws ?? 0) > 0;
        });
        const booted = await counters(cdp);
        console.log(`   renderer: ${booted.renderer}`);
        console.log(`   samples: ${booted.samples}  antialias: ${booted.antialias}`);
        console.log(`   canvas depth: ${booted.depth}  bits: ${booted.depthBits}`);
        report.renderer = booted.renderer;
        report.samples = booted.samples;
        report.depthBits = booted.depthBits;

        if (!booted.samples || booted.samples < 2) {
            throw new Error(
                `canvas is single-sampled (SAMPLES=${booted.samples}); the multisample blit bug ` +
                    `cannot reproduce here, so this run would not be a real gate`,
            );
        }

        if (effectsOnly) {
            await effects();
            return;
        }

        phase = "model";
        await waitFor("the model to be titled", 180_000, async () => {
            const title = await cdp.eval<string>("document.title");
            return title.includes("yam04") || title.includes(".mdl");
        });
        await waitFor("the model to be decoded", 180_000, async () => decoded > 0);
        await sleep(6000);
        await shot(cdp, "01-model");
        const plain = await counters(cdp);
        console.log(`   after load: draws=${plain.draws} links=${plain.links} blits=${plain.blits}`);
        report.plain = plain;

        if (orbit) {
            phase = "orbit";
            console.log("\n== orbit");
            for (const [at, by] of ORBIT_ANGLES.entries()) {
                await drag(cdp, by);
                await shot(cdp, `01-orbit-${at}`);
            }
        }

        if (views) {
            phase = "views";
            console.log("\n== preview views");
            for (const [name, x] of VIEWS) {
                await click(cdp, x, ROW_Y);
                await sleep(250);
                await shot(cdp, `01-view-${name}`);
            }
            // Off again, so whatever runs after this sees the shaded frame rather than a debug one.
            await click(cdp, VIEWS[VIEWS.length - 1][1], ROW_Y);
        }

        if (modelOnly) {
            console.log("\n== stopping after the model, before any click");
            return;
        }

        phase = "shaders";
        console.log("\n== game shaders");
        // Kept to compare the frame against once game shaders have been on and off again: a vertex
        // array or a binding the deferred path leaves behind shows here and nowhere else.
        const preview = await settled(cdp);
        await click(cdp, ROW_LEFT, ROW_Y);
        await sleep(1000);
        await shot(cdp, "02-shaded");

        // The G-buffer binding is what says the deferred path ran, and unlike the blit it survives
        // whatever the composite ends up doing.
        await waitFor("the game shaders to link and bind a G-buffer", 180_000, async () => {
            const c = await counters(cdp);
            return c.links > plain.links && c.drawBuffers > plain.drawBuffers;
        });
        // Before any channel is clicked, so the shot is of whichever the viewer starts on.
        await sleep(1500);
        await shot(cdp, "02-started");
        const shaded = await counters(cdp);
        console.log(
            `   after shading: links +${shaded.links - plain.links}` +
                ` blits +${shaded.blits - plain.blits}` +
                ` drawBuffers +${shaded.drawBuffers - plain.drawBuffers}`,
        );
        report.shaded = shaded;

        phase = "channels";
        console.log("\n== channels");
        const rowClip = { x: 0, y: ROW_Y - 14, width: WIDTH, height: 28 };
        const seen = new Set<string>();
        let index = 0;
        for (let x = SWEEP_FROM; x <= SWEEP_TO && seen.size < CHANNELS; x += SWEEP_STEP) {
            await click(cdp, x, ROW_Y);
            // Park the pointer away from the row first, or its hover highlight lands in the clip
            // and two shots of the same selection come out different.
            await cdp.send("Input.dispatchMouseEvent", {
                type: "mouseMoved",
                x: 700,
                y: 600,
                buttons: 0,
            });
            await sleep(250);
            const at = String(index++).padStart(2, "0");
            seen.add(await shot(cdp, `03-channel-${at}`, rowClip));
            // The row clip is what counts distinct selections; the whole frame is what says whether
            // the channel drew anything worth looking at.
            if (shots) await shot(cdp, `03-frame-${at}`);
        }
        console.log(`   distinct selections: ${seen.size}`);
        report.channels = seen.size;
        if (seen.size < CHANNELS) {
            throw new Error(
                `the channel row only ever showed ${seen.size} selections, wanted ${CHANNELS}; ` +
                    `the sweep never reached the targets, so this run proves nothing about them`,
            );
        }

        phase = "preview";
        console.log("\n== back to the preview");
        await click(cdp, ROW_LEFT, ROW_Y);
        await sleep(2000);
        const after = await screenshot(cdp, VIEWPORT);
        if (shots) {
            mkdirSync(shotDir, { recursive: true });
            writeFileSync(join(shotDir, "03-preview.png"), after);
        }
        report.previewConverged = preview.converged;
        if (preview.converged) {
            if (Bun.hash(after).toString(16) !== preview.hash) {
                throw new Error(
                    "the preview frame changed after game shaders were turned on and off again, so " +
                        "the deferred path left state behind that the preview path reads",
                );
            }
            console.log("   the preview frame came back the same");
        } else {
            const fraction = changedFraction(preview.data, after);
            report.previewChanged = fraction;
            console.log(
                `   the preview never held still to begin with (idle animation); ` +
                    `${(fraction * 100).toFixed(2)}% of pixels moved more than tolerance`,
            );
            if (fraction > CHANGED_TOLERANCE) {
                throw new Error(
                    `${(fraction * 100).toFixed(1)}% of the preview's pixels changed after game ` +
                        `shaders were turned on and off again (tolerance ${(CHANGED_TOLERANCE * 100).toFixed(0)}%), ` +
                        `more than the idle animation this model plays accounts for`,
                );
            }
        }

        phase = "scene";
        report.scene = await walk(cdp, origin, SCENE, "04-scene", "assets");

        phase = "level";
        report.level = await walk(cdp, origin, LEVEL, "05-level", "zones");

        await effects();
    } finally {
        if (crashed) failures.push({ where: phase, source: "browser", level: "error", text: "the renderer process crashed" });
        writeFileSync(
            join(root, "smoke/last-run.json"),
            JSON.stringify({ report, failures, noted, muted }, null, 2),
        );
        cdp.close();
        child.kill();
        await server.stop(true);
        rmSync(profile, { recursive: true, force: true });
    }
}

/// A layer file opened and left drawing long enough to have loaded something. An lgb opens in the
/// Assets tab and needs its 3D tab clicked; the lvb naming it opens in the Zones tab, which places
/// the scene itself rather than showing it behind a tree/scene toggle.
async function walk(cdp: Cdp, origin: string, path: string, name: string, route: string) {
    console.log(`\n== ${phase}: ${path}`);
    const opened = decoded;
    await cdp.send("Page.navigate", { url: `${origin}/${route}/${path}` });
    await waitFor(`${path} to be titled`, 180_000, async () => {
        const title = await cdp.eval<string>("document.title").catch(() => "");
        return title.includes(path.split("/").pop()!);
    });
    await waitFor(`${path} to be decoded`, 180_000, async () => decoded > opened);
    await sleep(3000);
    const before = await counters(cdp);
    if (route === "assets") {
        await click(cdp, SCENE_TAB.x, SCENE_TAB.y);
    }
    await waitFor("the scene to draw its instances", 300_000, async () => {
        const c = await counters(cdp).catch(() => ({}) as any);
        return (c.instanced ?? 0) > before.instanced;
    });
    await sleep(SETTLE);
    await shot(cdp, name);
    const held = await counters(cdp);
    console.log(`   instanced draws: ${held.instanced}  links: ${held.links}`);
    return held;
}

/// One entry per distinct message, since a GL error in a paint callback repeats every frame and the
/// same complaint about a hundred assets is still one problem.
function shape(text: string) {
    return text
        .split("\n")[0]
        .trim()
        .replace(/0x[0-9a-f]+/gi, "0x*")
        .replace(/[\w/]+\.(mdl|mtrl|tex|shpk|lgb|sgb|lvb|avfx)\b/gi, "*");
}

function report_failures() {
    const kinds = new Map<string, { count: number; phases: Set<string>; sample: Message }>();
    for (const failure of failures) {
        const key = shape(failure.text);
        let kind = kinds.get(key);
        if (!kind) kinds.set(key, (kind = { count: 0, phases: new Set(), sample: failure }));
        kind.count++;
        kind.phases.add(failure.where);
    }
    console.log(
        `FAIL: ${kinds.size} distinct problem(s) across ${failures.length} browser message(s)\n`,
    );
    const worst = [...kinds.values()].sort((a, b) => b.count - a.count);
    for (const kind of worst) {
        console.log(
            `${kind.count}x in ${[...kind.phases].join(", ")} ` +
                `(${kind.sample.source}/${kind.sample.level})`,
        );
        console.log(`  ${kind.sample.text.split("\n").slice(0, 6).join("\n  ")}\n`);
    }
}

main()
    .then(() => {
        console.log(`\n${"=".repeat(60)}`);
        if (failures.length) {
            report_failures();
            process.exit(1);
        }
        console.log("PASS: no GL errors, panics or ERROR logs");
    })
    .catch((error) => {
        console.log(`\n${"=".repeat(60)}`);
        console.log(`FAIL: ${error.message}\n`);
        if (failures.length) report_failures();
        process.exit(1);
    });
