import { describe, expect, it } from "vitest";
import {
  compileScriptDeterministically,
  sanitizeRawContent,
} from "./script_compiler";

describe("script_compiler", () => {
  it("sanitizes HTML tags and tables while preserving text", () => {
    const html = `
      <div style="color: red;">
        <h1>Call 2: Remodeler Pitch</h1>
        <table>
          <tr><td>Step 1</td><td>We help remodelers scale.</td></tr>
        </table>
        <p>Does that make sense?&nbsp;&amp;&nbsp;sound fair?</p>
      </div>
    `;
    const clean = sanitizeRawContent(html);
    expect(clean).not.toContain("<div");
    expect(clean).not.toContain("<table");
    expect(clean).not.toContain("&nbsp;");
    expect(clean).toContain("We help remodelers scale.");
    expect(clean).toContain("sound fair?");
  });

  it("enforces strict two-track separation: keeps objections out of center lines", () => {
    const raw = `
# Phase 1: Set the Frame
Hey John, appreciate you carving out the time.
Just to set the stage, I'm not here to pitch you today.

If they say "we are too busy":
Tell them that being busy is exactly why we automate the intake.

## Phase 2: The Solution
Here is how our prototype routes calls in under 60 seconds.

Objection: Too expensive / tight cash flow
We offer two split tranches so the first jobs pay for the setup.
    `;

    const { script, objections } = compileScriptDeterministically(raw, "Test Pitch");

    // Center spine must only contain the presentation dialogue
    expect(script.lines.length).toBe(3);
    expect(script.lines[0].body).toContain("appreciate you carving out the time");
    expect(script.lines[1].body).toContain("not here to pitch you today");
    expect(script.lines[2].body).toContain("prototype routes calls");

    // Neither objection should be in the center spine
    for (const line of script.lines) {
      expect(line.body).not.toContain("being busy is exactly why");
      expect(line.body).not.toContain("We offer two split tranches");
    }

    // Objections must be in the objection deck
    const busyObj = objections.find((o) => o.index === 4 || o.label.toLowerCase().includes("busy"));
    expect(busyObj).toBeDefined();
    const priceObj = objections.find((o) => o.index === 1);
    expect(priceObj?.rebuttal).toContain("two split tranches");
  });

  it("auto-splits compound sentences with rhythm pauses", () => {
    const longSentence = "We have spent the last three years building custom telephony integrations for residential general contractors across Denmark and Scandinavia.";
    const { script } = compileScriptDeterministically(longSentence);
    expect(script.lines[0].raw).toContain("⏸");
  });

  it("guarantees 10 invariant objection slots (1-9, 0)", () => {
    const { objections } = compileScriptDeterministically("Short script");
    expect(objections.length).toBe(10);
    const indices = objections.map((o) => o.index).sort();
    expect(indices).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
  });
});
