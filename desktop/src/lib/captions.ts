/**
 * Cutting a transcript into caption-sized pieces.
 *
 * Whisper segments by sentence, which is the right unit for a transcript and
 * the wrong one for a caption: a sentence fills a phone screen and is gone
 * before it is read. Short-form captions run a few words at a time, so the
 * segments get split before they become clips.
 *
 * Timings are shared out by word count, because that is all there is to go
 * on - `whisper-cli` gives one span per segment, not per word. So a chunk's
 * span is an estimate, honest to the segment's own bounds but not to where
 * each word actually falls. Good enough to read along with; not good enough
 * to cut picture against.
 */
import type { TranscribedSegment } from "./engine";

/** Off: keep whatever whisper decided a segment was. */
export const NO_WORD_LIMIT = 0;

/**
 * Splits one segment so no piece runs longer than `maxWords`.
 *
 * The pieces come out even rather than greedy. Nine words at a limit of four
 * is three-and-three-and-three, not four-and-four-and-one: a one-word orphan
 * flashing on screen reads worse than three balanced lines, and it costs
 * nothing to avoid.
 */
export function splitSegment(
  segment: TranscribedSegment,
  maxWords: number,
): TranscribedSegment[] {
  const words = segment.text.trim().split(/\s+/).filter(Boolean);
  if (maxWords < 1 || words.length <= maxWords) {
    return [segment];
  }

  const pieces = Math.ceil(words.length / maxWords);
  const base = Math.floor(words.length / pieces);
  // The first few pieces take the remainder, one word each, so the longest
  // and shortest piece are never more than a word apart.
  const longer = words.length % pieces;
  const span = segment.end - segment.start;

  const out: TranscribedSegment[] = [];
  let taken = 0;
  for (let piece = 0; piece < pieces; piece += 1) {
    const size = base + (piece < longer ? 1 : 0);
    const from = taken;
    taken += size;
    out.push({
      start: segment.start + (span * from) / words.length,
      end: segment.start + (span * taken) / words.length,
      text: words.slice(from, taken).join(" "),
    });
  }
  return out;
}

/** [`splitSegment`] over a whole transcript, order preserved. */
export function splitSegments(
  segments: TranscribedSegment[],
  maxWords: number,
): TranscribedSegment[] {
  if (maxWords < 1) return segments;
  return segments.flatMap((segment) => splitSegment(segment, maxWords));
}
