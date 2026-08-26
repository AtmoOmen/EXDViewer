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

A full run opens **nine effects** after the scene and takes around twenty minutes. Each one is a
fresh page, so each one pulls the two apricot packages again, and they are 20 and 40 MiB.

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
5. Opens a `.lgb`, clicks its **Scene** tab, and waits for instanced draws.
6. Does the same for the `.lvb` naming that zone, which reaches the environment panel's own files.
7. Opens each `.avfx` in turn, dropping local storage behind the navigation since eframe writes
   egui's panel widths into it, and clicks its playback slider at two points of its own timeline,
   which both pauses it and seeks, so the two shots of an effect land on the same frames every run.
   Each effect has to draw something, and across the run the two shots of at least one of them have
   to differ, or the click never landed on the slider and the shots are of an arbitrary frame.

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

Nothing at present.

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
