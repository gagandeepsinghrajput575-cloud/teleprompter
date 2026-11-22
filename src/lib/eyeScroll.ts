export interface EyeLineMetrics {
  containerTop: number;
  containerHeight: number;
  scrollTop: number;
  scrollHeight: number;
  elementTop: number;
  elementHeight: number;
}

export interface EyeLineOptions {
  /** Vertical focal ratio from the top of the scroll container (0.24 = upper webcam eye band). */
  eyeLineRatio?: number;
  /** Minimum top clearance in pixels when compact windows push a tall card upward. */
  topSafeMargin?: number;
  /** Minimum bottom clearance in pixels so tall cards do not clip off the bottom edge. */
  bottomSafeMargin?: number;
}

export interface SpringState {
  position: number;
  velocity: number;
  settled: boolean;
}

export const DEFAULT_EYE_LINE_RATIO = 0.24;
export const DEFAULT_TOP_SAFE_MARGIN = 16;
export const DEFAULT_BOTTOM_SAFE_MARGIN = 20;
/** Angular frequency (rad/s) for a critically-damped ~220ms settle with zero overshoot. */
export const SPRING_OMEGA = 24;
export const SETTLE_DISTANCE_PX = 0.5;
export const SETTLE_VELOCITY_PX = 8;
export const USER_SCROLL_HOLD_MS = 2800;

/**
 * Computes the exact `scrollTop` that pins the active card's *first reading line* (top edge)
 * to the upper webcam eye band while guaranteeing tall cards remain fully visible on compact
 * windows (down to `minHeight: 260`).
 */
export function computeEyeLineTarget(
  metrics: EyeLineMetrics,
  options: EyeLineOptions = {}
): number {
  const {
    containerTop,
    containerHeight,
    scrollTop,
    scrollHeight,
    elementTop,
    elementHeight,
  } = metrics;

  if (containerHeight <= 0) return Math.max(0, scrollTop);

  const eyeLineRatio = options.eyeLineRatio ?? DEFAULT_EYE_LINE_RATIO;
  const topSafeMargin = options.topSafeMargin ?? DEFAULT_TOP_SAFE_MARGIN;
  const bottomSafeMargin = options.bottomSafeMargin ?? DEFAULT_BOTTOM_SAFE_MARGIN;

  const desiredAnchor = containerHeight * eyeLineRatio;
  const maxAnchorToFitBottom = Math.max(
    topSafeMargin,
    containerHeight - elementHeight - bottomSafeMargin
  );
  const anchorOffset = Math.min(desiredAnchor, maxAnchorToFitBottom);

  const maxScrollTop = Math.max(0, scrollHeight - containerHeight);
  const rawTarget = scrollTop + (elementTop - containerTop) - anchorOffset;

  return Math.max(0, Math.min(maxScrollTop, rawTarget));
}

/**
 * Exact closed-form step of a critically-damped spring (zeta = 1).
 * Unconditionally stable and identical across 60Hz, 120Hz ProMotion, and dropped frames.
 */
export function stepCriticallyDampedSpring(
  current: number,
  target: number,
  velocity: number,
  dtSeconds: number,
  omega: number = SPRING_OMEGA
): SpringState {
  const displacement = current - target;

  if (
    Math.abs(displacement) <= SETTLE_DISTANCE_PX &&
    Math.abs(velocity) <= SETTLE_VELOCITY_PX
  ) {
    return { position: target, velocity: 0, settled: true };
  }

  const dt = Math.max(0, Math.min(0.05, dtSeconds));
  if (dt === 0) {
    return { position: current, velocity, settled: false };
  }

  const decay = Math.exp(-omega * dt);
  const c = velocity + omega * displacement;
  const nextDisplacement = (displacement + c * dt) * decay;
  const nextVelocity = (velocity - omega * c * dt) * decay;
  const nextPosition = target + nextDisplacement;

  if (
    Math.abs(nextDisplacement) <= SETTLE_DISTANCE_PX &&
    Math.abs(nextVelocity) <= SETTLE_VELOCITY_PX
  ) {
    return { position: target, velocity: 0, settled: true };
  }

  return {
    position: nextPosition,
    velocity: nextVelocity,
    settled: false,
  };
}

/**
 * Stateful rAF scroll controller for the teleprompter viewport.
 * - Reads DOM layout ONCE per retarget (never inside the animation frame -> zero layout thrash).
 * - Seamlessly retargets mid-flight preserving momentum when lines advance rapidly.
 * - Yields immediately when the rep manually scrolls with trackpad/wheel to peek ahead, and
 *   re-engages the instant `activeId` changes.
 */
export class EyeScrollController {
  private rafId: number | null = null;
  private container: HTMLElement | null = null;
  private current = 0;
  private target = 0;
  private velocity = 0;
  private lastTimestamp = 0;
  private userHoldUntil = 0;
  private options: EyeLineOptions;

  constructor(options: EyeLineOptions = {}) {
    this.options = options;
  }

  /** Marks manual trackpad/wheel inspection so reflow ticks do not yank the viewport back. */
  noteUserScroll(now: number = performance.now()): void {
    this.cancel();
    this.userHoldUntil = now + USER_SCROLL_HOLD_MS;
  }

  /** True while the rep is actively peeking elsewhere in the script via wheel/touch. */
  isUserHolding(now: number = performance.now()): boolean {
    return now < this.userHoldUntil;
  }

  /** Re-enables eye-level tracking immediately (called whenever `activeId` changes). */
  clearUserHold(): void {
    this.userHoldUntil = 0;
  }

  /** Cancels any active animation frame and zeroes residual velocity. */
  cancel(): void {
    if (this.rafId !== null) {
      window.cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
    this.velocity = 0;
    this.lastTimestamp = 0;
  }

  /**
   * Aligns `element` to the eye-level anchor inside `container`.
   * Pass `immediate: true` on initial mount, script switch, or `prefers-reduced-motion`.
   */
  align(
    container: HTMLElement,
    element: HTMLElement,
    opts: { immediate?: boolean; respectUserHold?: boolean } = {}
  ): void {
    if (container.clientHeight === 0) {
      this.cancel();
      return;
    }

    if (opts.respectUserHold && this.isUserHolding()) {
      return;
    }

    const cRect = container.getBoundingClientRect();
    const eRect = element.getBoundingClientRect();
    const nextTarget = computeEyeLineTarget(
      {
        containerTop: cRect.top,
        containerHeight: container.clientHeight,
        scrollTop: container.scrollTop,
        scrollHeight: container.scrollHeight,
        elementTop: eRect.top,
        elementHeight: eRect.height,
      },
      this.options
    );

    const reducedMotion =
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    const currentScroll = container.scrollTop;
    const distance = Math.abs(nextTarget - currentScroll);

    // Deadzone: avoid subpixel micro-jitter when already pinned to the eye line.
    if (distance <= SETTLE_DISTANCE_PX && this.rafId === null) {
      return;
    }

    // Large phase/script jumps (> 2.5 viewports) or explicit immediate requests snap cleanly
    // instead of blurring dozens of lines past the rep's eyes.
    const shouldSnap =
      Boolean(opts.immediate) ||
      reducedMotion ||
      distance > container.clientHeight * 2.5;

    if (shouldSnap) {
      this.cancel();
      this.container = container;
      this.current = nextTarget;
      this.target = nextTarget;
      container.scrollTop = nextTarget;
      return;
    }

    this.container = container;
    this.target = nextTarget;

    if (this.rafId === null) {
      this.current = currentScroll;
      this.velocity = 0;
      this.lastTimestamp = 0;
      this.rafId = window.requestAnimationFrame(this.tick);
    }
  }

  private tick = (timestamp: number): void => {
    const container = this.container;
    if (!container || container.clientHeight === 0) {
      this.cancel();
      return;
    }

    if (this.lastTimestamp === 0 || timestamp <= this.lastTimestamp) {
      this.lastTimestamp = timestamp;
      this.rafId = window.requestAnimationFrame(this.tick);
      return;
    }

    const dt = (timestamp - this.lastTimestamp) / 1000;
    this.lastTimestamp = timestamp;

    const step = stepCriticallyDampedSpring(
      this.current,
      this.target,
      this.velocity,
      dt
    );
    this.current = step.position;
    this.velocity = step.velocity;
    container.scrollTop = step.position;

    if (step.settled) {
      this.rafId = null;
      this.lastTimestamp = 0;
      this.velocity = 0;
    } else {
      this.rafId = window.requestAnimationFrame(this.tick);
    }
  };
}
