import { describe, expect, it } from "vitest";
import call1Fixture from "../fixtures/call1_audit_script.json";
import call2Fixture from "../fixtures/call2_sales_script.json";
import coldcallFixture from "../fixtures/coldcall_scalix.json";
import { cueChips, holdSeconds } from "./cues";
import { loadScript, parseAnnotations } from "./script";

describe("Script switching regression tests", () => {
  it("loads Call 1 audit script with correct line cues and structure", () => {
    const script = loadScript(call1Fixture as never);
    expect(script.lines.length).toBeGreaterThan(0);
    // Line 1 cue check
    const line1 = script.lines[0];
    expect(line1.raw).toContain("[⬇️]");
    expect(line1.cue.tone).toBe("down");
    expect(line1.cue.pauseAfterMs).toBeGreaterThanOrEqual(1000);
    // Spoken body has emojis stripped
    expect(line1.body).not.toContain("⬇️");
    expect(line1.body).not.toContain("⏸️");
    expect(line1.body).toContain("[Name]");
  });

  it("loads Call 2 sales closing script with correct line cues and structure", () => {
    const script = loadScript(call2Fixture as never);
    expect(script.lines.length).toBeGreaterThan(0);
    // Verify slow & low and rapid delivery annotations
    const slowLine = script.lines.find((l) => l.raw.includes("[🐢 SLOW & LOW"));
    expect(slowLine).toBeDefined();
    if (slowLine) {
      expect(slowLine.cue.pace).toBe("slow");
      expect(slowLine.cue.tone).toBe("down");
    }
  });

  it("loads Cold Call flowchart schema with center path and branches", () => {
    const script = loadScript(coldcallFixture as never);
    expect(script.lines.length).toBeGreaterThanOrEqual(10);
    // Check that both center turns and branch objections are loaded into lines
    const branchLine = script.lines.find((l) => l.id === 101);
    expect(branchLine).toBeDefined();
    expect(branchLine?.phase).toBe("Left Objection");
  });

  it("switches scripts cleanly with unique activeId boundaries", () => {
    const call1 = loadScript(call1Fixture as never);
    const call2 = loadScript(call2Fixture as never);
    const coldcall = loadScript(coldcallFixture as never);

    expect(call1.lines[0].id).toBe(1);
    expect(call2.lines[0].id).toBe(1);
    expect(coldcall.lines[0].id).toBe(1);

    // Ensure all script lines have valid non-empty raw text and valid parsed cues
    for (const s of [call1, call2, coldcall]) {
      for (const line of s.lines) {
        expect(line.raw.length).toBeGreaterThan(0);
        expect(line.cue).toBeDefined();
        expect(["down", "up", "neutral"]).toContain(line.cue.tone);
        expect(["slow", "fast", "normal"]).toContain(line.cue.pace);
        expect(line.cue.pauseAfterMs).toBeGreaterThanOrEqual(0);
        // Spoken body is either text or an intentional pause block (e.g. line 23 'Power pause - zip lips')
        if (line.body.length === 0) {
          expect(line.cue.pauseAfterMs).toBeGreaterThan(0);
        }
      }
    }
  });
});

describe("Comprehensive Line Cue Parsing", () => {
  it("parses single down tone [⬇️]", () => {
    const { cue, annotations } = parseAnnotations("State your reason cleanly [⬇️].");
    expect(cue.tone).toBe("down");
    expect(annotations.some((a) => a.kind === "tone" && a.value === "down")).toBe(true);
  });

  it("parses curious inflection [🤔]", () => {
    const { cue } = parseAnnotations("Does tomorrow at 15:30 work for you [🤔]?");
    expect(cue.tone).toBe("up");
  });

  it("parses slow turtle pace [🐢] as slow pace and downward tone", () => {
    const { cue } = parseAnnotations("[🐢] Let's take a closer look at the workflow.");
    expect(cue.pace).toBe("slow");
    expect(cue.tone).toBe("down");
  });

  it("parses lightning pace [⚡] as fast pace without altering tone", () => {
    const { cue } = parseAnnotations("[⚡] I'm reaching out because we work with top firms.");
    expect(cue.pace).toBe("fast");
  });

  it("parses pause marker [⏸️] and custom duration [⏸️ 2.5s]", () => {
    const defaultPause = parseAnnotations("Take notes. [⏸️]");
    expect(defaultPause.cue.pauseAfterMs).toBe(3000);

    const customPause = parseAnnotations("Hold on. [⏸️ 2.5s]");
    expect(customPause.cue.pauseAfterMs).toBe(2500);
  });

  it("evaluates holdSeconds and chips correctly", () => {
    const { cue } = parseAnnotations("[🐢] Here is the diagnosis. [⏸️ 1.5s]");
    expect(holdSeconds(cue)).toBe(1.5);
    const chips = cueChips(cue);
    expect(chips).toContain("🐢");
    expect(chips).toContain("⏸️");
    expect(chips).toContain("⬇️");
  });

  it("handles loose cues without brackets outside text", () => {
    const { body } = parseAnnotations("🐢 Good ⬇️. ⏸️ ⚡ I'm reaching out ⬇️.");
    expect(body).not.toMatch(/[\u2B07\u{1F914}\u{1F422}\u26A1\u23F8]/u);
  });
});
