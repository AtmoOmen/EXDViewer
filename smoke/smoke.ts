#!/usr/bin/env bun
// Drives the real wasm build in a real browser and fails on GL errors, panics and ERROR logs.
// Run it through smoke/run.sh, which resolves chromium and builds the app first.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

import { Cdp } from "./cdp.ts";

const here = dirname(new URL(import.meta.url).pathname);
const root = resolve(here, "..");
const dist = resolve(root, "viewer/dist");

const args = new Set(Bun.argv.slice(2));
const shots = args.has("--shots") || args.has("--explore");
const explore = args.has("--explore");
const modelOnly = explore || args.has("--model-only");
const effectsOnly = args.has("--avfx-only");
const shotDir = join(root, "smoke/shots");

function flag(name: string, fallback: string): string {
    const held = Bun.argv.find((argument) => argument.startsWith(`--${name}=`));
    return held ? held.slice(name.length + 3) : fallback;
}

const MODEL = flag("model", "bg/ex1/01_roc_r2/dun/r2d1/bgparts/r2d1_u1_yam04.mdl");
const SCENE = flag("scene", "bg/ex1/01_roc_r2/dun/r2d1/level/bg.lgb");

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
// sweep walks right from `SWEEP_FROM` across the channel labels that appear beside it. Recalibrate
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

function record(where: string, source: string, level: string, text: string) {
    const message: Message = { where: phase, source, level, text };
    if (/assets\/avfx:/.test(text)) noted.push(message);
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

async function shot(cdp: Cdp, name: string, clip?: any): Promise<string> {
    const params: Record<string, unknown> = { format: "png" };
    if (clip) params.clip = { ...clip, scale: 1 };
    const result = await cdp.send("Page.captureScreenshot", params);
    if (shots) {
        mkdirSync(shotDir, { recursive: true });
        writeFileSync(join(shotDir, `${name}.png`), Buffer.from(result.data, "base64"));
    }
    return Bun.hash(result.data).toString(16);
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

async function main() {
    if (!existsSync(join(dist, "index.html"))) {
        throw new Error(`no build at ${dist} (run smoke/run.sh, which builds first)`);
    }

    const server = serve();
    const origin = `http://127.0.0.1:${server.port}`;
    const profile = mkdtempSync(join(tmpdir(), "exdviewer-smoke-"));
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
        await cdp.send("Page.navigate", { url: `${origin}/assets/${path}` });
        await waitFor("the effect to be titled", 180_000, async () => {
            const title = await cdp.eval<string>("document.title").catch(() => "");
            return title.includes(name);
        });
        // Long enough for the two apricot packages, which are 20 and 40 MiB, and for the textures.
        await sleep(12000);
        const held = `05-avfx-${String(index).padStart(2, "0")}-${name.replace(/\.avfx$/, "")}`;
        const seen: string[] = [];
        for (const part of [0.3, 0.6]) {
            await click(cdp, SEEK.from + (SEEK.to - SEEK.from) * part, SEEK.y);
            await sleep(1500);
            seen.push(await shot(cdp, `${held}-${part}`, PREVIEW));
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
        report.renderer = booted.renderer;
        report.samples = booted.samples;

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
        await sleep(6000);
        await shot(cdp, "01-model");
        const plain = await counters(cdp);
        console.log(`   after load: draws=${plain.draws} links=${plain.links} blits=${plain.blits}`);
        report.plain = plain;

        if (modelOnly) {
            console.log("\n== stopping after the model, before any click");
            return;
        }

        phase = "shaders";
        console.log("\n== game shaders");
        await click(cdp, ROW_LEFT, ROW_Y);
        await sleep(1000);
        await shot(cdp, "02-shaded");

        // The G-buffer binding is what says the deferred path ran, and unlike the blit it survives
        // whatever the composite ends up doing.
        await waitFor("the game shaders to link and bind a G-buffer", 180_000, async () => {
            const c = await counters(cdp);
            return c.links > plain.links && c.drawBuffers > plain.drawBuffers;
        });
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
        for (let x = SWEEP_FROM; x <= SWEEP_TO; x += SWEEP_STEP) {
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

        phase = "scene";
        console.log(`\n== scene: ${SCENE}`);
        await cdp.send("Page.navigate", { url: `${origin}/assets/${SCENE}` });
        await waitFor("the scene file to be titled", 180_000, async () => {
            const title = await cdp.eval<string>("document.title").catch(() => "");
            return title.includes(SCENE.split("/").pop()!);
        });
        await sleep(3000);
        await click(cdp, SCENE_TAB.x, SCENE_TAB.y);
        await waitFor("the scene to draw its instances", 300_000, async () => {
            const c = await counters(cdp).catch(() => ({}) as any);
            return (c.instanced ?? 0) > 0;
        });
        await sleep(8000);
        await shot(cdp, "04-scene");
        const scene = await counters(cdp);
        console.log(`   instanced draws: ${scene.instanced}  links: ${scene.links}`);
        report.scene = scene;

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
