#!/usr/bin/env bun
// Shoots a zone over and over while it streams, so a fault that lasts a few frames is caught rather
// than waited out. Serves `viewer/dist` on a port of its own, stands the view where a preset says,
// and writes a png and the frame's own state beside each other.
//
//   CHROMIUM=$(...) bun smoke/burst.ts --level=<path.lvb> --preset=<file> --shots=40 --every=3000

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
const level = flag("level", "");
const preset = flag("preset", "");
const shots = Number(flag("shots", "40"));
const every = Number(flag("every", "3000"));
const outDir = join(root, "smoke", flag("out", "burst"));
const [WIDTH, HEIGHT] = flag("size", "1600x1000").split("x").map(Number);

const sleep = (ms: number) => new Promise((ok) => setTimeout(ok, ms));

function serve() {
    return Bun.serve({
        port: 0,
        fetch(request) {
            const url = new URL(request.url);
            const asked = join(dist, decodeURIComponent(url.pathname));
            if (asked.startsWith(dist) && !asked.endsWith("/") && existsSync(asked)) {
                return new Response(Bun.file(asked));
            }
            return new Response(Bun.file(join(dist, "index.html")), {
                headers: { "content-type": "text/html" },
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

async function press(cdp: any, x: number, y: number) {
    const at = { x, y, button: "left", clickCount: 1, buttons: 1 };
    await cdp.send("Input.dispatchMouseEvent", { ...at, type: "mouseMoved", buttons: 0 });
    await sleep(60);
    await cdp.send("Input.dispatchMouseEvent", { ...at, type: "mousePressed" });
    await sleep(40);
    await cdp.send("Input.dispatchMouseEvent", { ...at, type: "mouseReleased", buttons: 0 });
}

async function main() {
    if (!existsSync(join(dist, "index.html"))) throw new Error(`no build at ${dist}`);
    mkdirSync(outDir, { recursive: true });
    const server = serve();
    const origin = `http://127.0.0.1:${server.port}`;
    const profile = mkdtempSync(join(tmpdir(), "xiviewer-burst-"));
    const { child, port } = await launch(profile);
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const cdp = await Cdp.connect(
        targets.find((one: any) => one.type === "page").webSocketDebuggerUrl,
    );
    cdp.on("Runtime.consoleAPICalled", (p: any) => {
        const line = p.args.map(text).join(" ");
        if (/ERROR|error|warn|WARN|PROBE/.test(line)) console.log(`   | ${line.slice(0, 300)}`);
    });
    cdp.on("Runtime.exceptionThrown", (p: any) => {
        const held = p.exceptionDetails ?? {};
        console.log(`   !! ${String(held.exception?.description ?? held.text).slice(0, 400)}`);
    });
    try {
        await cdp.send("Runtime.enable");
        await cdp.send("Page.enable");
        await cdp.send("Log.enable");
        await cdp.send("Emulation.setDeviceMetricsOverride", {
            width: WIDTH, height: HEIGHT, deviceScaleFactor: 1, mobile: false,
        });
        await cdp.send("Page.navigate", { url: `${origin}/assets/${level}` });
        await cdp.eval("localStorage.clear()").catch(() => {});
        const began = Date.now();
        // The scene is a tab over the level's own listing, and that tab only exists once the file
        // has been read. Pressed until the viewer reports a frame, since a press timed against a
        // guess lands on the listing and every shot after it is of the wrong panel.
        for (let step = 0; step < 20; step++) {
            await press(cdp, 287, 116);
            await sleep(4000);
            const held = await cdp.eval("JSON.parse(window.__frame ?? 'null')").catch(() => null);
            if (held) break;
        }
        if (preset) {
            const held = readFileSync(preset, "utf8").trim();
            for (let step = 0; step < 2; step++) {
                await press(cdp, WIDTH - 200, 182);
                await sleep(400);
                for (const type of ["keyDown", "keyUp"]) {
                    await cdp.send("Input.dispatchKeyEvent", {
                        type, key: "a", code: "KeyA", windowsVirtualKeyCode: 65, modifiers: 2,
                    });
                }
                await cdp.send("Input.insertText", { text: held });
                await sleep(400);
                for (const type of ["keyDown", "keyUp"]) {
                    await cdp.send("Input.dispatchKeyEvent", {
                        type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r",
                    });
                }
                await sleep(1500);
            }
        }
        for (let at = 0; at < shots; at++) {
            await sleep(every);
            const state = await cdp.eval("JSON.parse(window.__frame ?? 'null')").catch(() => null);
            const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
            const name = String(at).padStart(3, "0");
            writeFileSync(join(outDir, `${name}.png`), Buffer.from(shot.data, "base64"));
            if (state) writeFileSync(join(outDir, `${name}.json`), JSON.stringify(state, null, 2));
            console.log(
                `   ${name}  ${Math.round((Date.now() - began) / 1000)}s  ` +
                `drawn ${state?.drawn}/${state?.placed} models ${state?.models} ` +
                `exposure ${state?.exposure?.toFixed?.(3)}`,
            );
        }
    } finally {
        cdp.close();
        child.kill();
        await server.stop(true);
        rmSync(profile, { recursive: true, force: true });
    }
}

await main();
