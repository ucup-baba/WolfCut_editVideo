import { describe, expect, test } from "vitest";

import { laneHeight, packRows } from "./EffectLane";
import type { TimelineEffect } from "../lib/editor";

function effect(id: string, start: number, duration: number): TimelineEffect {
  return {
    id,
    effectId: "gaussian-blur",
    params: {},
    start,
    duration,
    easeIn: 0,
    easeOut: 0,
    enabled: true,
  };
}

describe("packRows", () => {
  test("effects that never meet all share one row", () => {
    const { rowOf, rows } = packRows([
      effect("a", 0, 2),
      effect("b", 2, 2),
      effect("c", 6, 1),
    ]);
    expect(rows).toBe(1);
    expect([...rowOf.values()]).toEqual([0, 0, 0]);
  });

  test("an overlap opens a second row rather than stacking in place", () => {
    const { rowOf, rows } = packRows([effect("a", 0, 4), effect("b", 2, 4)]);
    expect(rows).toBe(2);
    expect(rowOf.get("a")).toBe(0);
    expect(rowOf.get("b")).toBe(1);
  });

  test("a row is reused as soon as it is free again", () => {
    // c starts after a ends, so it belongs on a's row even though b is still
    // running - otherwise the lane would grow a row per effect.
    const { rowOf, rows } = packRows([
      effect("a", 0, 2),
      effect("b", 1, 6),
      effect("c", 3, 2),
    ]);
    expect(rows).toBe(2);
    expect(rowOf.get("a")).toBe(0);
    expect(rowOf.get("b")).toBe(1);
    expect(rowOf.get("c")).toBe(0);
  });

  test("three at once need three rows", () => {
    const { rows } = packRows([effect("a", 0, 5), effect("b", 1, 5), effect("c", 2, 5)]);
    expect(rows).toBe(3);
  });

  test("the layout does not depend on the order they are stored in", () => {
    const laid = [effect("a", 0, 4), effect("b", 2, 4), effect("c", 8, 1)];
    const forwards = packRows(laid);
    const backwards = packRows([...laid].reverse());
    expect([...backwards.rowOf.entries()].sort()).toEqual(
      [...forwards.rowOf.entries()].sort(),
    );
  });

  test("touching end to end is not an overlap", () => {
    const { rows } = packRows([effect("a", 0, 2), effect("b", 2, 2)]);
    expect(rows).toBe(1);
  });

  test("an empty lane still has a row to be empty in", () => {
    expect(packRows([]).rows).toBe(1);
  });
});

describe("laneHeight", () => {
  test("grows with the rows and never collapses", () => {
    expect(laneHeight(2)).toBeGreaterThan(laneHeight(1));
    expect(laneHeight(1)).toBeGreaterThan(0);
  });
});
