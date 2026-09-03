# Smoke gates

Two gates exist, and neither replaces the other:

```
smoke/native.sh   # fast default: native app, offscreen, local sqpack, no network
smoke/run.sh      # the browser gate: real wasm build in real headless chromium
```

`native.sh` is what to run for an ordinary change: it builds the native `viewer` binary (dev
profile, so `debug_assertions` stays on for `egui_glow`'s `check_for_gl_error`), drives it against
`/home/asriel/.xlcore/ffxiv/game/sqpack` under Xvfb with no browser and no HTTP, and fails on a
panic, an ERROR-level log, or a step that never decodes. It is roughly **3-4x faster wall-clock**
than the browser gate over the same asset list (model + two scenes: ~36s native vs ~93s browser,
measured), because it skips the wasm build, the chromium launch, and every asset fetch.

**It is not a replacement for the browser gate.** Three GL faults have shipped that only the
browser gate would have caught: `egui_glow::CallbackFn` needing `Send + Sync` (a wasm *compile*
error, invisible to both gates; `cargo check --target wasm32-unknown-unknown` is what catches
that one), `get_parameter_framebuffer` panicking on wasm because glow cannot map a WebGL object it
did not create, and `blitFramebuffer` into a multisampled default framebuffer being
`INVALID_OPERATION` (the canvas there is 4x multisampled, and wasm is 32-bit; native has neither
property). See "What only the browser gate catches" below before trusting a green `native.sh` run
over a red `run.sh` one.

Run `smoke/run.sh` before anything that touches the shader pipeline, the wasm/JS boundary, or
offset arithmetic on file data. Run `smoke/native.sh` as the everyday fast check.

## Browser smoke gate

Runs the real wasm build in a real browser and fails on any GL error, panic or `ERROR`-level log.

```
smoke/run.sh
```

That is the whole command. It builds `viewer/dist` with trunk, serves it, drives headless
chromium over it, and exits non-zero with the browser messages that failed it.

Flags: `--model=`, `--scene=` and `--level=` each name one asset to walk and `--avfx=` takes a
comma-separated list. `--no-build` reuses the existing `viewer/dist`, `--shots` writes screenshots
to `smoke/shots/`, `--model-only` stops after the model renders and skips every click,
`--avfx-only` runs the effects and nothing else, `--explore` is `--model-only` with screenshots
(use it to recalibrate the click coordinates in `smoke.ts` after a UI change). `--orbit` turns the
camera between shots, which is what makes an ordering fault show rather than depending on the one
angle a model happens to open at, and takes each effect a whole turn in eighths once it is paused,
which is what a quad lying in a world plane has to lose its coverage across; `--views` walks the
preview path's own debug row. Every run writes `smoke/last-run.json`.

A full run opens **nine effects** after the scene and took four to five minutes across five runs
measured 2026-08-26, warm CDN cache; a cold one pulling the two apricot packages fresh on each of
the nine pages (20 and 40 MiB) can run well past that.

A red run here is not a broken harness; read the deduped list it prints, and check it against
"Known red" below before assuming you caused it.

## Why it exists

The desktop gates around the shader pipeline are large and none of them touch a browser. Three
bugs reached the user anyway, all of which compiled and passed every one of those gates:

- `egui_glow::CallbackFn` needs `Send + Sync`, which `glow::Context` is not on wasm.
- `get_parameter_framebuffer` panics on wasm, since glow cannot map a WebGL object it did not
  create. Ask `painter.intermediate_fbo()` instead.
- `blitFramebuffer` into a multisampled default framebuffer is `INVALID_OPERATION`. The canvas is
  4x multisampled and every headless harness before this one rendered single-sample.

## What it does

Chromium is fetched from the nix binary cache (no source build) and run headless, where WebGL2
comes from ANGLE over Vulkan/SwiftShader. **The canvas reports `SAMPLES=4`**, which is what makes
the multisample blit reproduce; the run aborts if it ever comes up single-sampled, because a
single-sampled run would pass without testing what this gate is for. It also reports the canvas's
own depth, which eframe asks for no attributes about and so comes out at WebGL's default of 24
bits here; a machine that answers otherwise is one where a viewer drawing into the canvas would
depth test against nothing.

It then walks the paths that broke:

1. `/assets/<model>.mdl`. Routes are URL-addressable and the setup screen auto-submits when it is
   handed a `redirect`, so opening a model needs no clicking.
2. Clicks **Game shaders** and waits for the deferred path to link programs and bind a G-buffer.
3. Sweeps the channel row, covering `SV_Target`, `SV_Target1..4` and `Lit`.
4. Clicks **Game shaders** off again and compares the preview frame against the one taken before
   any of that, which is what catches the deferred path leaving GL state behind.
5. Opens a `.lgb` in the Assets tab, clicks its **Scene** tab, and waits for instanced draws.
6. Opens the `.lvb` naming that zone in the Zones tab, which places the scene itself rather than
   showing it behind a tab click, and reaches the environment panel's own files.
7. Opens each `.avfx` in turn, dropping local storage behind the navigation since eframe writes
   egui's panel widths into it, and clicks its playback slider at two points of its own timeline,
   which both pauses it and seeks, so the two shots of an effect land on the same frames every run.
   Each effect has to draw something, and across the run the two shots of at least one of them have
   to differ, or the click never landed on the slider and the shots are of an arbitrary frame.
8. Opens `/character`, waits for the default body and its starting attire to both build and for
   the equipment menus to load, then opens the Head slot and picks an item, both confirmed off a
   log line naming which slot each click actually landed on rather than assumed from the
   coordinate alone. This redresses an already-drawn model rather than opening a fresh one.
   `drawBuffers` has to climb again within 30s of the redress. A single material that fails to
   link is not what this catches: `render()`'s own loop already skips one and keeps going, and
   that miss surfaces on its own as a fatal `ERROR:` log. What zero drawBuffers here actually means
   is that the viewer stopped being drawn at all: `show()` calls `gl.drawBuffers` unconditionally
   as its first GL call every frame it runs, so the only way this holds at exactly zero is a
   redress that fails wholesale and leaves the tab painting an error label forever after instead of
   the model. `draws` cannot stand in for this: egui's own panels repaint it constantly on their
   own, game shaders or not.

`smoke/instrument.js` is injected before the app loads and counts draws, instanced draws, blits,
`drawBuffers` calls and program links. Those counters are asserted, so a click that lands in the
wrong place fails the run instead of passing with nothing rendered.

## What it fails on

egui's own `check_for_gl_error` runs after every paint callback, so GL state errors surface with
no instrumentation. The signals are read off the console over CDP:

- any thrown exception, or `panicked at` in any message
- any message beginning `ERROR:`. **eframe maps Rust's `Error` level onto `console.warn` with an
  `ERROR:` prefix, not `console.error`**, so a gate watching `console.error` alone would miss
  every `egui_glow` GL error
- `GL_INVALID*`, `GL error`, `INVALID_FRAMEBUFFER_OPERATION` from chromium's own renderer log
- a crashed renderer process

Network-level errors are reported but not fatal, since the app probes for optional files; the
coverage counters are what catch a model that failed to load.

## Known red

**`timed out after 180000ms waiting for r2d1.lvb to be titled`, level phase.** Root-caused
2026-08-27. `smoke/run.sh` used to build the dev profile (`trunk build index.html`, no `--release`),
and the working theory going in was that `egui_glow::check_for_gl_error` panicked the whole app on
a transient GL error under load. That does not hold up: `check_for_gl_error_impl` only calls
`log::error!`, never panics, and forcing `WEBGL_lose_context` on both a dev and a `--release` build
produced identical behaviour (frozen canvas, no panic, either side, `gl.getError()` reporting
`CONTEXT_LOST_WEBGL`). The real difference is size and speed: the dev `viewer.wasm` measured 104.5
MiB against a `--release` build's 18.8 MiB (5.5x), first frame 582.9ms against 114.3ms (5x), and a
single texture decode 10-25x slower. A streaming zone is decode-bound, and the level phase's
`titled`/`decoded` waits fire only once the router runs, which needs the whole wasm module fetched
and booted first. `smoke/run.sh` now builds the `smoke` cargo profile (`Cargo.toml`): release
everywhere except `egui_glow`, which keeps `debug-assertions` on so its own `check_for_gl_error`
still runs (the unattributed `GL_INVALID_OPERATION (0x502)` below has nothing else to catch it),
and `overflow-checks` stays on project-wide so a malformed or oversized file still panics on the
32-bit wasm target instead of wrapping a `usize` silently. Reproduced live: three dev-profile runs
at load average ~30 (three concurrent, the machine also running other agents' chromiums) gave 2
PASS and 1 FAIL at exactly this message; the `smoke`-profile build measured the level phase's own
`titled`+`decoded`+`instanced` wait at 28-39s total across four separate runs at settled-to-moderate
load (~7-17), comfortably under the 180s budget it used to time out on. The budget was never too
tight for what the level phase actually does; it was too tight for a 104 MiB unoptimized wasm
competing with everything else on the box. Left at 180s.

**The app's own `fetch()` to `xiviewer.app` exhausts chromium under concurrent load; `curl` to the
same URL stays fast throughout.** `smoke/run.sh` now runs a CDP `Fetch.enable` proxy
(`proxyFetches` in `smoke/smoke.ts`) on `https://xiviewer.app/*`: every request chromium would have
made is instead answered from bun's own `fetch()`, so contention inside chromium's network stack
never gets a chance to happen. Measured across four runs after the fix: 0, 0, 2 and 12 fetches
failed against totals of 1480-1760 served, against the 103-157 failures per run a prior measurement
found unproxied. One thing this had to get right and initially didn't: a `Range` header (used for
partial mip/LOD reads) is not CORS-safelisted, so the browser sends its own preflight `OPTIONS` for
a ranged fetch. The first version of this proxy intercepted and replayed that preflight itself,
which broke the CORS handshake outright (`blocked by CORS policy: No 'Access-Control-Allow-Origin'
header is present`) even though the real server already answers it correctly unproxied.
`Fetch.continueRequest` on any `OPTIONS` method fixes it: let the preflight go straight to the real
server, and only intercept the request it is negotiating for.

This unblocks a zone that could not load headless at all before: `bg/ffxiv/wil_w1/twn/w1t1` (a
capital city) ran the full gate end to end with the proxy in place - model, shaders, scene, level,
character and all nine avfx effects. It still fails, but not on anything this fix touches: seven
pre-existing 404s, for `bg/ffxiv/wil_w1/twn/w1t1/grass/_grass{1,4}.tex` and five
`bgcommon/world/chi/shared/for_bg/sgbg_w_chi_*.sgb` paths the zone's own layer data names but the
install does not have. That is a pathlist/asset-completeness question for whoever owns w1t1's
zone-lighting work, not a harness fault.

**`timed out after 180000ms waiting for the equipment change to rebuild the model`, character
phase.** RESOLVED, and it was the harness, not the equipment-change path itself: `HEAD_SLOT`/
`HEAD_ITEM` (`smoke/smoke.ts`), calibrated when the step landed at `d5bb5a1e`, had drifted stale
against the side panel's current layout and landed on the "Equipment" heading rather than
"Head: Bare", so the picker never opened and every wait behind it ran to its own budget before
saying so. Confirmed by hand with `smoke/drive.ts --path=/character`: the old coordinate opens
nothing, the same x one row lower opens the picker correctly. The code between "Race" and
"Equipment" in `side_panel` reads byte-identical against `d5bb5a1e`, so the drift is not a code
regression in the panel itself. Untested candidate for whoever wants to close it rather than
recalibrate again next time: `adff0b4f` (`Put the character panel's arrow on its outer edge and add
its re-expand`) wrapped `character_header`'s heading in `with_layout(right_to_left) {
vertical_centered_justified { heading } }`, which sits above every row measured here and is exactly
the kind of change that can shift a row's height allocation; one build at `adff0b4f^` and one
`drive.ts` shot of where "Race" sits would confirm or rule it out. Recalibrated to
`{x:52,y:457}`/`{x:113,y:525}`.

A second, live-caught fault of the same class: the click can still miss at the *correct*
coordinate, since the canvas right after a picker opens is still repainting at a frame or two a
second under software rendering, and a press dispatched too soon after the move lands wherever
the pointer was on the previous frame rather than where it just moved to (the same lag
`smoke/drive.ts` already documents for its own clicks). One run's `HEAD_ITEM` click silently chose
nothing this way, at the same coordinate that had just worked moments before.

Fixed at both ends. `slot_ui` (`viewer/src/character/mod.rs`) now logs which slot a click opened
(`character: picking {slot}`) and which slot an item was chosen for (`character: chose {piece} for
{slot}`); `smoke.ts`'s new `clickUntil` reads that log back, retries a miss up to three times, and
fails in seconds with an attributable "recalibrate against smoke/drive.ts" message instead of
waiting out the redress step's own 180s budget with no clue why.

Verified against a real fault the way the step's own author did: poisoned `mdl::compose`/
`Rendered::redress` so a redress still rebuilds the mesh (the `assets/mdl: ... meshes,` log fires,
same as a healthy run) and then fails, matching "the model rebuilt but the composite stopped
running" exactly; the step reported the intended `the G-buffer never bound another draw target in
the 30s...` failure, and the injected fault was reverted after. A stale coordinate on its own now
fails in about 15s with the new attributable message instead of the old 180s timeout.

A full `smoke/run.sh` (model, shaders, channels, scene, level, character, all nine avfx effects)
passed clean end to end once with the recalibrated coordinates in place, before the later pass that
added `clickUntil`'s per-attempt `reset` and the longer click settle. Every run since, on a machine
under enough concurrent load to matter (see below), has reproduced the already-documented level-
phase `glDrawElementsInstanced` flake before ever reaching the character phase. That flake is not
this step's and not new; `--character-only` on the exact committed tree has passed repeatedly. A
`--no-build` full run on a quiet machine is still owed before calling the whole gate proven end to
end again.

**This step is genuinely load-sensitive, not just click-brittle.** Under a machine already running
several other agents' headless chromiums (measured: load average above 30, 50+ live chromium
processes), even `smoke/drive.ts`'s own slow, deliberate click (a 1500ms move-to-press delay,
already the fix for the same class of lag) missed the same coordinate that had just worked moments
earlier with the machine quieter. The retry in `clickUntil` absorbs ordinary lag; it is not a fix
for the machine being this loaded, and three attempts at ~1.7s apiece is not going to out-wait a
render loop that is losing real wall-clock time to a dozen unrelated tabs. A red character phase
during a run like that is not evidence of a regression; check `uptime` and the chromium count
before trusting it.

**`Feedback loop formed between Framebuffer and active Texture`, in the scene and level phases.**
RESOLVED. `blended()`'s resolve leg (`viewer/src/assets/viewers/layer/scene/gpu.rs`) drew into
`self.lit`, whose depth attachment is the live G-buffer depth, while at least one blended surface
in `bg/ex1/01_roc_r2/dun/r2d1` also samples that same texture as `DEPTH`/`DEPTH_PLANE` through
`engine()`. `self.lit` already had a sibling for exactly this, `self.bare`, a framebuffer over the
same color texture but a copy of the depth (`cutoff`) rather than the depth itself, already used by
`fog()` and `shade()`; `blended()` was never wired to it. Found by wrapping WebGL2 to name every
texture and framebuffer bound at the moment `gl.getError()` came back non-zero after a draw, which
named the offending framebuffer's `DEPTH_ATTACHMENT` as the exact texture bound at unit 0. Measured
at 376x on this tree with `smoke/run.sh`'s own counting; a full run (scene, level, all nine avfx
effects) is clean after the fix.

The model viewer's own `resolve()`/`sheer()` (`viewer/src/assets/viewers/mdl/gpu.rs`) share the
identical `frame()`-into-`engine()` pattern and got the same `cut()`/`bare()` fix, but neither
`chara/human/c0101/obj/hair/h0001/model/c0101h0001_hir.mdl` nor
`bg/ffxiv/sea_s1/fld/s1f2/bgparts/s1f2_w1_sea01.mdl` turned out to sample `DEPTH` there, so that
half of the fix is unverified by reproduction - fixed by inspection and structural identity with
the leg that did reproduce, not by a measured before/after.

The avfx effects were never part of the 376x: on this tree they were clean before the fix too, and
`viewer/src/assets/viewers/avfx/gpu.rs` draws through its own small buffer set rather than
`mdl::deferred::Buffers`, so it cannot reach this aliasing at all. The earlier note that one avfx
effect had triggered it was never reproduced here and stays unexplained; if it recurs it is a
separate cause.

**`GL_INVALID_OPERATION: glDrawElementsInstanced: Mismatch between texture format and sampler type
(signed/unsigned/float/shadow)`, level phase.** Seen once in five full post-fix runs (3x in that
run), never in the pre-fix baseline or in six more level-only repeats afterward. Not yet diagnosed
and not attributed to the fix above: it is a different failing call
(`glDrawElementsInstanced`, not `glDrawElements`) and a different validation reason, but WebGL only
reports one reason per failing draw and a draw that fails validation never executes, so a
feedback-loop draw that used to be rejected outright could be reaching a second, independent fault
in the same draw now that it runs. `engine()` hands `SHADOW_DEPTH` a compare-mode texture and
`DEPTH`/`DEPTH_PLANE` a plain one, which is the right shape for a signed/unsigned/float/shadow
mismatch if something binds the wrong one. Whoever hits this next: reproduce it under
`smoke/run.sh`'s own counting rather than assuming it is noise.

**Unattributed `GL_INVALID_OPERATION (0x502)` at `egui_glow/painter.rs:447`, `/zones/` runs of
`r2t1` and `r2d1`.** Not diagnosed. The attribution is generic: it is egui_glow's own
`check_for_gl_error!` polling `getError()`, not the call that raised it, and no chromium-native
named error (the kind that named `glDrawElementsInstanced` above) accompanied it. `generateMipmap`
is ruled out: bracketed alone with a `getError()` drain before and after, on a run where the 0x502
did fire, with zero hits. `drawArraysInstanced`/`drawElementsInstanced` are untested, not ruled
out - every run with them bracketed also failed to reproduce the 0x502 at all, so the probe never
had a chance to fire. The reproduction itself is flaky: roughly 1 in 5 to 1 in 6 of `/zones/.../
r2d1` runs, with instanced-draw counts swinging 8,986 to 81,603 across identical wasm on both
`/zones/` and a plain `/assets/` `.lgb` open, the same shape of variance the scene/level phase
already shows for how much of a streaming zone lands in time. So the acceptance bar of zero errors
on one run each of `r2t1`/`r2d1` cannot be read off a single pass or a single fail. A single
`/assets/` `bg.lgb` run at the highest instanced-draw count seen anywhere did not reproduce it
either, which argues against "Zones-tab specifically" but is one run against a roughly-1-in-5 rate
and proves nothing alone. One run had no instrumentation at all - `instrument.js` failed to parse
that time (a stray brace from mid-edit), so nothing was wrapped and `__smoke` never existed - and
it still did not reproduce, which is the one clean data point against the wrap itself being what
suppresses it. Whoever picks this up: many repeated runs looks like a more promising lever than a
wider wrap, since the reproduction looks like genuine load-timing variance rather than an observer
effect, but that rests on a single successful repro and a single zero-instrumentation miss, not on
a controlled series.

`the preview frame changed after game shaders were turned on and off again` used to fire on a
`--orbit` run and needed **both** halves of the mechanism to show. "Reset view" sits immediately
after the last channel label and is a plain button, not a selectable label, so a sweep that walked
past the channels clicked it and reset the camera. That only *fails* the comparison when the camera
was somewhere else to begin with, which is what `--orbit` arranges; without it the camera is already
at the reset pose and the stray click changes nothing. **Where the button sits moves with the number
of targets the model's program writes**, so a fixed `SWEEP_TO` fixes it for one model and not the
next: 750 held for `m0914` and `m0370` at six targets and still overran on `m0911`, which showed
seven distinct selections. The sweep now stops once it has seen `CHANNELS` distinct selections, so it
never reaches the button whatever the layout, and `SWEEP_TO` is only a backstop.

Worth keeping because two separate investigations blamed orbit itself, then blamed a coordinate.
Neither was the whole answer, and a run that passes without `--orbit` says nothing about one with it.

The effects seek used to fail in a **full** run and only there, on `every effect looked identical at
both points of its timeline` for all nine at once. `SEEK` and `PREVIEW` are absolute window
coordinates, and **the details panel is resizable and egui remembers its width across a
navigation**: an earlier phase left it wide (a character model's material list is the widest thing
the panel holds), which squeezed the viewer pane and moved the playback bar out from under `SEEK`.
Measured on one build, the panel's left edge sat at about x=1240 in an `--avfx-only` run and about
x=828 after the model, scene and level phases. **eframe writes that memory out as the page unloads**,
not on a timer, so local storage is empty for the whole of a phase and the width only lands as the
next navigation commits; clearing the store before the effects start therefore did nothing. Each
effect now clears it just behind its own navigation, which is before the wasm has loaded and read
it.

Both of the problems reported at `f8b3ecc` are fixed. `glDrawArrays: Mismatch between texture
format and sampler type` was the stand-in textures being made lazily from inside the binding loop:
making a texture binds it to whichever unit happens to be active, so one made partway through took
over the unit the sampler before it had just been given, and that sampler then read a texture of
the wrong format. It showed on exactly the frame each stand-in was first made, which is why there
was one message per viewer. `Buffers::stand_ins` now makes them all before anything is bound. The
15 `this model draws nothing at any detail level` messages were a model carrying no standard mesh
at any level being treated as a failure to read one; it is not, and `drawn` already records it.

The two blit faults are gone too. Measured at `b965b62`, before
`0e66465 Show the frame with a pass instead of a blit` and
`46463b9 Keep egui's clip rect off the frame's own buffers`:

| | `b965b62` | `f8b3ecc` | now |
|---|---|---|---|
| `glBlitFramebuffer: Invalid operation on multisampled framebuffer` | 251 | 0 | 0 |
| `glBlitFramebuffer: Blit feedback loop` | 49 | 0 | 0 |
| `ERROR: [egui_glow] GL error` | 306 | 2 | 0 |
| total messages | 623 | 19 | 0 |

**`the preview frame changed after game shaders were turned on and off again`, passing `--model=`
a `chara/...` path.** RESOLVED. Not a GL fault: every character model plays an idle animation that
never holds still, and `settled()` used to give up after 20 tries regardless, so the two preview
shots compared landed on arbitrary, uncorrelated points of that animation. `settled()` now reports
whether it actually converged. Where it did (the default static background model), the comparison
is still the exact hash it always was. Where it did not, the comparison falls back to the share of
pixels that moved more than a small per-channel amount, since idle motion changes a bounded part of
the frame and a real state leak does not.

Two attempts at forcing a real leak turned out to be no-ops and said so honestly rather than
passing by accident: an unrestored `BLEND` enable and a viewport left at the G-buffer's own size
both measured *below* idle noise, and `Model::draw`'s plain path turned out to re-establish cull,
texture units and its own depth/blend state every frame regardless, with the viewport a shaded
frame leaves not surviving into the next callback either. A third attempt did leak: gating
`u_alpha_threshold` at 2.0 on `self.game.buffers.size() != (0, 0)` (true once shading has ever run,
since the G-buffer is never torn back down) discards the model outright once shaded has toggled on
and off, which is deterministic Rust control flow rather than GL state, so it cannot be silently
re-established. Confirmed with `--shots` that the model actually vanishes in the second screenshot,
not just a metric moving.

Measured against `chara/human/c0101/obj/body/b0001/model/c0101b0001_top.mdl`: idle-only noise
0.11-2.11% across five runs; the same model discarded outright measured 6.68% and 7.76% on two
runs. `CHANGED_TOLERANCE` is set to 4%, roughly 2x the worst idle noise seen and roughly half the
smallest deliberate-regression signal measured; a run built with the `alpha_threshold` gate above
fails at 7.8%, and the same run reverted passes at 0.11%. A second, independent numpy diff over the
same two PNGs the harness compared landed at 6.6785%, three decimal places from the TypeScript
decoder's own 6.68%, which is what says the hand-rolled PNG decode and threshold arithmetic are
correct rather than coincidentally close.

**`the preview frame changed after game shaders were turned on and off again`, on the default
static `bg/ex1/01_roc_r2/dun/r2d1/bgparts/r2d1_u1_yam04.mdl`.** Not diagnosed, not caused by
anything in this branch: reproduces identically on an unmodified `origin/main` checkout
(`c1731987`) built and run on its own, with none of this branch's commits present. This model does
not animate, so `settled()` reports `converged: true` and the comparison is the exact hash it
always was; two full-gate runs on this branch and one on stock `main` all failed it the same way.
The diff is small and structural, not a blank or a solid-color tell: about 0.35% of pixels move by
more than 24 per channel, bounded to the model's own silhouette, and amplifying the difference image
4x shows the model's surface detail rather than a flat region, which reads as a rendering-order or
multisample-resolve difference rather than a leaked binding. Whoever picks this up: bisect the
merges into `main` since this branch forked rather than this branch's own commits, since stock
`main` alone already reproduces it.

## Probing one model

`probe.sh` walks a list of models under the game shaders and shoots each one, about two minutes a
model against the gate's twenty. It exists for reading a render back, not for passing or failing.

| flag | |
|---|---|
| `--size=WxH` | viewport, default matches the gate |
| `--zoom=N` | frames the model; large values fill the pane |
| `--settle=ms` | wait before the shot, to let materials and textures land |
| `--channel=x` | which target to show |
| `--toggle=x` | one more label of the toolbar row to click before the shot |
| `--plain` | the preview path instead of the game shaders |
| `--out=` | where the shots go |
| `--mark=` | echo console lines carrying a prefix |

`--mark` is how a temporary `console.log` in the renderer gets read back, which is what turned the
sampler bindings into a table during the task #55 investigation.

## Standing a scene where a TitleEdit preset was captured

`look.ts` opens a path against a running server and shoots whatever is on screen after a wait; no
gate, no decode check, no pass/fail. `--preset=<file>` pastes a TitleEdit preset into the level
viewer's own paste box, which is the only way from outside to put the camera, weather and hour
where a capture was taken from.

```
CHROMIUM=$(...) bun smoke/look.ts --origin=http://127.0.0.1:9080 --preset=capture.json /zones/<path.lvb>
```

**A `.lvb` wants the `/zones/` path, not the asset one.** Opened under `/assets/` it offers Tree
and Sounds and no scene: `scene_enabled` starts false for a level and only the Zones tab turns it
on. A run that opens the asset route clicks the Sounds tab, never publishes `window.__frame`, and
fails with no console error at all. `look.ts` takes a leading `/` path as given.

`preset.rs`'s JSON shape has two traps neither the plugin nor anything else documents:

- `TerritoryPath` is the level path with the `bg/` prefix and `.lvb` suffix both stripped, e.g.
  `"ex1/01_roc_r2/twn/r2t1/level/r2t1"` for `bg/ex1/01_roc_r2/twn/r2t1/level/r2t1.lvb`.
- `Point` fields (`Position`, `CameraPosition`, `FixOnPos`, ...) are PascalCase: `X`, `Y`, `Z`.

## Measuring a frame against the game's own

`frame.ts` stands this viewer where a captured game frame was taken from and reports the difference
as numbers. The camera, the lens and the frame's shape are read out of the capture's own constant
buffers, so the two views are the same view rather than two hand-flown ones, and the report states
the residual step, turn and lens difference rather than leaving it to the eye.

```
CHROMIUM=$(...) bun smoke/frame.ts --capture=~/rdcaps/tuli.zip.xml \
    --level=bg/ex5/02_ykt_y6/twn/y6t1/level/y6t1.lvb --time=14:10 --weather=1 \
    --crop=150,170,1450,1040 --out=smoke/y6t1
```

| flag | |
|---|---|
| `--capture=` | a capture `renderdoccmd convert -c zip.xml` has been run over |
| `--level=` | the `.lvb` the capture stood in, which no capture states |
| `--time=HH:MM` `--weather=N` | the clock and the weather, which no capture states either |
| `--camera=N` | which of the cameras the frame holds drew it, where it holds more than one |
| `--crop=` `--mask=` | the region to measure, in the game frame's own pixels |
| `--size=WxH` | the browser window, default `2400x1200` |
| `--wait=ms` | how long to let the zone stream before the shot |
| `--no-build` | serve `viewer/dist` as it stands |
| `--build=<sha>` | measure a wasm older than the sources on purpose |

It writes `frame.png` (what the game presented), `window.png`, `view-aligned.png` (this viewer
resampled onto the game's pixel grid), `difference.png`, `overlay.png` (the game in red and this
viewer in green, so geometry that only one of them drew is obvious), `report.txt`, and the preset
and camera the capture states.

**Saturation is the number to read across runs, not luminance.** The frame's gain is an
auto-exposure that follows how much of the zone has arrived, and `(max - min)/max` is invariant
under a gain. The report carries both, per whole region, per band and per grid cell.

**A run against a build older than the last change to what the wasm is made of fails.** The app
publishes its own commit, its viewport rect and its camera on `window.__frame`; nothing here guesses
where the scene sits in the window or which build drew it.

The two halves are usable alone: `rdframe` takes the frame and the camera out of a capture, and
`framediff` measures one image, or two, with no browser in the loop.

## What it does not cover


Only the three 3D viewers, and one asset each for the model, the scene and the level (the `.lgb`
and the `.lvb` share a viewer). The gate itself does not check that anything is drawn *correctly*;
`frame.ts` above is what does, and only for a zone a capture exists of. Channel coverage is a
positional sweep
over the row rather than a lookup of each label, so it counts distinct selections rather than
naming them. The click coordinates are calibrated against a 1600x1000 viewport and need
`--explore` and a fresh look if the layout moves.

It cannot catch the `Send + Sync` bug at all. That one is a wasm *compile* error, so its gate is
`cargo check -p viewer --target wasm32-unknown-unknown --lib`; nothing that runs a built app can
see it. Of the three bugs this exists for, the browser is the only place the other two show up.

`--model-only` returns before the shaders phase, so it never turns the game shaders on and never
links the deferred lighting programs. A pass in that mode says nothing either way about anything
`Program::screen` selects, translates or links, which once cost a correct change a revert.

It needs the network: the app reads from `https://xiviewer.app`, and the run uses the live API
so that the real decode path is what gets exercised.

## Native smoke gate

```
smoke/native.sh [--no-build]
```

Builds `viewer` (dev profile) and runs it with `--smoke <sqpack> <out-dir> <step>...`, a mode
`viewer/src/main.rs` adds alongside the ordinary native and wasm entry points. `viewer/src/smoke.rs`
drives the real `App`: it seeds `BACKEND_CONFIG` with a local `InstallLocation::Sqpack` before the
first frame (bypassing the setup screen, the same way a saved config does), seeds the router's
history with `/?redirect=/assets/<path>` (the same query param a real deep link bounces through),
waits for the `assets/preview: ` log line that says a viewer actually decoded its bytes, clicks
"Game shaders" or the "Scene" tab at a coordinate calibrated the same way `smoke.ts`'s are, waits
for the frame to settle, and takes a screenshot with `egui::ViewportCommand::Screenshot`. A
`log::Log` wrapper (`smoke::CountingLogger`) counts ERROR-level records as they are emitted, so a
run fails the instant one fires rather than idling out to its step timeout on top of it. Every run
writes `report.json` and one PNG per step to `smoke/native-shots/` (gitignored).

Offscreen rendering needs a real display, not a headless flag: `eframe::NativeOptions` has no
"don't open a window" mode, so the harness gives it a normal `ViewportBuilder::with_visible(false)`
window and points it at an [Xvfb](https://www.x.org/releases/X11R7.6/doc/man/man1/Xvfb.1.xhtml)
X server via `DISPLAY`, which never draws a window on any real screen. **This was measured, not
assumed, because the two ways to get a display here differ in a way that would have shipped a gate
that always passes:** an invisible window against the user's live Wayland session renders a real
GPU frame for a couple of frames and then the compositor stops delivering frame-done callbacks to
the invisible surface, so `request_repaint()` never fires again and the process hangs forever
rather than progressing or failing. Xvfb keeps delivering frames to an invisible window
indefinitely (through Mesa's software `llvmpipe`, confirmed via `GL_RENDERER`) and a real panic
inside `App::ui` still reaches the terminal as `panicked at ...` with a nonzero exit: winit's
Linux backend has no `catch_unwind` around the event loop (unlike its macOS and Windows backends),
so nothing swallows it.

### What it catches that the browser gate also catches
- A panic anywhere in the app, including inside the model, scene or level viewer.
- Any `log::error!`, including a real `egui_glow` GL error surfaced through
  `check_for_gl_error!` (native keeps `debug_assertions` on in a dev build; a release build would
  not run this check at all, so `native.sh` never builds `--release`).
- A step that never decodes (a wrong path, a broken read, a decode that panics quietly into a
  promise that never resolves).
- The deferred/"Game shaders" G-buffer path actually linking, binding and drawing (confirmed by
  eye: a model shot with the toggle on shows the shaded, textured frame, not the debug-normals
  view it opens on).
- A scene's or level's instanced draws actually landing (confirmed by eye: a `.lgb`/`.lvb` shot
  after the settle shows real geometry, not the black frame taken before instancing starts).

### What only the browser gate catches
- **`egui_glow::CallbackFn` needing `Send + Sync`.** This is a wasm *compile* error; native never
  even reaches it. Neither smoke gate is the check for this one; `cargo check -p viewer --target
  wasm32-unknown-unknown --lib` is, and it already runs as one of the four required gates.
- **`get_parameter_framebuffer` panicking on wasm.** glow cannot map a WebGL object it did not
  create; native's GL context has no such restriction, so a native run cannot exercise this path
  at all, faulty or not.
- **`blitFramebuffer` into a multisampled default framebuffer.** The wasm canvas is 4x
  multisampled by the browser; `native.sh`'s window asks for `multisampling: 0` like the ordinary
  native entry point does, so this class of bug has nothing to reproduce against here. A change
  that only breaks under multisampling will pass `native.sh` and fail `run.sh`.
- **32-bit overflow in offset/size arithmetic on file data.** wasm32 is a 32-bit target; native is
  64-bit. A parser bug that overflows `usize` in the browser cannot overflow the same way natively.
- Anything JS-boundary-shaped: `wasm-bindgen` glue, worker messaging, browser-only APIs.
- Whatever a model, scene or effect looks like on screen beyond "something drew and no error
  fired." `native.sh` takes screenshots but does not diff them against anything; a rendering
  regression that still avoids a GL error and still draws *something* passes both gates. `frame.ts`
  (above) is the tool that checks correctness, and only for a zone a capture exists of.
- Coverage is narrower by default: `native.sh` runs one model and two scene/level assets, not the
  nine `.avfx` effects the browser gate's full run walks. Nothing stops `--smoke` from taking more
  steps; `smoke/native.sh` just does not ask for more yet.

### Comparability with the browser gate
Both gates can be pointed at the same assets: `native.sh`'s `steps` array and `smoke.ts`'s
`MODEL`/`SCENE`/`LEVEL` defaults were kept in sync (`bg/ex1/01_roc_r2/dun/r2d1/...`) for exactly
this reason, so a red run in one is checkable against the other on identical input. Measured on this
machine (`fourier`), both under software rendering (`native.sh`'s Xvfb runs Mesa's `llvmpipe`,
`run.sh` runs SwiftShader): opening the model, toggling game shaders, and opening both the `.lgb`
and the `.lvb` took **~36s** end-to-end natively versus **~93s** for the browser gate's model +
shaders + channel sweep + scene + level phases (before any `.avfx` effect). `native.sh` run against
the machine's real GPU instead of Xvfb (an invisible window on the live desktop session, not
something the script does by default) took the same shape of time; the 3-4x gap is the wasm
build, the browser launch and the network fetches `native.sh` skips, not the renderer. The wasm
build itself (trunk, dev profile) took ~41s on top of the browser gate's run time once; `native.sh`'s
own dev build took ~6s incrementally or ~42s clean, so on a clean checkout the two are closer than
the run-time numbers alone suggest, and the gap grows every time either gate runs again without a
rebuild.
