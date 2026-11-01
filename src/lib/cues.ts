/**
 * Delivery cues are part of the script data model, not decoration: they encode the
 * tonality doctrine (downward inflection, slowed pace, deliberate silence) so the HUD
 * can coach delivery instead of only scrolling text.
 */
export type Tone = "down" | "up" | "neutral";
export type Pace = "slow" | "normal" | "fast";

export type Cue = {
  tone: Tone;
  pace: Pace;
  pauseAfterMs: number;
};

/** Minimum pause that is worth interrupting the rep's flow to display. */
export const HOLD_THRESHOLD_MS = 1200;

/** Human-readable chips for the active block. Order is stable for snapshot testing. */
export function cueChips(cue: Cue): string[] {
  const chips: string[] = [];

  if (cue.tone === "down") chips.push("⬇ DOWN");
  if (cue.tone === "up") chips.push("⬆ UP");

  if (cue.pace === "slow") chips.push("🐢 SLOW");
  if (cue.pace === "fast") chips.push("⚡ FAST");

  if (cue.pauseAfterMs >= HOLD_THRESHOLD_MS) chips.push("⏸ HOLD");

  return chips;
}

/** Seconds the rep should stay silent after this block, or null when no hold is needed. */
export function holdSeconds(cue: Cue): number | null {
  if (cue.pauseAfterMs < HOLD_THRESHOLD_MS) return null;
  return Math.round((cue.pauseAfterMs / 1000) * 10) / 10;
}
