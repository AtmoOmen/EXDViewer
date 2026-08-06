# Browser smoke gate

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

## What it does not cover

Only the three 3D viewers, and one asset each for the model, the scene and the level (the `.lgb`
and the `.lvb` share a viewer). It does not check that anything is drawn *correctly*: no reference
images, no pixel comparison. Channel coverage is a positional sweep
over the row rather than a lookup of each label, so it counts distinct selections rather than
naming them. The click coordinates are calibrated against a 1600x1000 viewport and need
`--explore` and a fresh look if the layout moves.

It cannot catch the `Send + Sync` bug at all. That one is a wasm *compile* error, so its gate is
`cargo check -p viewer --target wasm32-unknown-unknown --lib`; nothing that runs a built app can
see it. Of the three bugs this exists for, the browser is the only place the other two show up.

`--model-only` returns before the shaders phase, so it never turns the game shaders on and never
links the deferred lighting programs. A pass in that mode says nothing either way about anything
`Program::screen` selects, translates or links, which once cost a correct change a revert.

It needs the network: the app reads from `https://exd.camora.dev`, and the run uses the live API
so that the real decode path is what gets exercised.
