#!/usr/bin/env bun
// Opens the character tab and runs a script of clicks, typing and waits over it, shooting after
// each step and echoing every console line that carries a marker. A probe, not a gate.
//
//   CHROMIUM=$(...) bun smoke/drive.ts --out=run --script=steps.json

import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

import { Cdp } from "./cdp.ts";

const here = dirname(new URL(import.meta.url).pathname);
const root = resolve(here, "..");
const dist = resolve(root, "viewer/dist");

const argv = Bun.argv.slice(2);
const flag = (name: string, fallback: string) =>
    argv.find((one) => one.startsWith(`--${name}=`))?.slice(name.length + 3) ?? fallback;
const outDir = join(root, "smoke", flag("out", "drive"));
const script = flag("script", "");
const mark = flag("mark", "gate:");
const path = flag("path", "/character");
const settle = Number(flag("settle", "25000"));
const [WIDTH, HEIGHT] = flag("size", "1600x1000").split("x").map(Number);

const sleep = (ms: number) => new Promise((ok) => setTimeout(ok, ms));

function serve() {
    return Bun.serve({
        port: Number(flag("port", "9087")),
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
    const deadline = Date.now() + 60_000;
    while (!existsSync(portFile) && Date.now() < deadline) await sleep(200);
    await sleep(300);
    return { child, port: readFileSync(portFile, "utf8").split("\n")[0].trim() };
}

function text(argument: any): string {
    if (argument?.value !== undefined) return String(argument.value);
    if (argument?.description !== undefined) return String(argument.description);
    return argument?.type ?? "";
}

// Under a software renderer a loading character paints at a frame or two a second, and egui reads
// the pointer where the last frame left it: a press dispatched a few milliseconds behind the move
// is answered by whatever was under the old position. Every gap here is several frames wide.
async function click(cdp: Cdp, x: number, y: number) {
    const base = { x, y, button: "left", clickCount: 1, buttons: 1 };
    await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseMoved", buttons: 0 });
    await sleep(1500);
    await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mousePressed" });
    await sleep(400);
    await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseReleased", buttons: 0 });
    await sleep(1200);
}

async function wheel(cdp: Cdp, x: number, y: number, by: number) {
    // egui scrolls whatever the pointer is over, and it only knows where that is once a move has
    // been dispatched: a wheel on its own arrives with the pointer still off the panel.
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y, buttons: 0 });
    await sleep(80);
    for (let step = 0; step < 6; step++) {
        await cdp.send("Input.dispatchMouseEvent", {
            type: "mouseWheel",
            x,
            y,
            deltaX: 0,
            deltaY: by,
        });
        await sleep(90);
    }
    await sleep(300);
}

async function shot(cdp: Cdp, name: string) {
    const held = await cdp.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(outDir, `${name}.png`), Buffer.from(held.data, "base64"));
    console.log(`   ${name}.png`);
}

async function main() {
    const server = serve();
    const origin = `http://127.0.0.1:${server.port}`;
    const profile = mkdtempSync(join(tmpdir(), "xiviewer-drive-"));
    const { child, port } = await launch(profile);
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const cdp = await Cdp.connect(
        targets.find((one: any) => one.type === "page").webSocketDebuggerUrl,
    );
    cdp.on("Runtime.consoleAPICalled", (p: any) => {
        const line = p.args.map(text).join(" ");
        if (line.includes(mark)) console.log(`   | ${line.slice(0, 400)}`);
    });
    cdp.on("Runtime.exceptionThrown", (p: any) => {
        const held = p.exceptionDetails ?? {};
        console.log(`   !! ${String(held.exception?.description ?? held.text).slice(0, 400)}`);
    });
    cdp.on("Log.entryAdded", (p: any) => {
        const held = p.entry ?? {};
        if (held.level === "error" || /panicked at/.test(held.text ?? "")) {
            console.log(`   !! ${held.source}/${held.level}: ${String(held.text).slice(0, 400)}`);
        }
    });
    await cdp.send("Runtime.enable");
    await cdp.send("Page.enable");
    await cdp.send("Log.enable");
    await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: WIDTH,
        height: HEIGHT,
        deviceScaleFactor: 1,
        mobile: false,
    });
    mkdirSync(outDir, { recursive: true });
    try {
        await cdp.send("Page.navigate", { url: `${origin}${path}` });
        await cdp.eval("localStorage.clear()").catch(() => {});
        console.log(`== ${path}, settling ${settle}ms`);
        await sleep(settle);
        await shot(cdp, "00-open");
        const steps = script ? JSON.parse(readFileSync(script, "utf8")) : [];
        for (const [at, step] of steps.entries()) {
            const name = `${String(at + 1).padStart(2, "0")}-${step.name ?? "step"}`;
            console.log(`\n== ${name} ${JSON.stringify(step)}`);
            if (step.click) await click(cdp, step.click[0], step.click[1]);
            if (step.wheel) await wheel(cdp, step.wheel[0], step.wheel[1], step.wheel[2]);
            if (step.key) {
                for (const type of ["keyDown", "keyUp"]) {
                    await cdp.send("Input.dispatchKeyEvent", {
                        type,
                        key: step.key,
                        code: step.key === "Enter" ? "Enter" : undefined,
                        windowsVirtualKeyCode: step.key === "Enter" ? 13 : undefined,
                        modifiers: step.modifiers ?? 0,
                    });
                }
                await sleep(300);
            }
            if (step.type !== undefined) {
                await cdp.send("Input.insertText", { text: step.type });
                await sleep(600);
            }
            await sleep(step.wait ?? 2000);
            if (step.shot !== false) await shot(cdp, name);
        }
    } finally {
        cdp.close();
        child.kill();
        server.stop(true);
        rmSync(profile, { recursive: true, force: true });
    }
}

await main();
