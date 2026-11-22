import { describe, it, expect } from "vitest";
import { stepLine } from "./navigation";

describe("stepLine navigation", () => {
  const lines = [{ id: 1 }, { id: 2 }, { id: 3 }, { id: 4 }];

  it("advances down within bounds", () => {
    expect(stepLine(lines, 1, "down")).toEqual({ nextId: 2, moved: true });
    expect(stepLine(lines, 2, "down")).toEqual({ nextId: 3, moved: true });
    expect(stepLine(lines, 3, "down")).toEqual({ nextId: 4, moved: true });
  });

  it("holds position at the end when stepping down", () => {
    expect(stepLine(lines, 4, "down")).toEqual({ nextId: 4, moved: false });
  });

  it("retreats up within bounds", () => {
    expect(stepLine(lines, 4, "up")).toEqual({ nextId: 3, moved: true });
    expect(stepLine(lines, 3, "up")).toEqual({ nextId: 2, moved: true });
    expect(stepLine(lines, 2, "up")).toEqual({ nextId: 1, moved: true });
  });

  it("holds position at the beginning when stepping up", () => {
    expect(stepLine(lines, 1, "up")).toEqual({ nextId: 1, moved: false });
  });

  it("F-01: holds position symmetrically when current ID is not in lines (unlisted branch card)", () => {
    // Branch card id 101 is not in the centre-line lines list
    expect(stepLine(lines, 101, "down")).toEqual({ nextId: 101, moved: false });
    expect(stepLine(lines, 101, "up")).toEqual({ nextId: 101, moved: false });
  });

  it("handles empty lines gracefully", () => {
    expect(stepLine([], 1, "down")).toEqual({ nextId: 1, moved: false });
    expect(stepLine([], 1, "up")).toEqual({ nextId: 1, moved: false });
  });
});
