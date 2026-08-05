# Browser smoke gate

Runs the real wasm build in a real browser and fails on any GL error, panic or `ERROR`-level log.

```
smoke/run.sh
```

That is the whole command. It builds `viewer/dist` with trunk, serves it, drives headless
chromium over it, and exits non-zero with the browser messages that failed it.

Flags: `--no-build` reuses the existing `viewer/dist`, `--shots` writes screenshots to
`smoke/shots/`, `--explore` boots the model and stops before any click (use it to recalibrate the
click coordinates in `smoke.ts` after a UI change). Every run writes `smoke/last-run.json`.

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
single-sampled run would pass without testing what this gate is for.

It then walks the paths that broke:

1. `/assets/<model>.mdl`. Routes are URL-addressable and the setup screen auto-submits when it is
   handed a `redirect`, so opening a model needs no clicking.
2. Clicks **Game shaders** and waits for the deferred path to link programs and bind a G-buffer.
3. Sweeps the channel row, covering `SV_Target`, `SV_Target1..4` and `Lit`.
4. Opens a `.lgb`, clicks its **Scene** tab, and waits for instanced draws.

`smoke/instrument.js` is injected before the app loads and counts draws, instanced draws, blits,
`drawBuffers` calls and program links. Those counters are asserted, so a click that lands in the
wrong place fails the run instead of passing with nothing rendered.

## What it fails on

egui's own `check_for_gl_error` runs after every paint callback, so GL state errors surface with
no instrumentation. The signals are read off the console over CDP:

- any thrown exception, or `panicked at` in any message
- any message beginning `ERROR:` — **eframe maps Rust's `Error` level onto `console.warn` with an
  `ERROR:` prefix, not `console.error`**, so a gate watching `console.error` alone would miss
  every `egui_glow` GL error
- `GL_INVALID*`, `GL error`, `INVALID_FRAMEBUFFER_OPERATION` from chromium's own renderer log
- a crashed renderer process

Network-level errors are reported but not fatal, since the app probes for optional files; the
coverage counters are what catch a model that failed to load.

## What it does not cover

Only the two 3D viewers and only one asset each. It does not check that anything is drawn
*correctly* — no reference images, no pixel comparison. Channel coverage is a positional sweep
over the row rather than a lookup of each label, so it counts distinct selections rather than
naming them. The click coordinates are calibrated against a 1600x1000 viewport and need
`--explore` and a fresh look if the layout moves.

It needs the network: the app reads from `https://exd.camora.dev`, and the run uses the live API
so that the real decode path is what gets exercised.
