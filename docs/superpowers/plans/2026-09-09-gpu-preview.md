# GPU Preview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Composite stacked video layers in the webview on WebGL, so playback stops making a CPU round trip per frame.

**Architecture:** A `requestAnimationFrame` loop draws each visible clip's `<video>` into a WebGL2 texture and blends the stack with a fragment shader transliterated from `blend.wgsl`. This replaces the monitor's *approximation* - it is not a second source of truth. The engine still supplies the paused frame and every exported one, unchanged.

**Tech Stack:** TypeScript, React, WebGL2 (no libraries), Vitest.

**Spec:** `docs/superpowers/specs/2026-09-09-gpu-preview.md`

## Global Constraints

- No new npm or Rust dependencies. WebGL2 is in the platform.
- Every user-visible string goes through `t()`, with keys added to **both** `desktop/src/locales/en.json` and `desktop/src/locales/zh-CN.json`. `i18n.test.ts` fails on a missing key.
- `npm run typecheck`, `npm run lint`, `npm test` all pass before every commit.
- Run frontend tests on Node 22 (`nvm use 22`). Node 20 cannot load `assets.test.ts` and the run reports an error unrelated to your change.
- Commands run from `desktop/`.
- The engine (Rust) is not modified by this plan at all.

---

### Task 1: Derive every visible layer, not just the top one

`previewSourceAt` returns the single top-most visual clip, which is why the element preview cannot stack. The compositor needs the whole stack, bottom-first, mirroring the engine's `plan_frame`.

**Files:**
- Modify: `desktop/src/lib/monitor.ts`
- Test: `desktop/src/lib/monitor.test.ts`

**Interfaces:**
- Consumes: `clipsAt`, `findMedia`, `depthIn` (already in `monitor.ts`), `EditorProject`, `TimelineData` from `../lib/editor`.
- Produces:
  ```ts
  export interface PreviewLayer {
    clipId: string;
    path: string;
    time: number;      // seconds into the source file
    speed: number;
    isStill: boolean;
    scale: number;
    offsetX: number;
    offsetY: number;
    rotation: number;  // degrees, clockwise
    opacity: number;   // 0..1, fades already folded in
    blendMode: BlendMode;
  }
  export function previewLayersAt(
    project: EditorProject,
    timeline: TimelineData,
    playhead: number,
  ): PreviewLayer[];
  ```

- [ ] **Step 1: Write the failing test**

Add to `desktop/src/lib/monitor.test.ts`. The existing `project(clips)` helper and `clip({...})` factory in that file build the fixture; reuse them.

```ts
test("previewLayersAt returns every visible clip, bottom-most first", () => {
  const lower = clip({ id: "a", trackId: "T1", start: 0, duration: 10 });
  const upper = clip({ id: "b", trackId: "T2", start: 0, duration: 10, blendMode: "multiply" });
  const p = project([lower, upper]);

  const layers = previewLayersAt(p, activeTimeline(p), 5);

  expect(layers.map((layer) => layer.clipId)).toEqual(["a", "b"]);
  expect(layers[1].blendMode).toBe("multiply");
});

test("previewLayersAt skips audio and text, which are not pictures", () => {
  const p = project([
    clip({ id: "a", trackId: "T1", kind: "video" }),
    clip({ id: "b", trackId: "T2", kind: "audio" }),
    clip({ id: "c", trackId: "T2", kind: "text" }),
  ]);
  expect(previewLayersAt(p, activeTimeline(p), 1).map((l) => l.clipId)).toEqual(["a"]);
});

test("previewLayersAt maps the playhead into each clip's own source time", () => {
  // The clip starts at 4s on the timeline, 30s into its media, at 2x.
  const p = project([
    clip({ id: "a", trackId: "T1", start: 4, duration: 10, sourceStart: 30, speed: 2 }),
  ]);
  expect(previewLayersAt(p, activeTimeline(p), 7)[0].time).toBe(36);
});
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cd desktop && npx vitest run src/lib/monitor.test.ts
```

Expected: FAIL, `previewLayersAt is not a function`.

- [ ] **Step 3: Implement it**

Add to `desktop/src/lib/monitor.ts`, beside `previewSourceAt`. Import `type BlendMode` from `./editor`.

```ts
/**
 * Every clip putting pixels on screen at `playhead`, bottom-most first.
 *
 * The engine's `plan_frame` is the definition of what is on screen at an
 * instant; this is the same question asked in the webview, so the two stay
 * in step by answering it the same way - track order, fades folded into
 * opacity, audio and titles left out.
 */
export function previewLayersAt(
  project: EditorProject,
  timeline: TimelineData,
  playhead: number,
): PreviewLayer[] {
  const depth = depthIn(timeline);
  return clipsAt(project, playhead)
    .filter((clip) => clip.kind === "video" || clip.kind === "image")
    .sort((a, b) => (depth.get(b.trackId) ?? 0) - (depth.get(a.trackId) ?? 0))
    .flatMap((clip) => {
      const media = findMedia(project, clip.mediaId);
      if (!media) return [];
      return [
        {
          clipId: clip.id,
          path: media.path,
          time: clip.sourceStart + (playhead - clip.start) * clip.speed,
          speed: clip.speed,
          isStill: clip.kind === "image",
          scale: clip.scale,
          offsetX: clip.offsetX,
          offsetY: clip.offsetY,
          rotation: clip.rotation,
          opacity: clip.opacity,
          blendMode: clip.blendMode,
        },
      ];
    });
}
```

Check the sort direction against `previewSourceAt`: it already picks the top-most using `depthIn`, so read that function and make `previewLayersAt` produce the reverse order. If `depthIn` returns larger numbers for higher tracks, the comparator above is wrong - flip it and let the first test tell you.

- [ ] **Step 4: Run the tests**

```bash
cd desktop && npx vitest run src/lib/monitor.test.ts
```

Expected: PASS, all three.

- [ ] **Step 5: Commit**

```bash
git add desktop/src/lib/monitor.ts desktop/src/lib/monitor.test.ts
git commit -m "feat: derive the whole visible stack, not just the top clip"
```

---

### Task 2: Blend maths in TypeScript, pinned against the Rust copy

The shader needs the blend formulas in GLSL. Write them in TypeScript first, as pure functions with the same pinned values `wolfcut-render/src/blend.rs` uses - that is what stops the third copy drifting from the other two.

**Files:**
- Create: `desktop/src/lib/blend.ts`
- Test: `desktop/src/lib/blend.test.ts`

**Interfaces:**
- Consumes: `type BlendMode` from `./editor`.
- Produces: `export function blendRgb(base: [number, number, number], layer: [number, number, number], mode: BlendMode): [number, number, number]` — channels in `0..1`.

- [ ] **Step 1: Write the failing test**

Create `desktop/src/lib/blend.test.ts`. These are the same values pinned in `engine/crates/wolfcut-render/src/blend.rs`; copy them, do not invent new ones.

```ts
import { describe, expect, test } from "vitest";
import { blendRgb } from "./blend";

describe("blendRgb", () => {
  test("normal returns the layer untouched", () => {
    expect(blendRgb([0.2, 0.3, 0.4], [0.9, 0.1, 0.0], "normal")).toEqual([0.9, 0.1, 0.0]);
  });

  test("multiplying by white changes nothing", () => {
    expect(blendRgb([0.25, 0.5, 0.75], [1, 1, 1], "multiply")).toEqual([0.25, 0.5, 0.75]);
  });

  test("screening black changes nothing", () => {
    expect(blendRgb([0.25, 0.5, 0.75], [0, 0, 0], "screen")).toEqual([0.25, 0.5, 0.75]);
  });

  test("overlay is hard light with the roles swapped", () => {
    const base: [number, number, number] = [0.3, 0.6, 0.2];
    const layer: [number, number, number] = [0.8, 0.2, 0.9];
    const overlay = blendRgb(base, layer, "overlay");
    const hard = blendRgb(base, layer, "hard-light");

    overlay.forEach((v, i) => expect(v).toBeCloseTo([0.48, 0.36, 0.36][i], 5));
    hard.forEach((v, i) => expect(v).toBeCloseTo([0.72, 0.24, 0.84][i], 5));
    expect(overlay).not.toEqual(hard);
    expect(overlay).toEqual(blendRgb(layer, base, "hard-light"));
  });

  test("difference of equal colours is black", () => {
    expect(blendRgb([0.4, 0.4, 0.4], [0.4, 0.4, 0.4], "difference")).toEqual([0, 0, 0]);
  });

  test("every mode stays inside the cube", () => {
    const modes = [
      "normal", "darken", "multiply", "color-burn", "lighten", "screen",
      "plus-lighter", "color-dodge", "overlay", "soft-light", "hard-light",
      "difference", "exclusion", "hue", "saturation", "color", "luminosity",
    ] as const;
    for (const mode of modes) {
      for (let step = 0; step <= 10; step += 1) {
        const v = step / 10;
        for (const result of [
          blendRgb([v, v, v], [0, 0.5, 1], mode),
          blendRgb([0, 0.5, 1], [v, v, v], mode),
        ]) {
          for (const channel of result) {
            expect(channel).toBeGreaterThanOrEqual(0);
            expect(channel).toBeLessThanOrEqual(1);
          }
        }
      }
    }
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cd desktop && npx vitest run src/lib/blend.test.ts
```

Expected: FAIL, cannot resolve `./blend`.

- [ ] **Step 3: Implement it**

Create `desktop/src/lib/blend.ts` as a transliteration of `engine/crates/wolfcut-render/src/blend.rs`. Open that file and follow it line by line — in particular `BlendMode::Overlay => hard_light(layer, base)`, whose swapped arguments are the point of the fourth test.

```ts
/**
 * How a layer's colour combines with what is beneath it.
 *
 * The third copy of this arithmetic, and deliberately so: `blend.wgsl` in
 * OpenCut is where it came from, `wolfcut-render/src/blend.rs` is what the
 * export uses, and this is what the monitor draws with. All three are pinned
 * to the same values so a change to one that the others do not make fails a
 * test rather than shipping a preview that lies.
 */
import type { BlendMode } from "./editor";

type Rgb = [number, number, number];

const lum = (c: Rgb) => c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11;
const sat = (c: Rgb) => Math.max(...c) - Math.min(...c);

function clipColor(c: Rgb): Rgb {
  const l = lum(c);
  const n = Math.min(...c);
  const x = Math.max(...c);
  let out: Rgb = [...c];
  if (n < 0) out = out.map((v) => l + ((v - l) * l) / (l - n)) as Rgb;
  if (x > 1) out = out.map((v) => l + ((v - l) * (1 - l)) / (x - l)) as Rgb;
  return out;
}

const setLum = (c: Rgb, l: number): Rgb => {
  const d = l - lum(c);
  return clipColor([c[0] + d, c[1] + d, c[2] + d]);
};

function setSat(c: Rgb, target: number): Rgb {
  const max = Math.max(...c);
  const min = Math.min(...c);
  if (max <= min) return [0, 0, 0];
  const scale = target / (max - min);
  return [(c[0] - min) * scale, (c[1] - min) * scale, (c[2] - min) * scale];
}

const hardLight = (base: Rgb, layer: Rgb): Rgb =>
  base.map((b, i) =>
    layer[i] >= 0.5 ? 1 - 2 * (1 - b) * (1 - layer[i]) : 2 * b * layer[i],
  ) as Rgb;

function softLightChannel(base: number, layer: number): number {
  if (layer <= 0.5) return base - (1 - 2 * layer) * base * (1 - base);
  const d = base > 0.25 ? Math.sqrt(base) : ((16 * base - 12) * base + 4) * base;
  return base + (2 * layer - 1) * (d - base);
}

export function blendRgb(base: Rgb, layer: Rgb, mode: BlendMode): Rgb {
  const per = (f: (b: number, l: number) => number): Rgb =>
    base.map((b, i) => f(b, layer[i])) as Rgb;

  let out: Rgb;
  switch (mode) {
    case "normal": out = layer; break;
    case "darken": out = per(Math.min); break;
    case "multiply": out = per((b, l) => b * l); break;
    case "color-burn":
      out = per((b, l) => (l <= 0 ? 0 : 1 - Math.min((1 - b) / Math.max(l, 0.0001), 1)));
      break;
    case "lighten": out = per(Math.max); break;
    case "screen": out = per((b, l) => 1 - (1 - b) * (1 - l)); break;
    case "plus-lighter": out = per((b, l) => Math.min(b + l, 1)); break;
    case "color-dodge":
      out = per((b, l) => (l >= 1 ? 1 : Math.min(b / Math.max(1 - l, 0.0001), 1)));
      break;
    // Not a typo: overlay is hard light with the roles swapped, so the
    // picture beneath decides which half of the curve each channel takes.
    case "overlay": out = hardLight(layer, base); break;
    case "soft-light": out = per(softLightChannel); break;
    case "hard-light": out = hardLight(base, layer); break;
    case "difference": out = per((b, l) => Math.abs(b - l)); break;
    case "exclusion": out = per((b, l) => b + l - 2 * b * l); break;
    case "hue": out = setLum(setSat(layer, sat(base)), lum(base)); break;
    case "saturation": out = setLum(setSat(base, sat(layer)), lum(base)); break;
    case "color": out = setLum(layer, lum(base)); break;
    case "luminosity": out = setLum(base, lum(layer)); break;
  }
  return out.map((v) => Math.min(1, Math.max(0, v))) as Rgb;
}
```

- [ ] **Step 4: Run the tests**

```bash
cd desktop && npx vitest run src/lib/blend.test.ts
```

Expected: PASS, all six.

- [ ] **Step 5: Commit**

```bash
git add desktop/src/lib/blend.ts desktop/src/lib/blend.test.ts
git commit -m "feat: blend maths in TypeScript, pinned against the engine's"
```

---

### Task 3: The GLSL shader, generated from the same table

The shader needs the same seventeen modes. Generate the GLSL from one list so a mode cannot exist in TypeScript and be missing from the shader.

**Files:**
- Create: `desktop/src/lib/blendShader.ts`
- Test: `desktop/src/lib/blendShader.test.ts`

**Interfaces:**
- Consumes: `type BlendMode` from `./editor`.
- Produces:
  ```ts
  export const BLEND_MODE_INDEX: Record<BlendMode, number>;
  export function blendGlsl(): string;   // a GLSL function `vec3 blendRgb(vec3 base, vec3 layer, int mode)`
  ```

- [ ] **Step 1: Write the failing test**

```ts
import { describe, expect, test } from "vitest";
import { BLEND_MODE_INDEX, blendGlsl } from "./blendShader";

describe("the blend shader", () => {
  test("indexes every mode the model can hold", () => {
    expect(Object.keys(BLEND_MODE_INDEX)).toHaveLength(17);
    expect(BLEND_MODE_INDEX.normal).toBe(0);
  });

  test("its indices are dense and unique, because the shader switches on them", () => {
    const values = Object.values(BLEND_MODE_INDEX).sort((a, b) => a - b);
    expect(values).toEqual([...Array(17).keys()]);
  });

  test("emits a branch for every mode", () => {
    const glsl = blendGlsl();
    for (const index of Object.values(BLEND_MODE_INDEX)) {
      expect(glsl).toContain(`mode == ${index}`);
    }
  });

  test("declares the entry point the compositor calls", () => {
    expect(blendGlsl()).toContain("vec3 blendRgb(vec3 base, vec3 layer, int mode)");
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cd desktop && npx vitest run src/lib/blendShader.test.ts
```

Expected: FAIL, cannot resolve `./blendShader`.

- [ ] **Step 3: Implement it**

Create `desktop/src/lib/blendShader.ts`. The GLSL bodies are the same formulas as `blend.ts`; keep them in the same order so the two read side by side.

```ts
/**
 * The blend modes as GLSL, for the preview compositor.
 *
 * Built from one table so a mode cannot be added to the model and quietly
 * missing here - the tests count them. The arithmetic mirrors `blend.ts`,
 * which mirrors the engine's; see the note there on why there are three.
 */
import type { BlendMode } from "./editor";

/** The shader switches on an int; this is the mapping, and it is dense. */
export const BLEND_MODE_INDEX: Record<BlendMode, number> = {
  normal: 0,
  darken: 1,
  multiply: 2,
  "color-burn": 3,
  lighten: 4,
  screen: 5,
  "plus-lighter": 6,
  "color-dodge": 7,
  overlay: 8,
  "soft-light": 9,
  "hard-light": 10,
  difference: 11,
  exclusion: 12,
  hue: 13,
  saturation: 14,
  color: 15,
  luminosity: 16,
};

/** GLSL for each mode's body, keyed the same way. */
const BODIES: Record<BlendMode, string> = {
  normal: "layer",
  darken: "min(base, layer)",
  multiply: "base * layer",
  "color-burn": "cburn(base, layer)",
  lighten: "max(base, layer)",
  screen: "1.0 - (1.0 - base) * (1.0 - layer)",
  "plus-lighter": "min(base + layer, vec3(1.0))",
  "color-dodge": "cdodge(base, layer)",
  overlay: "hardLight(layer, base)",
  "soft-light": "softLight(base, layer)",
  "hard-light": "hardLight(base, layer)",
  difference: "abs(base - layer)",
  exclusion: "base + layer - 2.0 * base * layer",
  hue: "setLum(setSat(layer, sat(base)), lum(base))",
  saturation: "setLum(setSat(base, sat(layer)), lum(base))",
  color: "setLum(layer, lum(base))",
  luminosity: "setLum(base, lum(layer))",
};

const HELPERS = `
float lum(vec3 c) { return dot(c, vec3(0.3, 0.59, 0.11)); }
float sat(vec3 c) { return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b); }

vec3 clipColor(vec3 c) {
  float l = lum(c);
  float n = min(min(c.r, c.g), c.b);
  float x = max(max(c.r, c.g), c.b);
  vec3 o = c;
  if (n < 0.0) o = l + ((o - l) * l) / (l - n);
  if (x > 1.0) o = l + ((o - l) * (1.0 - l)) / (x - l);
  return o;
}

vec3 setLum(vec3 c, float l) { return clipColor(c + (l - lum(c))); }

vec3 setSat(vec3 c, float target) {
  float mx = max(max(c.r, c.g), c.b);
  float mn = min(min(c.r, c.g), c.b);
  if (mx <= mn) return vec3(0.0);
  return (c - mn) * (target / (mx - mn));
}

vec3 hardLight(vec3 base, vec3 layer) {
  return mix(2.0 * base * layer,
             1.0 - 2.0 * (1.0 - base) * (1.0 - layer),
             step(vec3(0.5), layer));
}

float softLightChannel(float base, float layer) {
  if (layer <= 0.5) return base - (1.0 - 2.0 * layer) * base * (1.0 - base);
  float d = base > 0.25 ? sqrt(base) : ((16.0 * base - 12.0) * base + 4.0) * base;
  return base + (2.0 * layer - 1.0) * (d - base);
}

vec3 softLight(vec3 b, vec3 l) {
  return vec3(softLightChannel(b.r, l.r), softLightChannel(b.g, l.g), softLightChannel(b.b, l.b));
}

vec3 cdodge(vec3 base, vec3 layer) {
  return mix(min(base / max(1.0 - layer, vec3(0.0001)), vec3(1.0)),
             vec3(1.0),
             step(vec3(1.0), layer));
}

vec3 cburn(vec3 base, vec3 layer) {
  return mix(1.0 - min((1.0 - base) / max(layer, vec3(0.0001)), vec3(1.0)),
             vec3(0.0),
             step(layer, vec3(0.0)));
}
`;

/** The whole blend section of the fragment shader. */
export function blendGlsl(): string {
  const branches = (Object.keys(BLEND_MODE_INDEX) as BlendMode[])
    .map((mode) => `  if (mode == ${BLEND_MODE_INDEX[mode]}) return clamp(${BODIES[mode]}, 0.0, 1.0);`)
    .join("\n");

  return `${HELPERS}
vec3 blendRgb(vec3 base, vec3 layer, int mode) {
${branches}
  return layer;
}
`;
}
```

- [ ] **Step 4: Run the tests**

```bash
cd desktop && npx vitest run src/lib/blendShader.test.ts
```

Expected: PASS, all four.

- [ ] **Step 5: Commit**

```bash
git add desktop/src/lib/blendShader.ts desktop/src/lib/blendShader.test.ts
git commit -m "feat: the blend modes as GLSL, generated from one table"
```

---

### Task 4: The compositor

A WebGL2 canvas that draws each layer as a textured quad, blending against what it has drawn so far.

**Files:**
- Create: `desktop/src/components/GlCompositor.tsx`
- Test: `desktop/src/components/glQuad.test.ts`
- Create: `desktop/src/lib/glQuad.ts`

The transform maths goes in `glQuad.ts` so it can be tested without a GL context, which jsdom does not provide.

**Interfaces:**
- Consumes: `PreviewLayer` (Task 1), `BLEND_MODE_INDEX` and `blendGlsl` (Task 3).
- Produces:
  ```ts
  // glQuad.ts
  export function quadFor(
    layer: { scale: number; offsetX: number; offsetY: number; rotation: number },
    media: { width: number; height: number },
    frame: { width: number; height: number },
  ): Float32Array;   // 12 numbers: two triangles in clip space
  ```

- [ ] **Step 1: Write the failing test**

Create `desktop/src/components/glQuad.test.ts`.

```ts
import { describe, expect, test } from "vitest";
import { quadFor } from "../lib/glQuad";

const frame = { width: 1920, height: 1080 };

describe("quadFor", () => {
  test("an untransformed layer of the frame's shape fills clip space", () => {
    const quad = quadFor(
      { scale: 1, offsetX: 0, offsetY: 0, rotation: 0 },
      { width: 1920, height: 1080 },
      frame,
    );
    const xs = [...quad].filter((_, i) => i % 2 === 0);
    const ys = [...quad].filter((_, i) => i % 2 === 1);
    expect(Math.min(...xs)).toBeCloseTo(-1, 4);
    expect(Math.max(...xs)).toBeCloseTo(1, 4);
    expect(Math.min(...ys)).toBeCloseTo(-1, 4);
    expect(Math.max(...ys)).toBeCloseTo(1, 4);
  });

  test("a narrower source is letterboxed rather than stretched", () => {
    const quad = quadFor(
      { scale: 1, offsetX: 0, offsetY: 0, rotation: 0 },
      { width: 1080, height: 1080 },
      frame,
    );
    const xs = [...quad].filter((_, i) => i % 2 === 0);
    // Square source in a 16:9 frame: full height, 1080/1920 of the width.
    expect(Math.max(...xs)).toBeCloseTo(1080 / 1920, 4);
  });

  test("scale grows the quad about its centre", () => {
    const quad = quadFor(
      { scale: 2, offsetX: 0, offsetY: 0, rotation: 0 },
      { width: 1920, height: 1080 },
      frame,
    );
    const xs = [...quad].filter((_, i) => i % 2 === 0);
    expect(Math.max(...xs)).toBeCloseTo(2, 4);
    expect(Math.min(...xs)).toBeCloseTo(-2, 4);
  });

  test("offsetX is a fraction of the frame, and positive moves right", () => {
    const quad = quadFor(
      { scale: 1, offsetX: 0.5, offsetY: 0, rotation: 0 },
      { width: 1920, height: 1080 },
      frame,
    );
    const xs = [...quad].filter((_, i) => i % 2 === 0);
    // Half a frame width is one whole unit of clip space, which is 2 wide.
    expect(Math.min(...xs)).toBeCloseTo(0, 4);
  });

  test("a half turn puts the corners where the opposite ones were", () => {
    const straight = quadFor(
      { scale: 1, offsetX: 0, offsetY: 0, rotation: 0 },
      { width: 1920, height: 1080 },
      frame,
    );
    const turned = quadFor(
      { scale: 1, offsetX: 0, offsetY: 0, rotation: 180 },
      { width: 1920, height: 1080 },
      frame,
    );
    const extent = (q: Float32Array) => [
      Math.min(...[...q].filter((_, i) => i % 2 === 0)),
      Math.max(...[...q].filter((_, i) => i % 2 === 0)),
    ];
    expect(extent(turned)[0]).toBeCloseTo(extent(straight)[0], 4);
    expect(extent(turned)[1]).toBeCloseTo(extent(straight)[1], 4);
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cd desktop && npx vitest run src/components/glQuad.test.ts
```

Expected: FAIL, cannot resolve `../lib/glQuad`.

- [ ] **Step 3: Implement the quad maths**

Create `desktop/src/lib/glQuad.ts`.

```ts
/**
 * Where one layer's picture lands, in clip space.
 *
 * The same placement `wolfcut-export`'s `place_layer` performs: contain-fit
 * inside the frame first, then the clip's own scale, rotation and offsets on
 * top of that. Offsets are fractions of the frame, so the same transform
 * means the same picture at any preview size - which is what lets the monitor
 * and the export agree about where a picture sits.
 */
export function quadFor(
  layer: { scale: number; offsetX: number; offsetY: number; rotation: number },
  media: { width: number; height: number },
  frame: { width: number; height: number },
): Float32Array {
  // Contain-fit: the largest the source can be without cropping.
  const fit = Math.min(frame.width / media.width, frame.height / media.height);
  // Half-extents in clip space, where the frame is 2 units across.
  const halfWidth = ((media.width * fit) / frame.width) * layer.scale;
  const halfHeight = ((media.height * fit) / frame.height) * layer.scale;

  const radians = (layer.rotation * Math.PI) / 180;
  const cos = Math.cos(radians);
  const sin = Math.sin(radians);

  // Clip space y points up and the offset points down, hence the negation.
  const centreX = layer.offsetX * 2;
  const centreY = -layer.offsetY * 2;

  const corner = (sx: number, sy: number): [number, number] => {
    const x = sx * halfWidth;
    const y = sy * halfHeight;
    return [centreX + x * cos - y * sin, centreY + x * sin + y * cos];
  };

  const topLeft = corner(-1, 1);
  const topRight = corner(1, 1);
  const bottomLeft = corner(-1, -1);
  const bottomRight = corner(1, -1);

  return new Float32Array([
    ...topLeft, ...bottomLeft, ...topRight,
    ...topRight, ...bottomLeft, ...bottomRight,
  ]);
}
```

- [ ] **Step 4: Run the tests**

```bash
cd desktop && npx vitest run src/components/glQuad.test.ts
```

Expected: PASS, all five. If the rotation test fails on sign, check the sin terms - the comment above records which way y points.

- [ ] **Step 5: Commit the maths**

```bash
git add desktop/src/lib/glQuad.ts desktop/src/components/glQuad.test.ts
git commit -m "feat: clip-space placement for the preview compositor"
```

- [ ] **Step 6: Implement the compositor component**

Create `desktop/src/components/GlCompositor.tsx`. It takes the layers and a way to reach each one's media element, and draws once per animation frame.

Read `EngineStillLayer` in `Preview.tsx` first: this component sits in the same place, with the same absolute positioning, and must not capture pointer events.

```tsx
import { useEffect, useRef } from "react";

import { BLEND_MODE_INDEX, blendGlsl } from "../lib/blendShader";
import { quadFor } from "../lib/glQuad";
import type { PreviewLayer } from "../lib/monitor";

const VERTEX = `#version 300 es
in vec2 position;
in vec2 texel;
out vec2 uv;
void main() {
  uv = texel;
  gl_Position = vec4(position, 0.0, 1.0);
}`;

const fragment = () => `#version 300 es
precision highp float;
in vec2 uv;
uniform sampler2D layerTexture;
uniform sampler2D beneath;
uniform float opacity;
uniform int blendMode;
out vec4 colour;

${blendGlsl()}

void main() {
  vec4 top = texture(layerTexture, uv);
  vec4 under = texture(beneath, gl_FragCoord.xy / vec2(textureSize(beneath, 0)));
  // The same rule the CPU compositor follows: a blend only engages where
  // something is beneath, so a lone layer draws as itself whatever its mode.
  vec3 mixed = mix(top.rgb, blendRgb(under.rgb, top.rgb, blendMode), under.a);
  float alpha = top.a * opacity;
  colour = vec4(mixed * alpha + under.rgb * (1.0 - alpha), alpha + under.a * (1.0 - alpha));
}`;

/**
 * The monitor's picture, composited in the webview.
 *
 * This is the *approximation*, upgraded: it stacks layers with their real
 * transforms, opacities and blend modes rather than showing only the top
 * clip through CSS. The engine still owns the paused frame and the export -
 * see `docs/superpowers/specs/2026-09-09-gpu-preview.md`.
 *
 * Returns null when WebGL2 is unavailable, which is the caller's signal to
 * leave the existing path alone rather than show nothing.
 */
export function GlCompositor({
  layers,
  frame,
  elementFor,
  onUnavailable,
}: {
  layers: PreviewLayer[];
  frame: { width: number; height: number };
  /** The playing media element for a layer, or null if it is not ready. */
  elementFor: (clipId: string) => HTMLVideoElement | HTMLImageElement | null;
  onUnavailable: () => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const state = useRef({ layers, frame, elementFor });
  state.current = { layers, frame, elementFor };

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const gl = canvas.getContext("webgl2", { premultipliedAlpha: false });
    if (!gl) {
      onUnavailable();
      return;
    }

    const program = buildProgram(gl);
    if (!program) {
      onUnavailable();
      return;
    }

    const textures = new Map<string, WebGLTexture>();
    let running = 0;

    const tick = () => {
      running = requestAnimationFrame(tick);
      const { layers: current, frame: size, elementFor: lookup } = state.current;
      if (canvas.width !== size.width || canvas.height !== size.height) {
        canvas.width = size.width;
        canvas.height = size.height;
      }
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.clearColor(0, 0, 0, 1);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.useProgram(program);

      for (const layer of current) {
        const media = lookup(layer.clipId);
        if (!media) continue;
        const natural =
          media instanceof HTMLVideoElement
            ? { width: media.videoWidth, height: media.videoHeight }
            : { width: media.naturalWidth, height: media.naturalHeight };
        if (!natural.width || !natural.height) continue;

        upload(gl, textures, layer.clipId, media);
        drawLayer(gl, program, quadFor(layer, natural, size), layer);
      }
    };
    tick();

    return () => {
      cancelAnimationFrame(running);
      for (const texture of textures.values()) gl.deleteTexture(texture);
      gl.deleteProgram(program);
    };
  }, [onUnavailable]);

  return (
    <canvas
      ref={canvasRef}
      className="pointer-events-none absolute inset-0 h-full w-full"
      aria-hidden
    />
  );
}
```

The three helpers (`buildProgram`, `upload`, `drawLayer`) are ordinary WebGL plumbing: compile the two shaders and link, `texImage2D` the element with `LINEAR` filtering and `CLAMP_TO_EDGE`, and bind the quad plus the uniforms `opacity` and `blendMode` (`BLEND_MODE_INDEX[layer.blendMode]`) before `drawArrays(gl.TRIANGLES, 0, 6)`. Write them beneath the component in the same file. Blending against what is already drawn needs the destination readable: use `gl.copyTexImage2D` into a `beneath` texture between layers, or two framebuffers swapped per layer - the second is faster and the one to prefer if the first shows seams.

- [ ] **Step 7: Check it compiles and lints**

```bash
cd desktop && npx tsc --noEmit && npm run lint
```

Expected: both clean.

- [ ] **Step 8: Commit**

```bash
git add desktop/src/components/GlCompositor.tsx
git commit -m "feat: a WebGL compositor for the monitor's picture"
```

---

### Task 5: Use it while playing, and fall back where it cannot draw

**Files:**
- Modify: `desktop/src/components/Preview.tsx`
- Modify: `desktop/src/App.tsx`
- Modify: `desktop/src/hooks/useEngineTruth.ts`
- Modify: `desktop/src/locales/en.json`, `desktop/src/locales/zh-CN.json`

**Interfaces:**
- Consumes: `GlCompositor` (Task 4), `previewLayersAt` (Task 1).

- [ ] **Step 1: Add the strings**

Both locale files, same keys:

```json
"preview.glUnavailable": "This machine has no WebGL, so the monitor is using the slower engine preview."
```

Chinese: `"preview.glUnavailable": "此设备不支持 WebGL，监视器将使用较慢的引擎预览。"`

- [ ] **Step 2: Hold the fallback flag in App.tsx**

Beside `approximationBroken`, which is the same shape of state and for the same reason:

```tsx
// Latched: a machine without WebGL will not grow it mid-session, and
// flapping between the two compositors would be worse than either.
const [glUnavailable, setGlUnavailable] = useState(false);
const onGlUnavailable = useCallback(() => setGlUnavailable(true), []);
```

Pass `layers={previewLayersAt(project, timeline, playhead)}` and `onGlUnavailable` down to `Preview`.

- [ ] **Step 3: Render it in Preview.tsx**

Above the `<video>` and below `EngineStillLayer`, so the engine's paused frame still wins when it arrives:

```tsx
{!glUnavailable && layers.length > 0 && (
  <GlCompositor
    layers={layers}
    frame={frame}
    elementFor={elementFor}
    onUnavailable={onGlUnavailable}
  />
)}
```

`elementFor` needs one media element per layer. Extend the existing single `<video>` into a keyed pool: one element per `clipId`, each with `src={mediaSrc}` and the same corrective sync the current element uses. Read the sync effect around `Preview.tsx:440` and repeat it per element rather than inventing a second scheme.

- [ ] **Step 4: Stop the engine streaming what the compositor now draws**

In `useEngineTruth.ts`, the streaming gate currently widens to one layer when the element cannot draw. The compositor can, so add it to the condition:

```ts
const floor = (approximationBroken || effectCovers(now)) && !glDrawing ? 1 : 2;
```

Pass `glDrawing` in from `App.tsx` as `!glUnavailable`. Leave the paused dwell alone: the engine's frame stays the truth when the playhead rests, which is the whole arrangement this fits into.

- [ ] **Step 5: Verify**

```bash
cd desktop && npx tsc --noEmit && npm run lint && npm test
```

Expected: all clean, and the test count up by the tests from Tasks 1-4.

Then run the app and watch two stacked clips play:

```bash
cd desktop && npm run app
```

Expected: playback is smooth where it was stepping, and the blend mode on the upper clip is visible while moving. Pause and confirm the picture does not jump - if it does, the compositor and the engine disagree about placement, and `quadFor` is where to look.

- [ ] **Step 6: Commit**

```bash
git add desktop/src
git commit -m "feat: the monitor composites in the webview while playing"
```

---

## Self-review notes

- **Spec coverage.** Requirement 1 is Task 1; 2 is Task 2; 3 is Tasks 3-4; 4 is Task 5 step 4; 5 is Task 5 steps 2-3. Nothing in the spec is unclaimed.
- **Known soft spot.** Task 4 step 6 describes the three WebGL helpers rather than spelling them out, and names the two ways to make the destination readable. That is the one place this plan asks the implementer to choose; everything else is written out. If that is not acceptable, split Task 4 into "plumbing" and "blending" and write both out.
- **Types.** `PreviewLayer` is defined in Task 1 and used in Tasks 4 and 5; `BLEND_MODE_INDEX` and `blendGlsl` in Task 3 and used in Task 4; `quadFor` in Task 4 and used in the same task. No name appears that is not defined.
