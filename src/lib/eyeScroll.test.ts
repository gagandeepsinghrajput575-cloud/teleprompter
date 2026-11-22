import { describe, expect, it } from "vitest";
import {
  computeEyeLineTarget,
  stepCriticallyDampedSpring,
  DEFAULT_EYE_LINE_RATIO,
} from "./eyeScroll";

describe("computeEyeLineTarget", () => {
  it("pins the top reading line to the exact same eye band regardless of card height", () => {
    const base = {
      containerTop: 60,
      containerHeight: 600,
      scrollTop: 400,
      scrollHeight: 4000,
      elementTop: 320,
    };

    const shortCardTarget = computeEyeLineTarget({ ...base, elementHeight: 44 });
    const longLine32Target = computeEyeLineTarget({ ...base, elementHeight: 196 });

    // Expected eye band = 600 * 0.24 = 144px from container top.
    // Raw delta = (320 - 60) - 144 = +116 -> target = 400 + 116 = 516.
    expect(shortCardTarget).toBe(516);
    expect(longLine32Target).toBe(516);
    expect(DEFAULT_EYE_LINE_RATIO).toBe(0.24);
  });

  it("pulls tall cards upward on compact windows (minHeight 260) so bottom text never clips", () => {
    const target = computeEyeLineTarget({
      containerTop: 0,
      containerHeight: 240,
      scrollTop: 500,
      scrollHeight: 3000,
      elementTop: 120,
      elementHeight: 200,
    });
    // maxAnchorToFitBottom = 240 - 200 - 20 = 20px (instead of 57.6px)
    // raw = 500 + 120 - 20 = 600
    expect(target).toBe(600);
  });

  it("clamps cleanly to [0, scrollHeight - containerHeight]", () => {
    const topClamped = computeEyeLineTarget({
      containerTop: 0,
      containerHeight: 600,
      scrollTop: 0,
      scrollHeight: 2000,
      elementTop: 20,
      elementHeight: 60,
    });
    expect(topClamped).toBe(0);

    const bottomClamped = computeEyeLineTarget({
      containerTop: 0,
      containerHeight: 600,
      scrollTop: 1300,
      scrollHeight: 2000,
      elementTop: 500,
      elementHeight: 60,
    });
    expect(bottomClamped).toBe(1400);
  });
});

describe("stepCriticallyDampedSpring", () => {
  it("settles within ~250ms with zero overshoot at both 60Hz and 120Hz", () => {
    for (const hz of [60, 120]) {
      const dt = 1 / hz;
      let pos = 0;
      let vel = 0;
      let settled = false;
      const target = 120;

      for (let frame = 0; frame < Math.ceil(0.35 * hz); frame++) {
        const next = stepCriticallyDampedSpring(pos, target, vel, dt);
        expect(next.position).toBeGreaterThanOrEqual(pos - 1e-6);
        expect(next.position).toBeLessThanOrEqual(target + 1e-6); // Zero overshoot
        pos = next.position;
        vel = next.velocity;
        if (next.settled) {
          settled = true;
          break;
        }
      }

      expect(settled).toBe(true);
      expect(pos).toBe(target);
      expect(vel).toBe(0);
    }
  });

  it("smoothly retargets mid-flight without snapping backward", () => {
    // Move halfway toward 100px
    let state = stepCriticallyDampedSpring(0, 100, 0, 0.05);
    expect(state.position).toBeGreaterThan(20);
    expect(state.velocity).toBeGreaterThan(0);

    // Rapid next-line advance retargets to 220px mid-flight
    const next = stepCriticallyDampedSpring(state.position, 220, state.velocity, 0.016);
    expect(next.position).toBeGreaterThan(state.position);
    expect(next.position).toBeLessThan(220);
    expect(next.settled).toBe(false);
  });
});
