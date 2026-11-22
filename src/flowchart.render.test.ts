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

  it("renders every centre turn and every active branch card", () => {
    for (const line of [...FLOW.center, ...FLOW.branches.filter((b) => !b.deleted)]) {
      expect(html).toContain(`flow-label">${line.label.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/'/g, "&#x27;")}<`);
    }
    for (const line of FLOW.branches.filter((b) => b.deleted)) {
      expect(html).not.toContain(`flow-label">${line.label}<`);
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
    const branchA = html.indexOf("Branch A · Employee on site");
    const pathD = html.indexOf("Path D · Early reflex brush-off");
    const pathC = html.indexOf("Path C · Where did you get my number?");
    const regarding = html.indexOf("What is this regarding?");
    const auditReq = html.indexOf("Audit requirement");
    const lockMonday = html.indexOf("Lock in Monday");

    // Left column contains Branch A and Path D (ordered 3rd block on left)
    expect(left).toBeLessThan(branchA);
    expect(branchA).toBeLessThan(pathD);
    expect(pathD).toBeLessThan(right);

    // Right column contains Path C (top of right), Regarding (lower), Audit requirement, and Lock in Monday (last)
    expect(right).toBeLessThan(pathC);
    expect(pathC).toBeLessThan(regarding);
    expect(regarding).toBeLessThan(auditReq);
    expect(auditReq).toBeLessThan(lockMonday);
  });
});
