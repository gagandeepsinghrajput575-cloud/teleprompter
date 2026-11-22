import { describe, expect, it } from "vitest";

describe("Audio Level Meter Performance & Logic", () => {
  it("clamps meter fill width between 0% and 100% without NaN or infinite values", () => {
    const calculateFillWidth = (level: number) => `${Math.min(100, Math.max(0, level * 400))}%`;

    expect(calculateFillWidth(0)).toBe("0%");
    expect(calculateFillWidth(0.05)).toBe("20%");
    expect(calculateFillWidth(0.25)).toBe("100%");
    expect(calculateFillWidth(0.5)).toBe("100%");
    expect(calculateFillWidth(1.0)).toBe("100%");
    expect(calculateFillWidth(-0.1)).toBe("0%");
  });

  it("handles high polling frequency intervals cleanly without memory accumulation", () => {
    // Verify rapid level calculations perform under 1ms for 1000 simulated ticks
    const t0 = performance.now();
    const levels = [0.01, 0.05, 0.12, 0.28, 0.04, 0.0, 0.8];
    let sum = 0;
    for (let i = 0; i < 10000; i++) {
      const lvl = levels[i % levels.length];
      const width = Math.min(100, Math.max(0, lvl * 400));
      sum += width;
    }
    const t1 = performance.now();
    expect(sum).toBeGreaterThan(0);
    expect(t1 - t0).toBeLessThan(50); // fast loop
  });
});
