import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import ColdCallFlowchart from "./ColdCallFlowchart";
import { FLOW } from "./lib/coldcall";

/**
 * A render smoke test. The Tauri window cannot be screenshotted in CI, so this is the only
 * automated check that the three-column chart actually renders — and that the active card
 * carries the gold attribute the CSS keys off.
 */
describe("ColdCallFlowchart render", () => {
  const now = new Date(2026, 10, 13, 9, 0, 0);
  const html = renderToString(
    createElement(ColdCallFlowchart, { flow: FLOW, activeId: 4, onJump: () => undefined, now })
  );

  it("renders all three columns", () => {
    expect(html).toContain("flow-col--center");
    expect(html.match(/flow-col--side/g)?.length).toBe(2);
  });

  it("renders every centre turn and every branch card", () => {
    for (const line of [...FLOW.center, ...FLOW.branches]) {
      expect(html).toContain(`flow-label">${line.label.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/'/g, "&#x27;")}<`);
    }
  });

  it("marks exactly one card as the gold active line", () => {
    expect(html.match(/data-active="true"/g)?.length).toBe(1);
    expect(html).toMatch(/data-active="true"[^>]*>[\s\S]*?Turn 4/);
  });

  it("shows the computed appointment days inside the booking cards", () => {
    expect(html).toContain("Saturday");
    expect(html).toContain("Sunday");
  });

  it("puts the objection cards on the correct sides", () => {
    const left = html.indexOf("Left objections");
    const right = html.indexOf("Right objections");
    const pathB = html.indexOf("Path B · Don&#x27;t take cold calls");
    const pathD = html.indexOf("Path D · Early reflex brush-off");
    expect(left).toBeLessThan(pathB);
    expect(pathB).toBeLessThan(right);
    expect(right).toBeLessThan(pathD);
  });

  it("renders toolbar with New Block button and view mode controls", () => {
    expect(html).toContain("➕ New Block");
    expect(html).toContain("🔀 Canvas");
    expect(html).toContain("📋 Columns");
  });

  it("renders pencil edit buttons on blocks", () => {
    expect(html).toContain("✏️");
    expect(html).toContain("flow-btn-icon");
  });
});
