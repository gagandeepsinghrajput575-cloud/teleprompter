if (typeof localStorage === "undefined" || !globalThis.localStorage) {
  const store = new Map<string, string>();
  const mockStorage = {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, String(value)),
    removeItem: (key: string) => store.delete(key),
    clear: () => store.clear(),
    key: (index: number) => Array.from(store.keys())[index] ?? null,
    get length() {
      return store.size;
    },
  };
  Object.defineProperty(globalThis, "localStorage", {
    value: mockStorage,
    writable: true,
    configurable: true,
  });
}

import { beforeEach, describe, expect, it } from "vitest";
import { parseCustomScript, getSavedScripts, saveCustomScript, deleteSavedScript } from "./customScript";

describe("customScript parser", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("parses markdown script correctly with headings and cue tags", () => {
    const md = `
# Enterprise SaaS Pitch

## Introduction
Greeting: Hey [Name], thanks for jumping on. [🐢 SLOW & LOW]
Hook: We help commercial HVAC teams automate dispatch. [⚡ PACE UP]

## Pricing
Proposal: Standard package is $4,500/mo. [⏸️ 2.0s]
`;

    const script = parseCustomScript("enterprise.md", md);
    expect(script.title).toBe("Enterprise SaaS Pitch");
    expect(script.lines.length).toBe(3);
    
    expect(script.lines[0].phase).toBe("Introduction");
    expect(script.lines[0].label).toBe("Greeting");
    expect(script.lines[0].cue.pace).toBe("slow");
    expect(script.lines[0].cue.tone).toBe("down");

    expect(script.lines[1].phase).toBe("Introduction");
    expect(script.lines[1].cue.pace).toBe("fast");

    expect(script.lines[2].phase).toBe("Pricing");
    expect(script.lines[2].cue.pauseAfterMs).toBe(2000);
  });

  it("parses html script correctly", () => {
    const html = `
<!DOCTYPE html>
<html>
<head><title>B2B Outbound Script</title></head>
<body>
  <h1>Opener</h1>
  <p>Opener: Hi, this is Sarah from Peak Growth. [🤔 CURIOUS]</p>
  <h2>Pillars</h2>
  <p>Pillar 1: We scale outbound pipelines. [⬇️ DOWN]</p>
</body>
</html>
`;
    const script = parseCustomScript("outbound.html", html);
    expect(script.title).toBe("B2B Outbound Script");
    expect(script.lines.length).toBe(2);
    expect(script.lines[0].phase).toBe("Opener");
    expect(script.lines[0].cue.tone).toBe("up");
    expect(script.lines[1].cue.tone).toBe("down");
  });

  it("saves and retrieves custom scripts from localStorage", () => {
    const md = "# Test\nStep 1: Test step";
    const script = parseCustomScript("test.md", md);

    saveCustomScript("My Script", script);
    const saved = getSavedScripts();
    expect(saved.length).toBe(1);
    expect(saved[0].name).toBe("My Script");
    expect(saved[0].script.title).toBe("Test");

    deleteSavedScript(saved[0].id);
    expect(getSavedScripts().length).toBe(0);
  });
});
