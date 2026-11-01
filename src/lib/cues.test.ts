import { describe, expect, it } from "vitest";
import { cueChips, holdSeconds, HOLD_THRESHOLD_MS, type Cue } from "./cues";

const base: Cue = { tone: "neutral", pace: "normal", pauseAfterMs: 0 };

describe("cueChips", () => {
  it("returns no chips for a neutral, continuous block", () => {
    expect(cueChips(base)).toEqual([]);
  });

  it("flags downward inflection", () => {
    expect(cueChips({ ...base, tone: "down" })).toEqual(["⬇ DOWN"]);
  });

  it("flags slowed pace", () => {
    expect(cueChips({ ...base, pace: "slow" })).toEqual(["🐢 SLOW"]);
  });

  it("flags a deliberate hold at or above the threshold", () => {
    expect(cueChips({ ...base, pauseAfterMs: HOLD_THRESHOLD_MS })).toEqual(["⏸ HOLD"]);
  });

  it("does not flag a hold below the threshold", () => {
    expect(cueChips({ ...base, pauseAfterMs: HOLD_THRESHOLD_MS - 1 })).toEqual([]);
  });

  it("keeps a stable order when every cue is present", () => {
    expect(cueChips({ tone: "down", pace: "slow", pauseAfterMs: 2000 })).toEqual([
      "⬇ DOWN",
      "🐢 SLOW",
      "⏸ HOLD",
    ]);
  });
});

describe("holdSeconds", () => {
  it("returns null when the block needs no hold", () => {
    expect(holdSeconds({ ...base, pauseAfterMs: 400 })).toBeNull();
  });

  it("rounds to one decimal place", () => {
    expect(holdSeconds({ ...base, pauseAfterMs: 1850 })).toBe(1.9);
  });
});
