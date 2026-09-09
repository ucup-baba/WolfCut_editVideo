import { describe, expect, test } from "vitest";

import { NO_WORD_LIMIT, splitSegment, splitSegments } from "./captions";
import type { TranscribedSegment } from "./engine";

function segment(text: string, start = 0, end = 10): TranscribedSegment {
  return { start, end, text };
}

describe("splitSegment", () => {
  test("leaves a segment already within the limit alone", () => {
    const short = segment("three little words");
    expect(splitSegment(short, 4)).toEqual([short]);
    expect(splitSegment(short, 3)).toEqual([short]);
  });

  test("no limit means whisper's own segmentation stands", () => {
    const long = segment("one two three four five six seven eight nine");
    expect(splitSegment(long, NO_WORD_LIMIT)).toEqual([long]);
    expect(splitSegment(long, -1)).toEqual([long]);
  });

  test("splits evenly rather than leaving an orphan", () => {
    // Nine words at four would be 4+4+1 greedily. Three even pieces read
    // better and nothing is lost by preferring them.
    const pieces = splitSegment(segment("one two three four five six seven eight nine"), 4);
    expect(pieces.map((piece) => piece.text)).toEqual([
      "one two three",
      "four five six",
      "seven eight nine",
    ]);
  });

  test("spreads the remainder one word at a time, longest first", () => {
    // Eight words, limit three: two pieces cannot hold them, so three do -
    // 3+3+2, never 3+2+3.
    const pieces = splitSegment(segment("a b c d e f g h"), 3);
    expect(pieces.map((piece) => piece.text.split(" ").length)).toEqual([3, 3, 2]);
  });

  test("shares the segment's span by word count and keeps its bounds", () => {
    const pieces = splitSegment(segment("one two three four", 4, 8), 2);
    expect(pieces).toEqual([
      { start: 4, end: 6, text: "one two" },
      { start: 6, end: 8, text: "three four" },
    ]);
  });

  test("never runs outside the segment it came from", () => {
    const source = segment("one two three four five six seven", 2.5, 5.25);
    const pieces = splitSegment(source, 2);
    expect(pieces[0].start).toBe(source.start);
    expect(pieces[pieces.length - 1].end).toBe(source.end);
    for (const piece of pieces) {
      expect(piece.start).toBeGreaterThanOrEqual(source.start);
      expect(piece.end).toBeLessThanOrEqual(source.end);
      expect(piece.end).toBeGreaterThanOrEqual(piece.start);
    }
  });

  test("pieces run back to back with no gap", () => {
    const pieces = splitSegment(segment("one two three four five six", 0, 3), 2);
    for (let index = 1; index < pieces.length; index += 1) {
      expect(pieces[index].start).toBe(pieces[index - 1].end);
    }
  });

  test("a zero-length segment splits without producing NaN", () => {
    const pieces = splitSegment(segment("one two three four", 7, 7), 2);
    expect(pieces).toHaveLength(2);
    for (const piece of pieces) {
      expect(Number.isFinite(piece.start)).toBe(true);
      expect(Number.isFinite(piece.end)).toBe(true);
    }
  });

  test("collapses the whitespace whisper leaves around a segment", () => {
    const pieces = splitSegment(segment("  one   two    three  four ", 0, 4), 2);
    expect(pieces.map((piece) => piece.text)).toEqual(["one two", "three four"]);
  });

  test("a blank segment is passed through rather than vanishing", () => {
    // Whisper emits these around silence. Dropping them here would be a
    // second decision hiding inside a splitter.
    const blank = segment("   ");
    expect(splitSegment(blank, 3)).toEqual([blank]);
  });
});

describe("splitSegments", () => {
  test("keeps the transcript's order", () => {
    const pieces = splitSegments(
      [segment("one two three four", 0, 4), segment("five six", 4, 6)],
      2,
    );
    expect(pieces.map((piece) => piece.text)).toEqual(["one two", "three four", "five six"]);
  });

  test("returns the transcript untouched when there is no limit", () => {
    const transcript = [segment("one two three four five")];
    expect(splitSegments(transcript, NO_WORD_LIMIT)).toBe(transcript);
  });
});
