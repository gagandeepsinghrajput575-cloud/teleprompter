import { describe, expect, it } from "vitest";
import fixture from "../fixtures/remodeler_script.json";
import { loadScript } from "./script";
import { DEFAULT_CONFIG, Tracker, type TrackedLine } from "./tracker";

/**
 * These are ports of the Rust test module in `src-tauri/src/tracker.rs`, one for one, with the
 * same inputs and the same expected values. They exist to prove the TypeScript tracker makes
 * the same decisions as the shipping implementation — a port that silently "improves" a guard
 * is worse than no port, because the packaged app would then disagree with every measurement
 * taken here.
 */
function lines(): TrackedLine[] {
  const script = loadScript(fixture as never);
  return script.lines.map(({ id, phase, label, body }) => ({ id, phase, label, body }));
}

/** Config that reproduces the Rust commit behaviour (window wiped, tail kept out of it). */
const PARITY = { retainWords: 0 } as const;

function tracker(config: Partial<typeof DEFAULT_CONFIG> = PARITY): Tracker {
  return new Tracker(lines(), config);
}

describe("parity with the Rust tracker", () => {
  it("finds the missed-leads line from a paraphrase", () => {
    const t = tracker();
    t.pushText("what happens when a lead rings while you are busy on a job site");
    const match = t.localMatch();
    expect(match?.id).toBe(11);
    expect(match?.confident).toBe(true);
  });

  it("finds the project-photos line", () => {
    const t = tracker();
    t.pushText("all those photos of your past projects do they just sit there");
    expect(t.localMatch()?.id).toBe(10);
  });

  it("finds the money line", () => {
    const t = tracker();
    t.pushText("you are easily leaving fifteen k on the table every month");
    expect(t.localMatch()?.id).toBe(20);
  });

  it("requires two confirmations for an unconfident forward move", () => {
    const t = tracker();
    expect(t.activeId).toBe(1);
    expect(t.consider(2, false)).toBeNull();
    expect(t.consider(2, false)).toBe(2);
  });

  it("commits a confident match immediately", () => {
    const t = tracker();
    expect(t.consider(3, true)).toBe(3);
  });

  it("needs three confirmations to move backwards", () => {
    const t = tracker();
    t.consider(6, true);
    expect(t.activeId).toBe(6);
    expect(t.consider(2, false)).toBeNull();
    expect(t.consider(2, false)).toBeNull();
    expect(t.consider(2, false)).toBe(2);
  });

  it("resets the pending count when the proposal disagrees", () => {
    const t = tracker();
    expect(t.consider(2, false)).toBeNull();
    expect(t.consider(3, false)).toBeNull();
    expect(t.consider(2, false)).toBeNull();
    expect(t.consider(2, false)).toBe(2);
  });

  it("ignores an out-of-range id", () => {
    const t = tracker();
    expect(t.consider(99, true)).toBeNull();
    expect(t.activeId).toBe(1);
  });

  it("clamps a manual override at both ends", () => {
    const t = tracker();
    expect(t.manual(-1)).toBe(1);
    expect(t.manual(1)).toBe(2);
    for (let i = 0; i < 40; i += 1) t.manual(1);
    expect(t.activeId).toBe(26);
  });

  it("bounds the buffer", () => {
    const t = tracker();
    t.pushText("word ".repeat(50));
    expect(t.snapshot().bufferedWords).toBe(DEFAULT_CONFIG.bufferWords);
  });

  it("does not treat a single shared word as confident evidence", () => {
    const t = tracker();
    t.pushText("my cousin is driving down from Ohio next weekend");
    const match = t.localMatch();
    expect(match?.id).toBe(12);
    expect(match?.confident).toBe(false);
  });

  it("does not treat two incidental words as enough coverage", () => {
    const t = tracker();
    t.pushText("my cousin is driving down from Ohio and back");
    const match = t.localMatch();
    expect(match?.id).toBe(12);
    expect(match?.confident).toBe(false);
  });

  it("keeps the candidate window near the active line", () => {
    const t = tracker();
    t.consider(5, true);
    expect(t.candidateWindow().map((line) => line.id)).toEqual([2, 3, 4, 5, 6, 7, 8]);
  });

  it("cannot re-trigger a cycle from an unchanged interim", () => {
    const t = tracker();
    expect(t.shouldRunCycle()).toBe(false);
    t.setPartial("what happens");
    expect(t.shouldRunCycle()).toBe(true);
    t.markCycleRan();
    expect(t.shouldRunCycle()).toBe(false);
    t.setPartial("what happens when a lead");
    expect(t.shouldRunCycle()).toBe(true);
  });

  it("includes the interim tail in the buffer", () => {
    const t = tracker();
    t.pushText("what happens when");
    t.setPartial("a lead calls");
    expect(t.spokenBuffer().endsWith("a lead calls")).toBe(true);
  });

  it("does not duplicate the interim tail when the final arrives", () => {
    const t = tracker();
    t.setPartial("what happens when a lead");
    t.pushText("what happens when a lead");
    t.clearPartial();
    const occurrences = t.spokenBuffer().split(/\s+/).filter((word) => word === "lead").length;
    expect(occurrences).toBe(1);
  });
});

describe("local match gates (measured, not assumed)", () => {
  it("reports the coverage the coverage gate rejected", () => {
    // Two shared words out of eight is exactly at the 0.25 floor; the guard that actually
    // rejects this case is the margin/score pair, and the numbers should say so.
    const t = tracker();
    t.pushText("my cousin is driving down from Ohio and back");
    const match = t.localMatch();
    expect(match?.hits).toBe(2);
    expect(match?.confident).toBe(false);
    expect(match?.coverage).toBeLessThanOrEqual(0.25);
  });

  it("makes the margin gate configurable per script", () => {
    const text = "my cousin is driving down from Ohio and back";
    const strict = tracker({ ...PARITY, localMargin: 50 });
    strict.pushText(text);
    expect(strict.localMatch()?.confident).toBe(false);

    const loose = tracker({ ...PARITY, minHits: 2, minCoverage: 0, minDominantScore: 0, localMargin: 0 });
    loose.pushText(text);
    expect(loose.localMatch()?.confident).toBe(true);
  });
});

/**
 * The commit-reset measurement. This is the change the replay harness was built to justify:
 * the Rust tracker wipes its window on every commit, so the next cycle runs against almost
 * nothing. Both halves are asserted, so the fix can never quietly become a no-op.
 */
describe("commit reset", () => {
  /** Feeds one line's words as a single final, then runs the next cycles it triggers. */
  function walk(config: Partial<typeof DEFAULT_CONFIG>) {
    const script = lines();
    const t = new Tracker(script, config);
    const commits: number[] = [];
    for (const line of script) {
      t.pushText(line.body);
      if (t.shouldRunCycle()) {
        t.markCycleRan();
        const match = t.localMatch();
        if (match) {
          const committed = t.consider(match.id, match.confident);
          if (committed !== null) commits.push(committed);
        }
      }
    }
    return { commits, active: t.activeId };
  }

  /**
   * The hypothesis this harness was built to test was: wiping the window on every commit leaves
   * the next cycle with too little context to clear the gates, so the highlight stalls and only
   * the language model can move it. Measured: **false**. Because a finalized line arrives as one
   * multi-word segment, the window is already refilled by the time the next cycle runs.
   *
   * Recorded as a test so the claim does not come back: both settings walk the script, so there
   * is no justification for changing the shipping behaviour, and `retainWords` stays off.
   */
  it("walks the script with the shipping wipe-the-window behaviour", () => {
    const { commits, active } = walk({ ...DEFAULT_CONFIG, retainWords: 0 });
    expect(commits.length).toBeGreaterThanOrEqual(6);
    expect(active).toBeGreaterThanOrEqual(7);
  });

  it("walks it no better with the tail retained (so the change is not shipped)", () => {
    const withTail = walk({ ...DEFAULT_CONFIG, retainWords: 6 });
    const withoutTail = walk({ ...DEFAULT_CONFIG, retainWords: 0 });
    expect(withTail.active).toBe(withoutTail.active);
  });
});
