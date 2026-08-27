#!/usr/bin/env bun
// Opens a path on a running server, waits, and shoots the viewport. No gate and no decode wait: a
// zone never settles, so this just looks at whatever is on screen after a while.
//
//   CHROMIUM=$(...) bun smoke/look.ts --origin=http://127.0.0.1:9080 --out=look <path> ...

import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

import { Cdp } from "./cdp.ts";

const here = dirname(new URL(import.meta.url).pathname);
const root = resolve(here, "..");

const argv = Bun.argv.slice(2);
const flag = (name: string, fallback: string) =>
    argv.find((one) => one.startsWith(`--${name}=`))?.slice(name.length + 3) ?? fallback;
const paths = argv.filter((one) => !one.startsWith("--"));
const outDir = join(root, "smoke", flag("out", "look"));
const origin = flag("origin", "http://127.0.0.1:9080");
const wait = Number(flag("wait", "45000"));
const preset = flag("preset", "");
const [WIDTH, HEIGHT] = flag("size", "1600x1000").split("x").map(Number);

const sleep = (ms: number) => new Promise((ok) => setTimeout(ok, ms));

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

async function main() {
    const profile = mkdtempSync(join(tmpdir(), "xiviewer-look-"));
    const { child, port } = await launch(profile);
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const cdp = await Cdp.connect(
        targets.find((one: any) => one.type === "page").webSocketDebuggerUrl,
    );
    cdp.on("Runtime.consoleAPICalled", (p: any) => {
        const line = p.args.map(text).join(" ");
        console.log(`   | ${line.slice(0, 300)}`);
    });
    // A Rust panic reaches the console, but a wasm abort and an out-of-memory do not: they arrive
    // as an uncaught exception, which is a different event. Without this a crash shows up as a
    // screenshot of eframe's reload button and nothing else.
    cdp.on("Runtime.exceptionThrown", (p: any) => {
        const held = p.exceptionDetails ?? {};
        const line = held.exception?.description ?? held.text ?? JSON.stringify(held);
        console.log(`   !! ${String(line).slice(0, 600)}`);
    });
    // Where a worker's own failures surface: `Runtime.consoleAPICalled` is the page's console alone,
    // and the sqpack worker is a target of its own. The gate reads these too, which is how it names
    // a `worker/error` the page never sees.
    cdp.on("Log.entryAdded", (p: any) => {
        const held = p.entry ?? {};
        if (held.level === "error" || /panicked at|ERROR:/.test(held.text ?? "")) {
            console.log(`   !! ${held.source}/${held.level}: ${String(held.text).slice(0, 400)}`);
        }
    });
    cdp.on("Network.responseReceived", (p: any) => {
        if (p.response?.status >= 400) {
            console.log(`   !! ${p.response.status} ${p.response.url}`);
        }
    });
    await cdp.send("Runtime.enable");
    await cdp.send("Page.enable");
    await cdp.send("Log.enable");
    await cdp.send("Network.enable");
    // The same counters the gate asserts, so a shot comes with the GL work behind it rather than
    // leaving a draw that stopped happening to the eye.
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
        source: readFileSync(join(root, "smoke", "instrument.js"), "utf8"),
    });
    await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: WIDTH,
        height: HEIGHT,
        deviceScaleFactor: 1,
        mobile: false,
    });
    mkdirSync(outDir, { recursive: true });
    try {
        for (const [at, path] of paths.entries()) {
            console.log(`\n== ${path}`);
            await cdp.send("Page.navigate", { url: path.startsWith("/") ? `${origin}${path}` : `${origin}/assets/${path}` });
            await cdp.eval("localStorage.clear()").catch(() => {});
            // A level opens on its file listing; the scene is a tab over. Clicked more than once
            // and over a spread of waits: the tab only exists once the file has been read, and a
            // single click timed against a guess lands on the listing and shoots the wrong panel.
            // The tab stays selected, so clicking it again costs nothing.
            const base = { x: 287, y: 116, button: "left", clickCount: 1, buttons: 1 };
            const tab = async () => {
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseMoved", buttons: 0 });
                await sleep(60);
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mousePressed" });
                await sleep(40);
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseReleased", buttons: 0 });
            };
            let waited = 0;
            for (const at of [6000, 6000, 8000, 10000, 10000, 10000]) {
                await sleep(at);
                waited += at;
                await tab();
            }
            // The paste box and the button under it, both docked to the right edge of the window.
            // Typing a preset in there is the one way from outside to stand this view where a
            // capture was taken from: the file dialog beside it opens nothing a browser can drive.
            if (preset) {
                // Repeated, because the box only exists once the scene tab is up and a press that
                // lands between two of a loading zone's frames is simply lost. Loading the same
                // preset twice costs nothing.
                const text = readFileSync(preset, "utf8").trim();
                const click = async (x: number, y: number) => {
                    const at = { x, y, button: "left" as const, clickCount: 1, buttons: 1 };
                    await cdp.send("Input.dispatchMouseEvent", { ...at, type: "mouseMoved", buttons: 0 });
                    await sleep(60);
                    await cdp.send("Input.dispatchMouseEvent", { ...at, type: "mousePressed" });
                    await sleep(40);
                    await cdp.send("Input.dispatchMouseEvent", { ...at, type: "mouseReleased", buttons: 0 });
                };
                for (let step = 0; step < 3; step++) {
                    await click(WIDTH - 200, 113);
                    await sleep(400);
                    // The box keeps what the last press put in it, so a second insert nests one
                    // preset inside another and the parse fails without saying which press did it.
                    for (const type of ["keyDown", "keyUp"]) {
                        await cdp.send("Input.dispatchKeyEvent", {
                            type, key: "a", code: "KeyA", windowsVirtualKeyCode: 65, modifiers: 2,
                        });
                    }
                    await cdp.send("Input.insertText", { text });
                    await sleep(400);
                    // Clicks "Load pasted" rather than sending Enter.
                    await click(WIDTH - 257, 135);
                    await sleep(2500);
                }
            }
            await sleep(Math.max(wait - waited, 0));
            // The panel the environment rows sit in runs past the window, so it is scrolled before
            // the shot: what a zone read out of its files is stated at the bottom of it.
            for (let step = 0; step < Number(flag("scroll", "0")); step++) {
                await cdp.send("Input.dispatchMouseEvent", {
                    type: "mouseWheel",
                    x: 1200,
                    y: 700,
                    deltaX: 0,
                    deltaY: 240,
                });
                await sleep(200);
            }
            const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
            const name = `${String(at).padStart(2, "0")}-${path.split("/").pop()}`;
            writeFileSync(join(outDir, `${name}.png`), Buffer.from(shot.data, "base64"));
            const state = {
                gl: await cdp.eval("JSON.stringify(globalThis.__smoke ?? null)").catch(() => null),
                frame: await cdp.eval("window.__frame ?? null").catch(() => null),
            };
            writeFileSync(join(outDir, `${name}.json`), JSON.stringify(state, null, 2));
            console.log(`   ${name}.png  ${state.gl}`);
        }
    } finally {
        cdp.close();
        child.kill();
        rmSync(profile, { recursive: true, force: true });
    }
}

await main();
