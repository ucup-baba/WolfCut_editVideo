# GPU preview: design

**Status:** proposed
**Problem owner:** the monitor is not watchable while more than one thing is on screen.

## The problem, measured

Every frame the monitor shows from the engine travels:

```
decode → composite (CPU) → RGBA bytes → IPC → ArrayBuffer → putImageData → canvas
```

`EngineStillLayer` in `desktop/src/components/Preview.tsx` is the end of that
road: a 2D canvas and `putImageData`. `useEngineTruth` pulls one frame at a
time, never queuing, and only pulls at all when the element preview cannot do
the job - two or more visual layers, a broken element, or a timeline effect
covering the playhead.

The result is correct and slow. Timeline effects made it worse: each one adds
an FFmpeg round trip per frame, measured at 74 ms on a 960x540 frame through
a blur. The process is three quarters of that and cannot be avoided - see the
module docs on `wolfcut-media/src/filter.rs` for why keeping one running
deadlocks.

No amount of tuning inside that pipeline reaches realtime. The frames have to
stop making the trip.

## Two ways out, and why one of them is not it

**A. A native GPU surface.** `WgpuCompositor` already exists and already
draws the same pixels as the CPU path, with parity tests. Render into a
native GL surface and put it where the monitor is.

Rejected. Tauri gives no supported way to composite a native surface inside
a webview. It means an overlay window tracked against the monitor's rect, per
platform, fighting z-order, HiDPI and every window manager - and failing in
ways that depend on the user's machine. The truthfulness it buys is not worth
a preview that works here and not there.

**B. Composite in the webview, on WebGL.** Draw each visible clip's `<video>`
into a texture and blend the stack with shaders.

Chosen. Three things already point this way:

- The preview *already* runs a `requestAnimationFrame` loop that draws a
  `<video>` into a canvas - that is what the pixelate, mirror and fisheye
  effects do today, hiding the element while they run. This extends a pattern
  that is in the codebase, rather than introducing a foreign one.
- `<video>` elements work again now that media is served over loopback, and
  they are hardware-decoded by the platform for free.
- The blend maths is already a GPU shader. `blend.wgsl` is where
  `wolfcut-core`'s `BlendMode` and `wolfcut-render`'s `blend_rgb` were ported
  from; a GLSL transliteration is a third copy of arithmetic that is already
  pinned by tests on both existing copies.

## What this does not fix

**Effects stay approximate while moving.** The catalogue is FFmpeg filter
strings; those cannot run in WebGL. This is true of option A as well - the
Rust compositor cannot run them either - so it is not a difference between
the two, and it is not a regression: it is exactly where the app is today.
The paused frame and the export remain the truth for effects.

Saying it plainly, because it is the thing most likely to disappoint: this
project makes *playback* smooth. It does not make effects real-time-accurate.

## The doctrine this has to fit

The monitor already has two answers for one instant, on purpose: an
approximation while things move, the engine's own frame when they stop
(desktop decision 0007, `useEngineTruth`). This project does not add a third.
It replaces the *approximation* - CSS filters over a single `<video>`, which
cannot stack layers at all - with one that composites properly. The engine
stays the truth for the paused frame and for export, unchanged.

That framing is what keeps this honest. A WebGL compositor that claimed to be
the truth would need pixel parity with the Rust one forever. As the
approximation, it needs to be close, and where it differs the pause settles
it - which is the arrangement every part of this monitor already runs on.

## Scope

This spec covers one subsystem: **compositing stacked video layers in the
webview, with transform, opacity and blend modes.**

Out of scope, each its own project:

- Effects as shaders (an upgrade to the effect approximation).
- Titles and overlays, which already draw as DOM over the picture.
- Audio and transport, which this does not touch.
- Retiring the engine-streamed path entirely. It stays as the fallback for
  anything the WebGL path declines to draw.

## Requirements

1. `previewLayersAt` derives every visible clip at an instant, bottom-first,
   with the transform, opacity and blend mode each needs - mirroring the
   engine's `plan_frame`, which is the one definition of what is on screen.
2. Blend maths in TypeScript matches `wolfcut-render`'s `blend_rgb` at pinned
   values, including the Overlay/HardLight swap that both existing copies
   already pin.
3. The compositor draws N layers with per-layer transform and opacity, and
   blends each against what is beneath it.
4. It is used while playing; the paused frame still comes from the engine.
5. Where it cannot draw - no WebGL context, a layer with no element - it
   declines and the existing engine path takes over. No blank monitor.

## Global constraints

- No new npm dependencies. WebGL2 is in the platform; a matrix library is not
  needed for a 2D quad.
- No new Rust dependencies.
- Every user-visible string goes through `t()` with keys in both
  `en.json` and `zh-CN.json`; `i18n.test.ts` enforces parity.
- `npm run typecheck`, `npm run lint` and `npm test` pass at every commit.
- Frontend tests run on Node 22. Node 20 cannot load the jsdom suite.
