import { describe, expect, it } from "vitest";
import fixture from "../fixtures/remodeler_script.json";
import { classifyToken, loadScript, parseAnnotations } from "./script";

describe("classifyToken", () => {
  it("recognises a downward inflection marker", () => {
    expect(classifyToken("\u2B07\uFE0F DOWN")).toEqual([{ kind: "tone", value: "down" }]);
  });

  it("maps CURIOUS to an upward inflection", () => {
    expect(classifyToken("\u{1F914} CURIOUS")).toEqual([{ kind: "tone", value: "up" }]);
  });

  it("maps SLOW & LOW to slow pace plus downward tone", () => {
    expect(classifyToken("\u{1F422} SLOW & LOW")).toEqual([
      { kind: "pace", value: "slow" },
      { kind: "tone", value: "down" },
    ]);
  });

  it("maps PACE UP to fast pace and NOT upward tone", () => {
    expect(classifyToken("\u26A1 PACE UP")).toEqual([{ kind: "pace", value: "fast" }]);
  });

  it("parses fractional second pauses", () => {
    expect(classifyToken("\u23F8\uFE0F 1.5s")).toEqual([{ kind: "pause", ms: 1500 }]);
  });

  it("treats the [Name] placeholder as ordinary text", () => {
    expect(classifyToken("Name")).toBeNull();
  });
});

describe("parseAnnotations", () => {
  it("keeps placeholders in the spoken body", () => {
    const { body } = parseAnnotations("Hey [Name], good to meet you. [\u2B07\uFE0F DOWN]");
    expect(body).toBe("Hey [Name], good to meet you.");
  });

  it("lets the last delivery instruction win", () => {
    const { cue } = parseAnnotations(
      "[\u{1F422} SLOW & LOW] Diagnosis. [\u23F8\uFE0F 1.5s] Feel right? [\u{1F914} CURIOUS]"
    );
    expect(cue).toEqual({ tone: "up", pace: "slow", pauseAfterMs: 1500 });
  });

  it("takes the longest pause when a block asks for two", () => {
    const { cue } = parseAnnotations("A. [\u23F8\uFE0F 1.5s] B. [\u23F8\uFE0F 2s] C.");
    expect(cue.pauseAfterMs).toBe(2000);
  });

  it("collapses the whitespace left behind by stripped markers", () => {
    const { body } = parseAnnotations("One. [\u2B07\uFE0F DOWN] Two.");
    expect(body).toBe("One. Two.");
  });
});

describe("remodeler fixture", () => {
  const script = loadScript(fixture as never);

  it("loads all eight blocks", () => {
    expect(script.blocks).toHaveLength(8);
    expect(script.blocks.map((b) => b.id)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
  });

  it("derives a body for every block that matches the stored spoken text exactly", () => {
    for (const block of script.blocks) {
      const stored = (fixture.blocks as { id: number; body: string }[]).find(
        (b) => b.id === block.id
      );
      expect(block.body, `block ${block.id} body drifted from the raw text`).toBe(stored?.body);
    }
  });

  it("leaves no annotation markers in the spoken text", () => {
    for (const block of script.blocks) {
      expect(block.body).not.toMatch(/\u2B07|\u23F8|\u{1F422}|\u26A1|\u{1F914}/u);
      expect(block.body).not.toContain("DOWN");
      expect(block.body).not.toContain("CURIOUS");
    }
  });

  it("derives the expected delivery cue for every block", () => {
    expect(script.blocks.map((b) => b.cue)).toEqual([
      { tone: "up", pace: "normal", pauseAfterMs: 1000 },
      { tone: "up", pace: "normal", pauseAfterMs: 0 },
      { tone: "up", pace: "normal", pauseAfterMs: 1000 },
      { tone: "up", pace: "normal", pauseAfterMs: 1000 },
      { tone: "up", pace: "normal", pauseAfterMs: 0 },
      { tone: "up", pace: "slow", pauseAfterMs: 2000 },
      { tone: "up", pace: "normal", pauseAfterMs: 0 },
      { tone: "down", pace: "fast", pauseAfterMs: 0 },
    ]);
  });

  it("gives every block a summary for the semantic tracker to match against", () => {
    for (const block of script.blocks) {
      expect(block.summary.length, `block ${block.id} has no summary`).toBeGreaterThan(40);
    }
  });
});
