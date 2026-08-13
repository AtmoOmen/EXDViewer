#!/usr/bin/env bun
// Opens each model named on the command line under game shaders, parks on one channel and shoots
// the viewport. Console lines carrying a marker prefix are echoed. This is a probe, not a gate.
//
//   CHROMIUM=$(...) bun smoke/probe.ts --out=probe --mark=probe: <path.mdl> ...

import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

import { Cdp } from "./cdp.ts";

const here = dirname(new URL(import.meta.url).pathname);
const root = resolve(here, "..");
const dist = resolve(root, "viewer/dist");

const argv = Bun.argv.slice(2);
const flag = (name: string, fallback: string) =>
    argv.find((one) => one.startsWith(`--${name}=`))?.slice(name.length + 3) ?? fallback;
const models = argv.filter((one) => !one.startsWith("--"));
const outDir = join(root, "smoke", flag("out", "probe"));
const mark = flag("mark", "probe:");
const channel = Number(flag("channel", "-1"));
const toggle = Number(flag("toggle", "-1"));
// Points to click after the channel, as x,y pairs. Anything outside the toolbar row -- a control in
// a panel the row opens -- is only reachable this way.
const points = flag("click", "")
    .split(",")
    .filter((one) => one !== "")
    .map(Number);
const zoom = Number(flag("zoom", "0"));
const settle = Number(flag("settle", "8000"));
const wait = Number(flag("wait", "9000"));
const hold = Number(flag("hold", "2500"));
const shaded = !argv.includes("--plain");

const [WIDTH, HEIGHT] = flag("size", "1600x1000").split("x").map(Number);
const ROW_Y = 116;
const ROW_LEFT = 268;

const sleep = (ms: number) => new Promise((ok) => setTimeout(ok, ms));

async function waitFor(what: string, timeoutMs: number, probe: () => Promise<boolean>) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
        if (await probe()) return;
        await sleep(250);
    }
    throw new Error(`timed out waiting for ${what}`);
}

function serve() {
    return Bun.serve({
        port: 0,
        async fetch(request) {
            const asked = join(dist, decodeURIComponent(new URL(request.url).pathname));
            if (asked.startsWith(dist) && !asked.endsWith("/")) {
                const file = Bun.file(asked);
                if (await file.exists()) return new Response(file);
            }
            return new Response(Bun.file(join(dist, "index.html")), {
                headers: { "content-type": "text/html; charset=utf-8" },
            });
        },
    });
}

async function launch(profile: string) {
    const child = Bun.spawn(
        [
            process.env.CHROMIUM ?? "chromium",
            "--headless",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--enable-unsafe-swiftshader",
            "--no-first-run",
            "--no-default-browser-check",
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
    return { child, port: readFileSync(portFile, "utf8").split("\n")[0].trim() };
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

async function shot(cdp: Cdp, name: string, clip?: unknown) {
    const result = await cdp.send("Page.captureScreenshot", {
        format: "png",
        ...(clip ? { clip } : {}),
    });
    mkdirSync(outDir, { recursive: true });
    writeFileSync(join(outDir, `${name}.png`), Buffer.from(result.data, "base64"));
}

function text(argument: any): string {
    if (argument === undefined || argument === null) return "";
    if (argument.value !== undefined) return String(argument.value);
    if (argument.description !== undefined) return String(argument.description);
    return argument.type ?? "";
}

async function main() {
    const server = serve();
    const origin = `http://127.0.0.1:${server.port}`;
    const profile = mkdtempSync(join(tmpdir(), "exdviewer-probe-"));
    const { child, port } = await launch(profile);
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const cdp = await Cdp.connect(
        targets.find((one: any) => one.type === "page").webSocketDebuggerUrl,
    );

    const seen = new Set<string>();
    // The router titles the page the moment it navigates, long before a viewer is on screen. A
    // click sent against a title alone lands on the empty panel and is gone.
    let decoded = 0;
    cdp.on("Runtime.consoleAPICalled", (p: any) => {
        const line = p.args.map(text).join(" ");
        if (line.includes("assets/preview: ") || line.includes("assets/mdl: ")) decoded += 1;
        if (line.includes(mark) && !seen.has(line)) {
            seen.add(line);
            console.log(`   | ${line.slice(0, 400)}`);
        }
        if (/panicked at|ERROR:/.test(line)) console.log(`   ! ${line.slice(0, 300)}`);
    });

    await cdp.send("Runtime.enable");
    await cdp.send("Page.enable");
    await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: WIDTH,
        height: HEIGHT,
        deviceScaleFactor: 1,
        mobile: false,
    });

    try {
        for (const [at, model] of models.entries()) {
            const name = model.replace(/^\//, "").split("/").pop() ?? model;
            console.log(`\n== ${model}`);
            seen.clear();
            const opened = decoded;
            await cdp.send("Page.navigate", { url: model.startsWith("/") ? `${origin}${model}` : `${origin}/assets/${model}` });
            await cdp.eval("localStorage.clear()").catch(() => {});
            await waitFor(`${name} to be titled`, 120_000, async () => {
                const title = await cdp.eval<string>("document.title").catch(() => "");
                return title.toLowerCase().includes(name.toLowerCase());
            });
            await waitFor(`${name} to be decoded`, 120_000, async () => decoded > opened);
            await sleep(wait);
            const tag = `${String(at).padStart(2, "0")}-${name.replace(/\.mdl$/, "")}`;
            // Before the shaders are turned on, so the plain and shaded runs frame alike.
            for (let at = 0; at < zoom; at++) {
                await cdp.send("Input.dispatchMouseEvent", {
                    type: "mouseMoved",
                    x: Math.round(WIDTH * 0.47),
                    y: Math.round(HEIGHT * 0.55),
                    buttons: 0,
                });
                await sleep(120);
                await cdp.send("Input.dispatchMouseEvent", {
                    type: "mouseWheel",
                    x: Math.round(WIDTH * 0.47),
                    y: Math.round(HEIGHT * 0.55),
                    deltaX: 0,
                    deltaY: -120,
                });
                await sleep(400);
            }
            await cdp.send("Input.dispatchMouseEvent", {
                type: "mouseMoved",
                x: Math.round(WIDTH * 0.47),
                y: Math.round(HEIGHT * 0.6),
                buttons: 0,
            });
            await sleep(1500);
            if (shaded) {
                await click(cdp, ROW_LEFT, ROW_Y);
                await sleep(settle);
                if (channel >= 0) {
                    await click(cdp, channel, ROW_Y);
                    await sleep(2500);
                }
            }
            if (toggle >= 0) await click(cdp, toggle, ROW_Y);
            for (let at = 0; at + 1 < points.length; at += 2) {
                await click(cdp, points[at], points[at + 1]);
                await sleep(hold);
                await shot(cdp, `${tag}-click${at / 2}`);
            }
            await shot(cdp, shaded ? `${tag}-${WIDTH}x${HEIGHT}` : `${tag}-plain`);
        }
    } finally {
        cdp.close?.();
        child.kill();
        server.stop(true);
    }
}

await main();
