#!/usr/bin/env bun
// Serves the build and shoots one scene several times over, with nothing touched between the shots
// but the clock. A scene that animates nothing gives the same frame every time, which is what says
// the shots are comparable at all.
//
//   CHROMIUM=$(...) bun smoke/glow.ts --out=glow --shots=4 --gap=1500 <path.sgb>

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
const paths = argv.filter((one) => !one.startsWith("--"));
const outDir = join(root, "smoke", flag("out", "glow"));
const settle = Number(flag("settle", "40000"));
const shots = Number(flag("shots", "4"));
const gap = Number(flag("gap", "1500"));
const [WIDTH, HEIGHT] = flag("size", "1100x760").split("x").map(Number);

const sleep = (ms: number) => new Promise((ok) => setTimeout(ok, ms));

function serve() {
    return Bun.serve({
        port: Number(flag("port", "9085")),
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

async function main() {
    const server = serve();
    const origin = `http://127.0.0.1:${server.port}`;
    const profile = mkdtempSync(join(tmpdir(), "xiviewer-glow-"));
    const { child, port } = await launch(profile);
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const cdp = await Cdp.connect(
        targets.find((one: any) => one.type === "page").webSocketDebuggerUrl,
    );
    cdp.on("Runtime.consoleAPICalled", (p: any) => {
        const line = p.args.map(text).join(" ");
        if (/panicked at|ERROR/.test(line)) console.log(`   | ${line.slice(0, 300)}`);
    });
    cdp.on("Runtime.exceptionThrown", (p: any) => {
        const held = p.exceptionDetails ?? {};
        console.log(`   !! ${String(held.exception?.description ?? held.text).slice(0, 400)}`);
    });
    await cdp.send("Runtime.enable");
    await cdp.send("Page.enable");
    await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: WIDTH,
        height: HEIGHT,
        deviceScaleFactor: 1,
        mobile: false,
    });
    mkdirSync(outDir, { recursive: true });
    try {
        for (const path of paths) {
            const name = path.split("/").pop()!.replace(/\.\w+$/, "");
            console.log(`\n== ${path}`);
            await cdp.send("Page.navigate", { url: `${origin}/assets/${path}` });
            await cdp.eval("localStorage.clear()").catch(() => {});
            // The scene is a tab over from the listing a file opens on, and the tab only exists
            // once the file has been read. Clicking it again once it is selected costs nothing.
            const base = { x: 287, y: 116, button: "left", clickCount: 1, buttons: 1 };
            let waited = 0;
            for (const step of [6000, 6000, 8000, 10000, 10000]) {
                await sleep(step);
                waited += step;
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseMoved", buttons: 0 });
                await sleep(60);
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mousePressed" });
                await sleep(40);
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseReleased", buttons: 0 });
            }
            await sleep(Math.max(settle - waited, 0));
            for (let shot = 0; shot < shots; shot++) {
                const held = await cdp.send("Page.captureScreenshot", { format: "png" });
                writeFileSync(join(outDir, `${name}-${shot}.png`), Buffer.from(held.data, "base64"));
                console.log(`   ${name}-${shot}.png`);
                if (shot + 1 < shots) await sleep(gap);
            }
        }
    } finally {
        cdp.close();
        child.kill();
        await server.stop(true);
        rmSync(profile, { recursive: true, force: true });
    }
}

await main();
