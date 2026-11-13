/**
 * Script-line tracker — executable TypeScript port of `src-tauri/src/tracker.rs`.
 *
 * Why a port rather than "just fix the Rust": the sandbox this work is being done in has no
 * Rust toolchain and no route to crates.io, so nothing under `src-tauri/` can be compiled or
 * tested here. Tracking behaviour is the product's core claim ("it follows the line you are
 * actually delivering"), and a change to it that cannot be run is a guess. Porting it to TS
 * buys two things:
 *
 *   1. every threshold and guard below is exercised by tests that actually execute, and
 *   2. a replay harness can score any script + transcript pair, offline, deterministically.
 *
 * Parity is the default: with `retainWords: 0` this reproduces the Rust implementation's
 * decisions, including its quirks, and the tests in `tracker.test.ts` are line-for-line ports
 * of the Rust test module. Deliberate departures are listed under `DEVIATIONS`.
 *
 * The algorithm is unchanged from the Rust original: a rolling 18-word window of what was
 * said, scored against every line's vocabulary with inverse-document-frequency weighting, and
 * four gates before the local match is allowed to trust itself (hit count, absolute score
 * floor, coverage, margin over the runner-up). Hysteresis then decides whether to commit.
 */

/**
 * Deliberate differences from `tracker.rs`:
 *
 *  - `retainWords` optionally keeps the last N words of the window on commit instead of wiping.
 *    This is OFF by default: measurement (see `tracker.test.ts`, "commit reset") showed the wipe
 *    does *not* stall a line-by-line walk, so enabling it would be a change with no evidence
 *    behind it. It stays available for scripts whose lines arrive in short fragments.
 *  - Filters, thresholds and weights are configurable, so a script can be tuned without editing
 *    constants that other scripts silently depend on.
 *  - The line-vocabulary index is computed once instead of on every cycle (the script is
 *    immutable), which removes ~26 tokenisations per cycle.
 */
export const DEVIATIONS = [
  "optional window retention on commit (default off; measurement found no stall to fix)",
  "line vocabulary is indexed once, not per cycle",
  "thresholds are configurable per script",
] as const;

export type TrackerConfig = {
  /** How many trailing words of spoken speech the matcher sees. */
  bufferWords: number;
  /** New finalized words required before a classification cycle runs. */
  wordsPerCycle: number;
  /** Interim words required before an in-progress sentence can trigger a cycle. */
  partialWordsTrigger: number;
  /** Stage 1 confidence margin: the best line must beat the runner-up by this ratio. */
  localMargin: number;
  /** Minimum number of distinct shared words before stage 1 may trust itself. */
  minHits: number;
  /** Absolute score floor used when no other line scores at all (the ratio is vacuous then). */
  minDominantScore: number;
  /** Fraction of a line's own vocabulary that must actually have been spoken. */
  minCoverage: number;
  /** Words kept after a commit. `0` reproduces the Rust behaviour. */
  retainWords: number;
  /** Confirmation counts that must agree before a move is committed. */
  confirmations: { confident: number; forward: number; backward: number };
};

export const DEFAULT_CONFIG: TrackerConfig = {
  bufferWords: 18,
  wordsPerCycle: 2,
  partialWordsTrigger: 2,
  localMargin: 1.25,
  minHits: 2,
  minDominantScore: 1.6,
  minCoverage: 0.25,
  retainWords: 0,
  confirmations: { confident: 1, forward: 2, backward: 3 },
};

/**
 * Same list as the Rust `STOPWORDS`. These are words that carry no identifying signal: they
 * appear in nearly every line, so letting them score would flatten the difference between
 * lines rather than sharpen it.
 */
export const STOPWORDS: ReadonlySet<string> = new Set([
  "the", "and", "you", "your", "for", "that", "this", "with", "are", "what", "how", "they",
  "them", "have", "has", "was", "were", "from", "but", "not", "all", "can", "will", "just",
  "out", "about", "into", "when", "then", "than", "there", "here", "does", "did", "doing", "get",
  "got", "one", "two", "our", "its", "we", "us", "me", "my", "so", "if", "or", "of", "to", "in",
  "on", "at", "is", "it", "be", "as", "a", "an", "do", "much", "most", "right", "now", "like",
  "really", "actually", "think", "know",
]);

export type TrackedLine = {
  id: number;
  phase: string;
  label: string;
  /** Spoken text with delivery markers stripped. */
  body: string;
};

export type LocalMatch = {
  /** Best-scoring line id. */
  id: number;
  /** True only when every gate passed. */
  confident: boolean;
  score: number;
  /** Distinct buffer words the line shares. */
  hits: number;
  /** `hits / |line vocabulary|`, where the vocabulary includes the label. */
  coverage: number;
  /** The runner-up's score, for diagnosing near-ties. */
  second: number;
};

export type Snapshot = {
  activeId: number;
  pendingId: number | null;
  pendingCount: number;
  bufferedWords: number;
  partialWords: number;
  secondsSinceChange: number;
};

/**
 * Splits text into lowercase content words.
 *
 * Deliberately identical in shape to the Rust `tokens`: split on anything that is not
 * alphanumeric, an apostrophe or a hyphen, drop words shorter than three characters, drop
 * stopwords. Short words are dropped because "AI" and "up" collide across every line.
 */
export function tokenize(text: string): Set<string> {
  const out = new Set<string>();
  for (const piece of text.toLowerCase().split(/[^a-z0-9'-]+/)) {
    const word = piece.replace(/^'+|'+$/g, "");
    if (word.length >= 3 && !STOPWORDS.has(word)) out.add(word);
  }
  return out;
}

function clamp01(value: number): number {
  return value < 0 ? 0 : value > 1 ? 1 : value;
}

export class Tracker {
  readonly config: TrackerConfig;
  private readonly lines: TrackedLine[];
  /** Line vocabulary, indexed once. `label ∪ body`, matching the Rust behaviour. */
  private readonly vocabulary: Set<string>[];

  private active: number;
  private window: string[] = [];
  private wordsSinceCycle = 0;
  private pendingId: number | null = null;
  private pendingCount = 0;
  private changedAt = Date.now();
  private partial = "";
  private partialMarked = 0;

  constructor(lines: TrackedLine[], config: Partial<TrackerConfig> = {}) {
    if (lines.length === 0) throw new Error("script has no lines");
    this.lines = lines;
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.vocabulary = lines.map((line) => tokenize(`${line.label} ${line.body}`));
    this.active = lines[0].id;
  }

  get activeId(): number {
    return this.active;
  }

  /** Every line, for the HUD and for the LLM candidate window. */
  get scriptLines(): readonly TrackedLine[] {
    return this.lines;
  }

  /** The rolling window plus the in-progress sentence, i.e. what the matcher sees. */
  spokenBuffer(): string {
    const partial = this.partial.trim();
    return partial ? `${this.window.join(" ")} ${partial}`.trim() : this.window.join(" ");
  }

  /** Adds finalized words; returns how many were new. */
  pushText(text: string): number {
    let added = 0;
    for (const word of text.split(/\s+/)) {
      if (!word) continue;
      if (this.window.length === this.config.bufferWords) this.window.shift();
      this.window.push(word);
      added += 1;
    }
    this.wordsSinceCycle += added;
    return added;
  }

  /** Interims are rewritten by the recogniser, so this replaces rather than appends. */
  setPartial(text: string): void {
    this.partial = text.trim();
  }

  /** A finalized segment supersedes the interim that predicted it. */
  clearPartial(): void {
    this.partial = "";
    this.partialMarked = 0;
  }

  /**
   * Stage 0: is there enough new speech to justify a cycle? Interims count, which is what lets
   * the highlight move while a sentence is still being spoken.
   */
  shouldRunCycle(): boolean {
    if (this.wordsSinceCycle >= this.config.wordsPerCycle) return true;
    const partialWords = this.partial.split(/\s+/).filter(Boolean).length;
    return (
      partialWords >= this.config.partialWordsTrigger && partialWords !== this.partialMarked
    );
  }

  markCycleRan(): void {
    this.partialMarked = this.partial.split(/\s+/).filter(Boolean).length;
    this.wordsSinceCycle = 0;
  }

  /**
   * Stage 1: lexical match with rare words weighted higher.
   *
   * `df` is a document frequency over lines, so a word shared by one line contributes
   * `ln(N)` while a word every line uses contributes ~0. The `.max(0.05)` floor stops a
   * universally-shared word from contributing nothing at all, which would make the hit count
   * and the score disagree.
   */
  localMatch(): LocalMatch | null {
    const buffer = tokenize(this.spokenBuffer());
    if (buffer.size === 0) return null;

    const total = this.vocabulary.length;
    const scored = this.lines.map((line, index) => {
      const words = this.vocabulary[index];
      let score = 0;
      let hits = 0;
      for (const token of buffer) {
        if (!words.has(token)) continue;
        hits += 1;
        let df = 0;
        for (const other of this.vocabulary) if (other.has(token)) df += 1;
        score += Math.max(Math.log(total / Math.max(1, df)), 0.05);
      }
      return { id: line.id, score, hits, lineTokens: words.size };
    });

    scored.sort((a, b) => b.score - a.score || a.id - b.id);
    const [best, runnerUp] = scored;
    if (best.score <= 0) return null;

    const second = runnerUp?.score ?? 0;
    // A lone shared word must not count as evidence. When every other line scores zero the
    // ratio test passes vacuously, which is how off-script speech ("driving down from Ohio")
    // used to jump to a line that merely contains the word "driving".
    const marginOk =
      second <= Number.EPSILON
        ? best.score >= this.config.minDominantScore
        : best.score >= second * this.config.localMargin;

    const coverage = best.lineTokens === 0 ? 0 : best.hits / best.lineTokens;
    const confident =
      best.hits >= this.config.minHits && marginOk && coverage >= this.config.minCoverage;

    return {
      id: best.id,
      confident,
      score: best.score,
      hits: best.hits,
      coverage: clamp01(coverage),
      second,
    };
  }

  /**
   * Applies hysteresis. Returns the new active id when a change is committed.
   *
   * Forward moves need less proof than backward moves: a sales call moves forwards, so a
   * backward jump is more likely to be a misheard word than a real return to an earlier line.
   */
  consider(proposed: number, confident: boolean): number | null {
    if (!this.lines.some((line) => line.id === proposed)) return null;
    if (proposed === this.active) {
      this.pendingId = null;
      this.pendingCount = 0;
      return null;
    }

    const required = confident
      ? this.config.confirmations.confident
      : proposed > this.active
        ? this.config.confirmations.forward
        : this.config.confirmations.backward;

    if (this.pendingId === proposed) {
      this.pendingCount += 1;
    } else {
      this.pendingId = proposed;
      this.pendingCount = 1;
    }

    if (this.pendingCount < required) return null;

    this.active = proposed;
    this.pendingId = null;
    this.pendingCount = 0;
    this.commit();
    return proposed;
  }

  /** Manual override always wins immediately. */
  manual(delta: number): number {
    const index = this.lines.findIndex((line) => line.id === this.active);
    const base = index === -1 ? 0 : index;
    const next = Math.min(Math.max(base + delta, 0), this.lines.length - 1);
    this.active = this.lines[next].id;
    this.pendingId = null;
    this.pendingCount = 0;
    this.commit();
    return this.active;
  }

  /**
   * Reset the context a commit invalidates.
   *
   * The Rust version cleared the window and the interim tail outright. That leaves the next
   * cycle with nothing to match against: two new words is enough to run a cycle but not enough
   * to clear `minHits`, so every advance after the first has to wait for the language model.
   * Keeping a short tail keeps local matching usable while hysteresis stops the tail from
   * dragging the highlight backwards.
   */
  private commit(): void {
    const keep = Math.max(0, Math.min(this.config.retainWords, this.window.length));
    this.window = keep === 0 ? [] : this.window.slice(this.window.length - keep);
    this.partial = "";
    this.partialMarked = 0;
    this.wordsSinceCycle = 0;
    this.changedAt = Date.now();
  }

  snapshot(): Snapshot {
    return {
      activeId: this.active,
      pendingId: this.pendingId,
      pendingCount: this.pendingCount,
      bufferedWords: this.window.length,
      partialWords: this.partial.split(/\s+/).filter(Boolean).length,
      secondsSinceChange: Math.round((Date.now() - this.changedAt) / 1000),
    };
  }

  /**
   * Lines within reach of the current position, used to shrink the LLM prompt. Three back,
   * three forward, clamped at both ends.
   */
  candidateWindow(): TrackedLine[] {
    const index = this.lines.findIndex((line) => line.id === this.active);
    const base = index === -1 ? 0 : index;
    return this.lines.slice(Math.max(0, base - 3), Math.min(this.lines.length, base + 4));
  }

  /** True when stage 1 cannot settle the line, so the model would be consulted. */
  static needsLlm(match: LocalMatch | null): boolean {
    return !(match && match.confident);
  }
}
