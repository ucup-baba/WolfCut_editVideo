/**
 * App-level preferences, in localStorage.
 *
 * These are machine preferences, not edit state: which Whisper model to use,
 * which language to expect. They deliberately do not live in the project -
 * copying a project to a machine without the model should not break it, and
 * the choice is about this computer's speed/quality tradeoff.
 */

const MODEL_KEY = "wolfcut.transcriber.model";
const LANGUAGE_KEY = "wolfcut.transcriber.language";

/** The default model: the speed/quality sweet spot, per the settings panel. */
export const DEFAULT_TRANSCRIBER_MODEL = "base.en";

export function getTranscriberModel(): string {
  return localStorage.getItem(MODEL_KEY) ?? DEFAULT_TRANSCRIBER_MODEL;
}

export function setTranscriberModel(id: string): void {
  localStorage.setItem(MODEL_KEY, id);
}

/** A Whisper language code, or "auto" to let the model decide. */
export function getTranscriberLanguage(): string {
  return localStorage.getItem(LANGUAGE_KEY) ?? "auto";
}

export function setTranscriberLanguage(code: string): void {
  localStorage.setItem(LANGUAGE_KEY, code);
}

const CAPTION_WORDS_KEY = "wolfcut.transcriber.captionWords";

/**
 * Most words a single caption may hold, or 0 for whisper's own segmentation.
 *
 * Zero by default: a transcript is what the tool returns, and cutting it
 * finer is an editorial choice this app should not make on anyone's behalf
 * without being asked. Short-form captions usually want four to six.
 */
export function getCaptionMaxWords(): number {
  const stored = Number(localStorage.getItem(CAPTION_WORDS_KEY));
  return Number.isFinite(stored) && stored > 0 ? Math.floor(stored) : 0;
}

export function setCaptionMaxWords(words: number): void {
  localStorage.setItem(CAPTION_WORDS_KEY, String(Math.max(0, Math.floor(words))));
}

const TTS_MODEL_KEY = "wolfcut.tts.model";
const TTS_VOICE_KEY = "wolfcut.tts.voice";

/** The default voice model: the compact build the settings panel recommends. */
export const DEFAULT_TTS_MODEL = "kokoro-int8-multi-lang-v1_0";

/** The default speaker: af_heart, Kokoro's showcase voice. */
export const DEFAULT_TTS_VOICE = 3;

export function getTtsModel(): string {
  return localStorage.getItem(TTS_MODEL_KEY) ?? DEFAULT_TTS_MODEL;
}

export function setTtsModel(id: string): void {
  localStorage.setItem(TTS_MODEL_KEY, id);
}

/** A Kokoro speaker id, from the host's voices table. */
export function getTtsVoice(): number {
  const raw = localStorage.getItem(TTS_VOICE_KEY);
  if (raw === null) return DEFAULT_TTS_VOICE;
  const stored = Number(raw);
  return Number.isInteger(stored) && stored >= 0 ? stored : DEFAULT_TTS_VOICE;
}

export function setTtsVoice(id: number): void {
  localStorage.setItem(TTS_VOICE_KEY, String(id));
}
