import { useRef, useState, type PointerEvent as ReactPointerEvent } from "react";

import type { TimelineEffect } from "../lib/editor";
import { useLocale } from "../lib/i18n";

/** One row of blocks. Shorter than a track: an effect is a span, not a picture. */
const ROW_HEIGHT = 26;

/** Room above and below the rows, so blocks are not flush against the edges. */
const LANE_PADDING = 4;

/**
 * Which row each effect draws on, and how many rows that needs.
 *
 * Two effects covering the same instant is a real thing to want - a blur
 * under a grain, both fading - so overlap is allowed. Drawn on one row it
 * reads as a mistake rather than a stack, which is what a calendar solves the
 * same way: an effect takes the first row where nothing already sits under
 * it, and the lane grows to fit.
 */
export function packRows(effects: readonly TimelineEffect[]): {
  rowOf: Map<string, number>;
  rows: number;
} {
  const rowOf = new Map<string, number>();
  /** Where each row is free from, in seconds. */
  const freeFrom: number[] = [];

  // In time order, so the packing does not depend on the order they happen
  // to be stored in - two documents with the same effects must draw alike.
  for (const effect of [...effects].sort((a, b) => a.start - b.start || a.id.localeCompare(b.id))) {
    let row = freeFrom.findIndex((from) => from <= effect.start);
    if (row === -1) {
      row = freeFrom.length;
      freeFrom.push(0);
    }
    freeFrom[row] = effect.start + effect.duration;
    rowOf.set(effect.id, row);
  }
  return { rowOf, rows: Math.max(1, freeFrom.length) };
}

/** How tall the lane has to be to hold `rows` of blocks. */
export function laneHeight(rows: number): number {
  return rows * ROW_HEIGHT + LANE_PADDING;
}

/** How near an edge counts as grabbing it rather than the block. */
const EDGE_GRAB = 7;

/** What a drag is doing, decided at pointer-down and fixed for its duration. */
type Grip = "move" | "start" | "end" | "easeIn" | "easeOut";

interface Drag {
  id: string;
  grip: Grip;
  /** Pointer x where the drag began, in client pixels. */
  fromX: number;
  /** The effect as it was before the drag - every delta is against this. */
  origin: TimelineEffect;
}

/**
 * The effect lane: timeline effects as blocks you drag, trim and fade.
 *
 * Drawn in the DOM rather than into the timeline's canvas, and deliberately.
 * The canvas owns clips, whose hit-testing, selection and drag state are one
 * machine; an effect is a different object with different gestures, and
 * threading it through that machine would make both harder to follow. This
 * lane shares only the two numbers that matter for staying aligned - seconds
 * per pixel, and where the view is scrolled to.
 *
 * Gestures land locally first and commit once on release, so a drag is one
 * engine command and one undo step rather than sixty.
 */
export function EffectLane({
  effects,
  secondsPerPixel,
  scrollLeft,
  selectedId,
  onSelect,
  onCommit,
  onRemove,
}: {
  effects: readonly TimelineEffect[];
  secondsPerPixel: number;
  /** Where the view starts, in seconds - the canvas's own scroll position. */
  scrollLeft: number;
  selectedId: string | null;
  onSelect: (effectId: string | null) => void;
  /** A gesture finished: the accumulated change as one engine command. */
  onCommit: (effectId: string, patch: Partial<TimelineEffect>) => void;
  onRemove: (effectId: string) => void;
}) {
  const { t } = useLocale();
  const drag = useRef<Drag | null>(null);
  /** The effect being dragged, as it looks right now. Null when idle. */
  const [live, setLive] = useState<TimelineEffect | null>(null);

  const toX = (seconds: number) => (seconds - scrollLeft) / secondsPerPixel;

  const shown = (effect: TimelineEffect) =>
    live && live.id === effect.id ? live : effect;

  // Packed from what is drawn, not from what is stored, so a block being
  // dragged finds its own row as it moves rather than sitting over another.
  const { rowOf, rows } = packRows(effects.map((effect) => shown(effect)));

  const gripAt = (effect: TimelineEffect, offsetX: number): Grip => {
    const width = effect.duration / secondsPerPixel;
    if (offsetX <= EDGE_GRAB) return "start";
    if (offsetX >= width - EDGE_GRAB) return "end";
    // The ease grips sit where the ramps end, and only once there is a ramp
    // to grab. Before that the corner handles below are how you make one.
    const easeInX = effect.easeIn / secondsPerPixel;
    const easeOutX = width - effect.easeOut / secondsPerPixel;
    if (effect.easeIn > 0 && Math.abs(offsetX - easeInX) <= EDGE_GRAB) return "easeIn";
    if (effect.easeOut > 0 && Math.abs(offsetX - easeOutX) <= EDGE_GRAB) return "easeOut";
    return "move";
  };

  const begin = (effect: TimelineEffect, grip: Grip) => (event: ReactPointerEvent) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { id: effect.id, grip, fromX: event.clientX, origin: effect };
    setLive(effect);
    onSelect(effect.id);
  };

  const move = (event: ReactPointerEvent) => {
    const state = drag.current;
    if (!state) return;
    const delta = (event.clientX - state.fromX) * secondsPerPixel;
    setLive(apply(state, delta));
  };

  const end = (event: ReactPointerEvent) => {
    const state = drag.current;
    if (!state) return;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    const settled = apply(state, (event.clientX - state.fromX) * secondsPerPixel);
    drag.current = null;
    setLive(null);
    // Only what the gesture could have changed, so a move does not also
    // rewrite the eases and a trim does not rewrite the start.
    onCommit(settled.id, {
      start: settled.start,
      duration: settled.duration,
      easeIn: settled.easeIn,
      easeOut: settled.easeOut,
    });
  };

  return (
    <div
      className="relative min-w-0 flex-1 overflow-hidden bg-sunken/40"
      style={{ height: laneHeight(rows) }}
      onPointerDown={() => onSelect(null)}
    >
      {effects.map((stored) => {
        const effect = shown(stored);
        const left = toX(effect.start);
        const width = Math.max(2, effect.duration / secondsPerPixel);
        const selected = selectedId === effect.id;

        return (
          <div
            key={effect.id}
            role="button"
            tabIndex={0}
            aria-label={t("effectLane.block", { name: effect.effectId })}
            title={effect.effectId}
            onPointerDown={(event) =>
              begin(effect, gripAt(effect, event.nativeEvent.offsetX))(event)
            }
            onPointerMove={move}
            onPointerUp={end}
            onKeyDown={(event) => {
              if (event.key === "Delete" || event.key === "Backspace") {
                event.preventDefault();
                onRemove(effect.id);
              }
            }}
            className={`absolute flex h-[22px] cursor-grab items-center overflow-hidden
                        rounded border text-[10px] active:cursor-grabbing ${
                          selected
                            ? "border-accent bg-accent-soft text-primary"
                            : "border-hairline-strong bg-panel text-secondary"
                        } ${effect.enabled ? "" : "opacity-40"}`}
            style={{ left, width, top: (rowOf.get(effect.id) ?? 0) * ROW_HEIGHT + LANE_PADDING / 2 }}
          >
            {/* The ramps, drawn as the wedges they are. Purely a picture -
                the grips that change them are hit-tested in `gripAt`. */}
            {effect.easeIn > 0 && (
              <span
                aria-hidden
                className="pointer-events-none absolute inset-y-0 left-0 bg-accent/25"
                style={{
                  width: effect.easeIn / secondsPerPixel,
                  clipPath: "polygon(0 100%, 100% 0, 100% 100%)",
                }}
              />
            )}
            {effect.easeOut > 0 && (
              <span
                aria-hidden
                className="pointer-events-none absolute inset-y-0 right-0 bg-accent/25"
                style={{
                  width: effect.easeOut / secondsPerPixel,
                  clipPath: "polygon(0 0, 0 100%, 100% 100%)",
                }}
              />
            )}
            <span className="pointer-events-none truncate px-1.5">{effect.effectId}</span>

            {/* Corner handles: drag either one inwards to make a ramp where
                there is none yet, which `gripAt` cannot offer until one
                exists. */}
            <span
              onPointerDown={begin(effect, "easeIn")}
              onPointerMove={move}
              onPointerUp={end}
              title={t("effectLane.easeIn")}
              className="absolute left-0 top-0 h-2 w-2 cursor-ew-resize rounded-br bg-accent/70"
            />
            <span
              onPointerDown={begin(effect, "easeOut")}
              onPointerMove={move}
              onPointerUp={end}
              title={t("effectLane.easeOut")}
              className="absolute right-0 top-0 h-2 w-2 cursor-ew-resize rounded-bl bg-accent/70"
            />
          </div>
        );
      })}
    </div>
  );
}

/**
 * The effect as the drag has it now.
 *
 * Every gesture is clamped so the result is a thing that can exist: nothing
 * starts before zero, nothing is shorter than a frame or two, and neither
 * ramp may outrun the span. The engine clamps again - this is so the block
 * under the pointer never draws a shape the engine would refuse.
 */
function apply(state: Drag, delta: number): TimelineEffect {
  const { origin, grip } = state;
  const floor = 1 / 30;

  switch (grip) {
    case "move":
      return { ...origin, start: Math.max(0, origin.start + delta) };

    case "start": {
      // The tail stays put: dragging the head is a trim, not a move.
      const shift = Math.min(Math.max(delta, -origin.start), origin.duration - floor);
      return {
        ...origin,
        start: origin.start + shift,
        duration: origin.duration - shift,
        easeIn: Math.min(origin.easeIn, origin.duration - shift),
        easeOut: Math.min(origin.easeOut, origin.duration - shift),
      };
    }

    case "end": {
      const duration = Math.max(floor, origin.duration + delta);
      return {
        ...origin,
        duration,
        easeIn: Math.min(origin.easeIn, duration),
        easeOut: Math.min(origin.easeOut, duration),
      };
    }

    case "easeIn": {
      const easeIn = clampEase(origin.easeIn + delta, origin.duration - origin.easeOut);
      return { ...origin, easeIn };
    }

    case "easeOut": {
      // Dragging the right-hand grip leftwards lengthens the ramp, so the
      // delta runs the other way.
      const easeOut = clampEase(origin.easeOut - delta, origin.duration - origin.easeIn);
      return { ...origin, easeOut };
    }
  }
}

/** A ramp is never negative and never eats the room the other one needs. */
function clampEase(value: number, room: number): number {
  return Math.min(Math.max(0, value), Math.max(0, room));
}
