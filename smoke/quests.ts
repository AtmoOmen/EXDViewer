#!/usr/bin/env bun
// Opens the Quests tab on a quest, clicks its way to the playback view and shoots each step.
//
//   CHROMIUM=$(...) bun smoke/quests.ts --row=65575 --out=play

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
const outDir = join(root, "smoke", flag("out", "play"));
const row = flag("row", "65575");
const wait = Number(flag("wait", "25000"));
const [WIDTH, HEIGHT] = flag("size", "1600x1000").split("x").map(Number);
// Each entry is `x,y[,waitMs]`, clicked in turn with a shot taken after every one.
const clicks = flag("clicks", "")
    .split(";")
    .filter((one) => one.length > 0)
    .map((one) => one.split(",").map(Number));

const sleep = (ms: number) => new Promise((ok) => setTimeout(ok, ms));

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

async function main() {
    const server = serve();
    const origin = `http://127.0.0.1:${server.port}`;
    const profile = mkdtempSync(join(tmpdir(), "xiviewer-quests-"));
    const { child, port } = await launch(profile);
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const cdp = await Cdp.connect(
        targets.find((one: any) => one.type === "page").webSocketDebuggerUrl,
    );
    cdp.on("Runtime.consoleAPICalled", (p: any) => {
        const line = p.args.map(text).join(" ");
        if (/ERROR|WARN|panicked/.test(line)) console.log(`   | ${line.slice(0, 300)}`);
    });
    cdp.on("Runtime.exceptionThrown", (p: any) => {
        const held = p.exceptionDetails ?? {};
        console.log(`   !! ${String(held.exception?.description ?? held.text).slice(0, 600)}`);
    });
    cdp.on("Log.entryAdded", (p: any) => {
        const held = p.entry ?? {};
        if (held.level === "error" || /panicked at|ERROR:/.test(held.text ?? "")) {
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

    const shoot = async (name: string) => {
        const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
        writeFileSync(join(outDir, `${name}.png`), Buffer.from(shot.data, "base64"));
        console.log(`   ${name}.png`);
    };
    const click = async (x: number, y: number) => {
        const base = { x, y, button: "left", clickCount: 1, buttons: 1 };
        await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseMoved", buttons: 0 });
        await sleep(60);
        await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mousePressed" });
        await sleep(40);
        await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseReleased", buttons: 0 });
    };

    try {
        await cdp.send("Page.navigate", { url: `${origin}/quests/${row}` });
        await cdp.eval("localStorage.clear()").catch(() => {});
        await sleep(wait);
        await shoot("00-open");
        for (const [at, [x, y, held]] of clicks.entries()) {
            await click(x, y);
            await sleep(held ?? 4000);
            await shoot(`${String(at + 1).padStart(2, "0")}-click`);
        }
    } finally {
        cdp.close();
        child.kill();
        rmSync(profile, { recursive: true, force: true });
        await server.stop(true);
    }
}

await main();
