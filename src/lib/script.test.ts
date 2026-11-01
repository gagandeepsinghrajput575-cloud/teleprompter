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
    expect(script.lines).toHaveLength(26);
    expect(script.lines.map((l) => l.id)).toEqual(
      Array.from({ length: 26 }, (_, i) => i + 1)
    );
  });

  it("keeps the four phases contiguous and in order", () => {
    const phases = script.lines.map((l) => l.phase);
    expect(new Set(phases).size).toBe(4);
    expect(phases[0]).toContain("PHASE 1");
    expect(phases[25]).toContain("PHASE 4");
    // A phase must never reappear after a later phase has started.
    const runs = phases.filter((p, i) => i === 0 || phases[i - 1] !== p);
    expect(new Set(runs).size).toBe(runs.length);
  });

  it("leaves no annotation markers in the spoken text", () => {
    for (const line of script.lines) {
      expect(line.body).not.toMatch(/\u2B07|\u23F8|\u{1F422}|\u26A1|\u{1F914}/u);
      expect(line.body).not.toContain("DOWN");
      expect(line.body).not.toContain("CURIOUS");
    }
  });

  it("keeps the [Name] placeholder in the spoken text", () => {
    expect(script.lines[0].body).toContain("[Name]");
  });

  it("derives cues per line, not per paragraph", () => {
    const cue = (id: number) => script.lines.find((l) => l.id === id)?.cue;
    expect(cue(1)).toEqual({ tone: "neutral", pace: "normal", pauseAfterMs: 0 });
    expect(cue(2)).toEqual({ tone: "down", pace: "normal", pauseAfterMs: 0 });
    expect(cue(6)).toEqual({ tone: "up", pace: "normal", pauseAfterMs: 1000 });
    expect(cue(14)).toEqual({ tone: "down", pace: "slow", pauseAfterMs: 0 });
    expect(cue(21)).toEqual({ tone: "up", pace: "normal", pauseAfterMs: 2000 });
    expect(cue(23)).toEqual({ tone: "neutral", pace: "fast", pauseAfterMs: 0 });
  });
});
