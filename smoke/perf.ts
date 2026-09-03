#!/usr/bin/env bun
// Opens a scene, times what it takes to load, then flies the camera and counts frames.
//
//   CHROMIUM=$(...) bun smoke/perf.ts --origin=http://127.0.0.2:9084 --out=perf <path>

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
const outDir = join(root, "smoke", flag("out", "perf"));
const origin = flag("origin", "http://127.0.0.2:9084");
const wait = Number(flag("wait", "60000"));
const spin = Number(flag("spin", "6000"));
const label = flag("label", "");
const logFile = flag("log", "");
const scale = Number(flag("scale", "1"));
const [WIDTH, HEIGHT] = flag("size", "1600x1000").split("x").map(Number);

const sleep = (ms: number) => new Promise((ok) => setTimeout(ok, ms));

// Counters installed before the app runs. `requestAnimationFrame` is what eframe paints on, so a
// wrapped callback counts painted frames and the gaps between them are the frame times.
const PROBE = `
(() => {
  const held = { frames: 0, marks: [], calls: {}, stalls: [], last: 0 };
  window.__perf = held;
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (fn) => raf((at) => { held.frames += 1; held.marks.push(at); if (held.marks.length > 4000) held.marks.shift(); if (held.last && at - held.last > 400) held.stalls.push([Math.round(held.last), Math.round(at - held.last)]); held.last = at; return fn(at); });
  const watched = [
    "drawElements", "drawElementsInstanced", "drawArrays", "drawArraysInstanced",
    "getUniformLocation", "useProgram", "bindTexture", "bindBufferRange", "bindBuffer",
    "disableVertexAttribArray", "vertexAttribPointer", "bufferData", "texImage2D",
    "uniform1i", "uniform2f", "uniform4fv", "uniformMatrix4fv", "bindVertexArray", "linkProgram",
  ];
  for (const kind of [window.WebGL2RenderingContext, window.WebGLRenderingContext]) {
    if (!kind) continue;
    for (const name of watched) {
      const fn = kind.prototype[name];
      if (typeof fn !== "function") continue;
      kind.prototype[name] = function (...args) { held.calls[name] = (held.calls[name] ?? 0) + 1; return fn.apply(this, args); };
    }
  }
})();
`;

const SAMPLE = `(() => { const h = window.__perf; return { frames: h.frames, calls: { ...h.calls }, marks: h.marks.slice() }; })()`;

function quantiles(marks: number[]) {
    const gaps = [];
    for (let at = 1; at < marks.length; at++) gaps.push(marks[at] - marks[at - 1]);
    if (!gaps.length) return { median: 0, p95: 0, worst: 0 };
    gaps.sort((a, b) => a - b);
    const pick = (share: number) => gaps[Math.min(gaps.length - 1, Math.floor(gaps.length * share))];
    return { median: pick(0.5), p95: pick(0.95), worst: gaps[gaps.length - 1] };
}

async function launch(profile: string) {
    const child = Bun.spawn(
        [
            process.env.CHROMIUM ?? "chromium",
            "--headless",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            ...(process.env.SOFTWARE ? ["--enable-unsafe-swiftshader"] : ["--use-gl=angle", "--use-angle=gl", "--ignore-gpu-blocklist", "--enable-gpu"]),
            // Paints are vblank-paced otherwise, which rounds every frame time to a multiple of
            // 16.7 ms and hides any change smaller than that.
            ...(process.env.UNCAPPED ? ["--disable-gpu-vsync", "--disable-frame-rate-limit"] : []),
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

async function main() {
    const profile = mkdtempSync(join(tmpdir(), "xiviewer-perf-"));
    const { child, port } = await launch(profile);
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const cdp = await Cdp.connect(
        targets.find((one: any) => one.type === "page").webSocketDebuggerUrl,
    );
    const failures: string[] = [];
    const timings: string[] = [];
    cdp.on("Runtime.consoleAPICalled", (p: any) => {
        const line = p.args.map((one: any) => String(one?.value ?? one?.description ?? "")).join(" ");
        if (line.includes("TIMING")) timings.push(`${Date.now() - opened} ${line.slice(line.indexOf("TIMING"))}`);
    });
    cdp.on("Runtime.exceptionThrown", (p: any) => {
        const held = p.exceptionDetails ?? {};
        failures.push(String(held.exception?.description ?? held.text ?? "").slice(0, 200));
    });
    cdp.on("Log.entryAdded", (p: any) => {
        const held = p.entry ?? {};
        if (held.level === "error") failures.push(String(held.text).slice(0, 200));
    });

    const moved = new Map<string, { bytes: number; count: number; last: number }>();
    const seen = new Map<string, string>();
    let opened = 0;
    let failed = 0;
    const timeline: Array<[number, number]> = [];
    let carried = 0;
    let outstanding = 0;
    let peak = 0;
    let latency = 0;
    let answered = 0;
    const started = new Map<string, number>();
    // Every request as it happened, so a load's shape can be read back off one run rather than
    // inferred from the totals. Written only where `--log` names a file.
    type Record = { url: string; range: string; at: number; head: number; done: number; bytes: number; status: number; cache: string };
    const logged: Record[] = [];
    const tracked = new Map<string, Record>();
    const samples: Array<{ at: number; live: string[]; frame: string | null }> = [];
    cdp.on("Network.requestWillBeSent", (p: any) => {
        seen.set(p.requestId, p.request.url);
        started.set(p.requestId, Date.now());
        outstanding += 1;
        peak = Math.max(peak, outstanding);
        if (!logFile) return;
        const one = {
            url: p.request.url,
            range: p.request.headers?.Range ?? p.request.headers?.range ?? "",
            at: Date.now() - opened,
            head: -1,
            done: -1,
            bytes: 0,
            status: 0,
            cache: "",
        };
        tracked.set(p.requestId, one);
        logged.push(one);
    });
    cdp.on("Network.responseReceived", (p: any) => {
        const one = tracked.get(p.requestId);
        if (!one) return;
        one.head = Date.now() - opened;
        one.status = p.response.status;
        const headers = p.response.headers ?? {};
        one.cache = headers["cf-cache-status"] ?? headers["Cf-Cache-Status"] ?? "";
    });
    const settled = (id: string) => {
        const at = started.get(id);
        if (at === undefined) return;
        started.delete(id);
        outstanding -= 1;
        latency += Date.now() - at;
        answered += 1;
        const one = tracked.get(id);
        if (one) one.done = Date.now() - opened;
    };
    cdp.on("Network.loadingFailed", (p: any) => { failed += 1; settled(p.requestId); });
    cdp.on("Network.loadingFinished", (p: any) => {
        const one = tracked.get(p.requestId);
        if (one) one.bytes = p.encodedDataLength;
        settled(p.requestId);
        const url = seen.get(p.requestId) ?? "?";
        const kind = url.includes("/api/") ? (url.split("?")[0].split(".").pop() ?? "api") : "app";
        const moving = moved.get(kind) ?? { bytes: 0, count: 0, last: 0 };
        moving.bytes += p.encodedDataLength;
        moving.count += 1;
        moving.last = Date.now() - opened;
        moved.set(kind, moving);
        carried += p.encodedDataLength;
        timeline.push([Date.now() - opened, carried]);
    });

    await cdp.send("Runtime.enable");
    await cdp.send("Page.enable");
    await cdp.send("Log.enable");
    await cdp.send("Network.enable");
    await cdp.send("Network.setCacheDisabled", { cacheDisabled: true });
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source: PROBE });
    await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: WIDTH,
        height: HEIGHT,
        deviceScaleFactor: scale,
        mobile: false,
    });
    const renderer = await cdp.eval(`(() => { const c = document.createElement('canvas'); const g = c.getContext('webgl2'); if (!g) return 'no webgl2'; const d = g.getExtension('WEBGL_debug_renderer_info'); return String(g.getParameter(d ? d.UNMASKED_RENDERER_WEBGL : g.RENDERER)); })()`).catch(() => "unknown");
    console.log(`   gl: ${renderer}`);
    mkdirSync(outDir, { recursive: true });
    try {
        for (const [at, path] of paths.entries()) {
            console.log(`\n== ${label ? `${label} ` : ""}${path}`);
            opened = Date.now();
            moved.clear();
            timeline.length = 0;
            carried = 0;
            failed = 0;
            failures.length = 0;
            timings.length = 0;
            outstanding = 0;
            peak = 0;
            latency = 0;
            answered = 0;
            started.clear();
            logged.length = 0;
            tracked.clear();
            samples.length = 0;
            await cdp.send("Page.navigate", { url: `${origin}/assets/${path}` });
            await cdp.eval("localStorage.clear()").catch(() => {});
            const base = { x: 287, y: 116, button: "left", clickCount: 1, buttons: 1 };
            const tab = async () => {
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseMoved", buttons: 0 });
                await sleep(60);
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mousePressed" });
                await sleep(40);
                await cdp.send("Input.dispatchMouseEvent", { ...base, type: "mouseReleased", buttons: 0 });
            };
            // What was in flight moment to moment, which is what separates a load waiting on the
            // wire from one that has nothing to ask for yet.
            const ticker = logFile
                ? setInterval(() => {
                      samples.push({
                          at: Date.now() - opened,
                          live: [...started.keys()].map((id) => seen.get(id) ?? "?"),
                          frame: null,
                      });
                  }, 250)
                : undefined;
            // Clicked all the way through the wait rather than a fixed few times: the tab only
            // exists once the file has been read, and the server reloads the page from under a run
            // whenever it rebuilds.
            for (let waited = 0; waited < wait; waited += 6000) {
                await sleep(Math.min(6000, wait - waited));
                await tab();
                if (logFile) {
                    const held = await cdp.eval("window.__frame ?? null").catch(() => null);
                    samples.push({ at: Date.now() - opened, live: [], frame: held });
                }
            }
            if (ticker !== undefined) clearInterval(ticker);

            // The pointer has to stand over the viewport for the keys to fly it, and the flight
            // integrates the frame's own delta, so the distance covered is the same however fast
            // the frames come. Out and back, so both runs end where they started.
            const over = { x: 900, y: 600, button: "none", buttons: 0 };
            await cdp.send("Input.dispatchMouseEvent", { ...over, type: "mouseMoved" });
            const key = async (type: string, name: string, code: string, at: number) =>
                cdp.send("Input.dispatchKeyEvent", { type, key: name, code, windowsVirtualKeyCode: at, nativeVirtualKeyCode: at });
            await cdp.eval("window.__perf.marks.length = 0");
            const before = await cdp.eval(SAMPLE);
            const since = Date.now();
            await key("keyDown", "d", "KeyD", 68);
            await sleep(spin / 2);
            await key("keyUp", "d", "KeyD", 68);
            await key("keyDown", "a", "KeyA", 65);
            await sleep(spin / 2);
            await key("keyUp", "a", "KeyA", 65);
            const elapsed = (Date.now() - since) / 1000;
            const after = await cdp.eval(SAMPLE);

            const frames = after.frames - before.frames;
            const gaps = quantiles(after.marks);
            const per = (name: string) =>
                frames ? ((after.calls[name] ?? 0) - (before.calls[name] ?? 0)) / frames : 0;
            const total = [...moved.entries()].sort((a, b) => b[1].bytes - a[1].bytes);
            for (const [kind, held] of total) {
                console.log(
                    `   net ${kind}: ${(held.bytes / 1048576).toFixed(2)} MiB, ${held.count} requests, last at ${(held.last / 1000).toFixed(1)} s`,
                );
            }
            const bytes = total.reduce((a, b) => a + b[1].bytes, 0);
            const count = total.reduce((a, b) => a + b[1].count, 0);
            const done = total.reduce((a, b) => Math.max(a, b[1].last), 0);
            console.log(`   net total: ${(bytes / 1048576).toFixed(2)} MiB, ${count} requests, ${failed} failed, last at ${(done / 1000).toFixed(1)} s`);
            const share = (at: number) => {
                const want = carried * at;
                const found = timeline.find(([, held]) => held >= want);
                return found ? (found[0] / 1000).toFixed(1) : "-";
            };
            console.log(`   net reached: 50% at ${share(0.5)} s, 90% at ${share(0.9)} s, rate ${(bytes / 1048576 / Math.max(done / 1000, 0.001)).toFixed(2)} MiB/s`);
            console.log(`   net in flight: ${peak} at the peak, ${answered ? (latency / answered).toFixed(0) : "-"} ms a request over ${answered} answered`);
            console.log(
                `   fps: ${(frames / elapsed).toFixed(1)} over ${elapsed.toFixed(1)} s (${frames} frames), frame median ${gaps.median.toFixed(1)} ms, p95 ${gaps.p95.toFixed(1)} ms, worst ${gaps.worst.toFixed(0)} ms`,
            );
            const calls = frames ? Object.keys(after.calls).reduce((a, k) => a + ((after.calls[k] ?? 0) - (before.calls[k] ?? 0)), 0) / frames : 0;
            const drawn = per("drawElementsInstanced") + per("drawElements") + per("drawArrays") + per("drawArraysInstanced");
            console.log(
                `   cost: ${drawn ? ((gaps.median * 1000) / drawn).toFixed(1) : "-"} us per draw, ${drawn.toFixed(0)} draws and ${calls.toFixed(0)} watched gl calls a frame`,
            );
            console.log(
                `   per frame: ${per("drawElementsInstanced").toFixed(0)} instanced draws, ${per("drawElements").toFixed(0)} draws, ${per("getUniformLocation").toFixed(0)} uniform lookups, ${per("disableVertexAttribArray").toFixed(0)} attrib disables, ${per("vertexAttribPointer").toFixed(0)} attrib pointers, ${per("bindVertexArray").toFixed(0)} array binds, ${per("useProgram").toFixed(0)} programs, ${per("bindTexture").toFixed(0)} texture binds, ${per("bufferData").toFixed(0)} buffer uploads`,
            );
            const frame = await cdp.eval("window.__frame ?? null").catch(() => null);
            if (frame) {
                const held = JSON.parse(frame);
                console.log(`   build: ${held.commit}${held.clean ? "" : "+dirty"} at ${held.built}`);
                console.log(`   loaded: ${held.models} models, ${held.materials} materials, ${held.drawn} drawn of ${held.placed} placed`);
            }
            for (const line of timings.slice(-14)) console.log(`   ${line}`);
            if (failures.length) {
                console.log(`   failures: ${failures.length}`);
                for (const line of failures.slice(0, 6)) console.log(`     !! ${line}`);
            }
            const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
            const name = `${String(at).padStart(2, "0")}-${path.split("/").pop()}`;
            writeFileSync(join(outDir, `${name}.png`), Buffer.from(shot.data, "base64"));
            if (logFile) {
                const to = paths.length > 1 ? `${logFile}.${at}` : logFile;
                const stalls = await cdp.eval("window.__perf ? window.__perf.stalls : []").catch(() => []);
                writeFileSync(to, JSON.stringify({ path, requests: logged, samples, stalls, timings }));
                console.log(`   log: ${to}, ${logged.length} requests, ${samples.length} samples`);
            }
        }
    } finally {
        cdp.close();
        child.kill();
        rmSync(profile, { recursive: true, force: true });
    }
}

await main();
