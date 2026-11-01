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

  // Labels mirror the script's own annotation vocabulary so the HUD reads the same way
  // the rep wrote the script.
  if (cue.tone === "down") chips.push("⬇️ DOWN");
  if (cue.tone === "up") chips.push("🤔 CURIOUS");

  if (cue.pace === "slow") chips.push("🐢 SLOW & LOW");
  if (cue.pace === "fast") chips.push("⚡ PACE UP");

  const hold = holdSeconds(cue);
  if (hold !== null) chips.push(`⏸️ ${hold}s`);

  return chips;
}

/** Seconds the rep should stay silent after this block, or null when no hold is needed. */
export function holdSeconds(cue: Cue): number | null {
  if (cue.pauseAfterMs < HOLD_THRESHOLD_MS) return null;
  return Math.round((cue.pauseAfterMs / 1000) * 10) / 10;
}
