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

import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";

describe("App render", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("renders default coldcall view without crashing", () => {
    const html = renderToString(createElement(App));
    expect(html).toContain("hud");
    expect(html).toContain("flow");
    expect(html).toContain("Cold Call");
  });

  it("renders call modal screen when selectedCall is null", () => {
    localStorage.setItem("tp.selectedCall", "");
    // Simulate initial selectedCall being null by removing from localStorage
    localStorage.removeItem("tp.selectedCall");
    const html = renderToString(createElement(App));
    expect(html).toContain("hud");
  });

  it("renders Call 1 audit blocks when selectedCall is call1", () => {
    localStorage.setItem("tp.selectedCall", "call1");
    const html = renderToString(createElement(App));
    expect(html).toContain("hud");
    expect(html).toContain("blocks");
    expect(html).toContain("Audit");
  });

  it("renders Call 2 sales blocks when selectedCall is call2", () => {
    localStorage.setItem("tp.selectedCall", "call2");
    const html = renderToString(createElement(App));
    expect(html).toContain("hud");
    expect(html).toContain("blocks");
    expect(html).toContain("Close");
  });
});
