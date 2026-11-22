import { describe, expect, it } from "vitest";
import {
  appointmentDays,
  ColdCallTracker,
  FLOW,
  TIER0_MIN_WORDS,
  TIER1_MIN_WORDS,
  normalizeWords,
  type Tier,
} from "./coldcall";

/** Every assertion starts from a clean tracker, so tests cannot leak state into each other. */
function tracker(start = 1) {
  return new ColdCallTracker(FLOW, start);
}

describe("the fixture is the verbatim Scalix flowchart", () => {
  it("has the centre turns, in order", () => {
    expect(FLOW.center.map((line) => line.turn)).toEqual([
      "Turn 1", "Turn 3", "Turn 4", "Turn 5", "Turn 6", "Turn 7",
    ]);
    expect(FLOW.center.map((line) => line.label)).toEqual([
      "Opening",
      "The Hook & Assumption",
      "The Leak & The Offer",
      "Lock the time",
      "Get the email",
      "Spell back and sign off",
    ]);
  });

  it("has every branch the call sheet requires", () => {
    const labels = FLOW.branches.map((branch) => branch.label);
    for (const required of [
      "On-Site / Tools Running",
      "Branch A · Employee on site",
      "What is this regarding?",
      "Path B · Don't take cold calls",
      "Path D · Early reflex brush-off",
      "Path C · Where did you get my number?",
      "Send me an email first",
      "Already have a web / tech guy",
      "We don't use AI",
      "Why is it free? / What's the catch?",
      "Booked out / plenty of work",
    ]) {
      expect(labels, `missing branch: ${required}`).toContain(required);
    }
  });

  it("keeps the copy verbatim, including numbers and wording", () => {
    const turn = (id: number) => FLOW.center.find((line) => line.id === id)!.raw;
    const branch = (label: string) => FLOW.branches.find((b) => b.label === label)!.raw;

    expect(turn(1)).toContain("[Prospect Name], hi. It's Gagan from Scalix Elite");
    expect(turn(4)).toContain("40,000 to 80,000 DKK");
    expect(turn(6)).toContain("[time] it is");
    expect(turn(7)).toContain("[spell back email address slowly]");
    expect(branch("Path B · Don't take cold calls")).toContain(
      "Neither do I, [Prospect Name], if I can possibly avoid it"
    );
    expect(branch("Why is it free? / What's the catch?")).toContain(
      "80% of established firms"
    );
  });

  it("puts objections on both sides of the centre column", () => {
    const t = tracker();
    expect(t.side("left").length).toBeGreaterThan(0);
    expect(t.side("right").length).toBeGreaterThan(0);
    expect(t.side("left").length + t.side("right").length).toBe(FLOW.branches.length);
  });
});

describe("Tier 0 - rep-first objection openers", () => {
  // The exact openers from the requirement, each mapping to the card it must surface.
  const openers: [string, number][] = [
    ["Neither do I", 104],
    ["I'm not surprised", 105],
    ["Our research team", 106],
    ["For sure I can email", 107],
    ["Good to hear", 108],
    ["Well using AI", 110],
    ["The issue when you're slammed", 112],
    ["Because we know 80", 111],
    ["Understood I won't waste", 100],
  ];

  for (const [opener, expected] of openers) {
    it(`"${opener}" lands on card ${expected}`, () => {
      const t = tracker(7);
      expect(t.observe(opener)).toBe(expected);
    });
  }

  it("fires on the first two words, not the whole phrase", () => {
    // "neither do i" is three words; two is the floor. Nothing else should be needed.
    expect(TIER0_MIN_WORDS).toBe(2);
    const t = tracker(7);
    expect(t.observe("neither do")).toBe(104);
  });

  it("reports itself as tier 0", () => {
    const t = tracker(7);
    const decision = t.decide("i'm not surprised at all");
    expect(decision?.tier).toBe<Tier>(0);
    expect(decision?.id).toBe(105);
  });

  it("never loses to prospect audio in the same breath", () => {
    // The prospect pushes back, then the rep answers with an objection opener. The rep wins.
    const t = tracker(7);
    const decision = t.decide(
      "just send me an email with some info, our research team pulls numbers from public registries"
    );
    expect(decision?.tier).toBe<Tier>(0);
    expect(decision?.id).toBe(106);
  });
});

describe("Tier 1 - three words of any line start", () => {
  it(`opens every centre turn from its first ${TIER1_MIN_WORDS} words`, () => {
    for (const line of FLOW.center) {
      const start = normalizeWords(line.raw).slice(0, TIER1_MIN_WORDS);
      const t = tracker(line.id === 7 ? 1 : 7);
      expect(t.observe(start.join(" ")), `turn ${line.turn}: "${start.join(" ")}"`).toBe(line.id);
    }
  });

  it("opens a branch from its first three words", () => {
    const t = tracker(7);
    expect(t.observe("alright i'm trying")).toBe(101);
  });

  it("ignores two words when three are required for a line", () => {
    const t = tracker(7);
    expect(t.observe("got it")).toBeNull();
  });

  it("does not hijack the highlight from prospect-only speech", () => {
    const t = tracker(1);
    expect(t.observe("look i don't really take cold calls")).toBeNull();
  });

  it("surfaces a branch from the prospect's own wording (Tier 2)", () => {
    const t = tracker(1);
    const decision = t.decide("just send me an email");
    expect(decision?.tier).toBe<Tier>(2);
    expect(decision?.id).toBe(107);
  });
});

describe("sequence: after a paragraph, the next turn", () => {
  it("walks Turn 4 to 5 to 6 to 7 on their opening words", () => {
    const t = tracker(3);
    expect(t.observe("got it, yeah, so the reason for the call")).toBe(4);
    expect(t.observe("before committing to anything")).toBe(5);
    expect(t.observe("perfect it is whats your best")).toBe(6);
    expect(t.observe("got it, that's right")).toBe(7);
  });

  it("knows the next turn before it is spoken", () => {
    const t = tracker(4);
    expect(t.nextExpectedId()).toBe(5);
    expect(t.advance()).toBe(5);
    expect(t.nextExpectedId()).toBe(6);
  });

  it("returns an objection to the turn the flow resumes at", () => {
    const t = tracker(4);
    expect(t.observe("neither do i if i can possibly avoid it")).toBe(104);
    expect(t.nextExpectedId()).toBe(4);
    expect(t.advance()).toBe(4);
  });

  it("chains the web-guy objection into its follow-up", () => {
    const t = tracker(4);
    expect(t.observe("good to hear")).toBe(108);
    expect(t.nextExpectedId()).toBe(109);
  });

  it("ends the script at the last turn", () => {
    const t = tracker(7);
    expect(t.nextExpectedId()).toBeNull();
    expect(t.advance()).toBe(7);
  });
});

describe("manual taps", () => {
  it("jumps anywhere and records the trail", () => {
    const t = tracker();
    t.jump(111);
    expect(t.activeId).toBe(111);
    t.jump(3);
    expect(t.activeId).toBe(3);
    expect(t.trail).toEqual([1, 111, 3]);
  });

  it("ignores an unknown id rather than blanking the highlight", () => {
    const t = tracker(4);
    expect(t.jump(9999)).toBe(4);
  });

  it("is not undone by the next stray word", () => {
    const t = tracker(1);
    t.jump(110);
    expect(t.observe("umm")).toBeNull();
    expect(t.activeId).toBe(110);
  });
});

describe("appointment days", () => {
  it("computes tomorrow and the day after from the current date", () => {
    // Friday 13 Nov 2026 -> Saturday 14th, Sunday 15th.
    const days = appointmentDays(new Date(2026, 10, 13, 9, 0, 0));
    expect(days.tomorrow.weekday).toBe("Saturday");
    expect(days.tomorrow.short).toBe("Sat 14 Nov");
    expect(days.tomorrow.iso).toBe("2026-11-14");
    expect(days.dayAfter.weekday).toBe("Sunday");
    expect(days.dayAfter.short).toBe("Sun 15 Nov");
    expect(days.dayAfter.iso).toBe("2026-11-15");
  });

  it("crosses a month boundary correctly", () => {
    const days = appointmentDays(new Date(2026, 10, 30, 9, 0, 0));
    expect(days.tomorrow.iso).toBe("2026-12-01");
    expect(days.dayAfter.iso).toBe("2026-12-02");
  });

  it("never mutates the date it was given", () => {
    const now = new Date(2026, 10, 13);
    appointmentDays(now);
    expect(now.getDate()).toBe(13);
  });
});

/**
 * Regression: an objection opener used to sit in the match window for a dozen words and keep
 * re-selecting its own card. The rep would answer the objection, start Turn 4, and the HUD would
 * stay pinned to the objection because Tier 0 is checked first and never expired.
 */
describe("stale matches expire", () => {
  it("lets a newer line outrank an older objection", () => {
    const t = tracker(4);
    expect(t.observe("neither do i")).toBe(104);
    // The rep has answered the objection and moved on.
    expect(t.observe("got it yeah so the reason for the call")).toBe(4);
  });

  it("lets a newer turn outrank an older objection", () => {
    const t = tracker(4);
    expect(t.observe("our research team")).toBe(106);
    expect(t.observe("awesome im assuming youre")).toBe(5);
  });

  it("does not re-decide an objection that is already behind us", () => {
    const t = tracker(104);
    const decision = t.decide("got it yeah so the reason for the call");
    expect(decision?.id).toBe(4);
    expect(decision?.tier).toBe<Tier>(1);
  });

  it("still fires an objection the moment it is spoken", () => {
    // Zero latency is the point of Tier 0: no need to wait for a final transcript.
    const t = tracker(4);
    expect(t.observe("neither do", false)).toBe(104);
  });
});

/**
 * Deepgram rewrites interim transcripts. Appending each rewrite duplicated every phrase into the
 * window, which made old anchors look fresh and could break contiguity outright.
 */
describe("interim transcripts replace, never accumulate", () => {
  it("keeps one copy of a growing interim", () => {
    const t = tracker(4);
    t.decide("neither do", false);
    t.decide("neither do i", false);
    expect([...t.heard]).toEqual(["neither", "do", "i"]);
  });

  it("drops the interim tail when the final supersedes it", () => {
    const t = tracker(4);
    t.decide("neither do i", false);
    t.decide("neither do i", true);
    expect([...t.heard]).toEqual(["neither", "do", "i"]);
  });

  it("still matches against the in-progress sentence", () => {
    const t = tracker(4);
    const decision = t.decide("neither do", false);
    expect(decision?.id).toBe(104);
  });
});

describe("retreat", () => {
  it("steps back one centre turn at a time", () => {
    const t = tracker(5);
    expect(t.retreat()).toBe(4);
    expect(t.retreat()).toBe(3);
  });

  it("stops at the first turn", () => {
    const t = tracker(1);
    expect(t.retreat()).toBe(1);
  });
});

describe("utterances arbitrate between tiers", () => {
  it("still fires after the rep keeps talking in the same utterance", () => {
    // A single final can run on for a dozen words after the opener. Distance from the end of
    // the utterance must not expire it - only a *newer* utterance can.
    const t = tracker(7);
    const decision = t.decide(
      "our research team pulls numbers from public registries and county filings"
    );
    expect(decision?.tier as Tier).toBe(0);
    expect(decision?.id).toBe(106);
  });

  it("keeps the objection card when the prospect answers it", () => {
    const t = tracker(7);
    expect(t.observe("neither do i")).toBe(104);
    // Branch A's prospect trigger would otherwise claim the card the rep just opened.
    expect(t.decide("employee on site, I'm one of his guys")?.id).toBe(104);
  });

  it("releases the objection card when the rep speaks a line again", () => {
    const t = tracker(7);
    expect(t.observe("neither do i")).toBe(104);
    expect(t.observe("before committing to anything")).toBe(5);
  });

  it("lets a newer line beat an opener from the previous utterance", () => {
    // The rep answered an objection, then moved on: the HUD must follow the new line.
    const t = tracker(7);
    expect(t.observe("neither do i")).toBe(104);
    expect(t.observe("awesome im assuming youre")).toBe(5);
  });
});
