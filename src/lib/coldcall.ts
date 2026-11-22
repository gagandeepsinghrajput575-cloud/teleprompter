/**
 * Cold-call flowchart model and the rep-first matcher that drives it.
 *
 * This is deliberately separate from `tracker.ts`. That tracker answers "how much of this line
 * has been spoken", which is the right question for a linear script. A flowchart needs a
 * different question answered first: **which card am I on**, decided from the words a rep says
 * in the first half-second. So the decision order here is inverted:
 *
 *   Tier 0 — a rep objection opener ("Neither do I", "Our research team"). Two words are enough,
 *            because these openers are distinctive and the whole point is zero-latency. It
 *            outranks everything, including anything the prospect said.
 *   Tier 1 — the opening words of a centre turn or a branch line. Three words, so
 *            "hi it's Gagan" lands on Turn 1 and "got it yeah so" lands on Turn 4.
 *   Tier 2 — a prospect trigger ("just send me an email"), matched against the branch copy.
 *            Lowest priority, and never allowed to override a rep opener.
 *
 * Everything is local: no network, no Drive, no service. The fixture is the transcript of
 * record, so the matcher can be scored offline and identically every run.
 */
import fixture from "../fixtures/coldcall_scalix.json";
import { STOPWORDS } from "./tracker";

export type FlowCard = {
  id: number;
  /** Present only on branches. */
  side?: "left" | "right";
  /** Present only on the centre path, e.g. "Turn 4". */
  turn?: string;
  label: string;
  /** Spoken copy, verbatim, beats joined. */
  raw: string;
  /** The spoken copy split at paragraph boundaries. */
  beats: string[];
  /** Opening words of each beat; the Tier 1 evidence. */
  anchors: string[];
  /** The rep's short objection openers; the Tier 0 evidence. */
  objectionAnchors: string[];
  /** Prospect wording that should surface this card (Tier 2). */
  trigger?: string;
  /** The card to move to once this one is delivered. */
  next: number | null;
  /** Whether this objection is archived / deleted from the active display. */
  deleted?: boolean;
};

export type ColdCallFlow = {
  id: string;
  title: string;
  version: number;
  audience?: string;
  notes?: string;
  center: FlowCard[];
  branches: FlowCard[];
};

export const FLOW = fixture as unknown as ColdCallFlow;

/** Tier thresholds, in words. Exported so a test can assert the contract, not the code. */
export const TIER0_MIN_WORDS = 2;
export const TIER1_MIN_WORDS = 3;
export const TIER2_MIN_WORDS = 4;

/**
 * Every tier matches against the same tail, so `end` offsets are comparable across tiers.
 * That comparability is what lets "most recently said wins" arbitrate between a stale objection
 * opener and the line the rep is actually reading now.
 */
const MATCH_WINDOW = 12;
/** How many normalized words of speech the matcher remembers. */
const RECENT_LIMIT = 30;

/**
 * Normalizes speech and script copy to the same word sequence.
 *
 * Placeholders (`[Prospect Name]`) are dropped: the rep says a real name, so the placeholder
 * must not sit in the middle of an anchor and break contiguity. Apostrophes are dropped so
 * Deepgram's "don't" and the script's "don't" agree, and a colon-split time ("15:30") becomes
 * the two spoken words "15 30".
 */
export function normalizeWords(text: string): string[] {
  return text
    .replace(/\[[^\]]*\]/g, " ")
    .replace(/[\u{1F000}-\u{1FAFF}\u{2190}-\u{2BFF}\u{FE0F}\u{200D}]/gu, " ")
    .toLowerCase()
    .replace(/['\u2019]/g, "")
    .replace(/[^a-z0-9]+/g, " ")
    .trim()
    .split(/\s+/)
    .filter(Boolean);
}

/**
 * Longest prefix of `anchor` that appears contiguously in the recent speech.
 *
 * Returns the matched word count plus where the match ended (`end`), or null when nothing
 * matched at least `minWords`. Contiguous matching is what makes this fast and self-limiting:
 * "neither do i" fires, "i do not agree with neither of those" does not.
 */
export type Hit = { words: number; end: number };

export function findAnchor(
  recent: string[],
  anchor: string[],
  minWords: number,
  window: number
): Hit | null {
  if (anchor.length < minWords) return null;
  const tail = recent.slice(-window);
  // Longest first, but scanning backwards so `end` is the most recent occurrence: a line's
  // opening words stay in the window for several utterances, and the newest match is the one
  // being spoken now.
  for (let k = Math.min(anchor.length, tail.length); k >= minWords; k -= 1) {
    for (let start = tail.length - k; start >= 0; start -= 1) {
      let matched = true;
      for (let offset = 0; offset < k; offset += 1) {
        if (tail[start + offset] !== anchor[offset]) {
          matched = false;
          break;
        }
      }
      if (matched) return { words: k, end: start + k };
    }
  }
  return null;
}

/**
 * Longest contiguous run of `phrase` that appears in the recent speech, anywhere in the phrase.
 *
 * Used for prospect triggers, which are mid-sentence prose ("just send me an email with some
 * info") rather than author-chosen openers. Anchors stay prefix-only: an opener is defined by
 * where it starts.
 */
export function findPhrase(
  recent: string[],
  phrase: string[],
  minWords: number,
  window: number
): Hit | null {
  if (phrase.length < minWords) return null;
  const tail = recent.slice(-window);
  for (let k = Math.min(phrase.length, tail.length); k >= minWords; k -= 1) {
    for (let at = tail.length - k; at >= 0; at -= 1) {
      for (let start = 0; start + k <= phrase.length; start += 1) {
        let matched = true;
        for (let offset = 0; offset < k; offset += 1) {
          if (tail[at + offset] !== phrase[start + offset]) {
            matched = false;
            break;
          }
        }
        if (matched) return { words: k, end: at + k };
      }
    }
  }
  return null;
}

export type Tier = 0 | 1 | 2;

export type Decision = {
  id: number;
  tier: Tier;
  wordsMatched: number;
  anchor: string;
  /** Where the match ended in the matching window. */
  end: number;
  /** Which utterance the match came from; the newest utterance always wins. */
  utterance: number;
};

type PreparedCard = {
  card: FlowCard;
  anchors: string[][];
  objectionAnchors: string[][];
  triggers: string[][];
};

/** One normalized word and the utterance it was heard in. */
type Word = { w: string; u: number };

function prepare(flow: ColdCallFlow): PreparedCard[] {
  const cards = [...flow.center, ...flow.branches];
  return cards.map((card) => ({
    card,
    anchors: card.anchors.map(normalizeWords).filter((words) => words.length > 0),
    objectionAnchors: card.objectionAnchors.map(normalizeWords).filter((words) => words.length > 0),
    // A prospect trigger is prose ("If they say general "Not interested"..."), so only the
    // quoted part, if any, is matchable. Without quotes, the whole line is the trigger.
    triggers: [card.trigger ?? ""]
      .flatMap((value) => value.match(/"[^"]+"|“[^”]+”/g) ?? [value])
      .map((value) => normalizeWords(value.replace(/["“”]/g, " ")))
      .filter((words) => words.length >= TIER2_MIN_WORDS),
  }));
}

export class ColdCallTracker {
  readonly flow: ColdCallFlow;
  private readonly prepared: PreparedCard[];
  private active: number;
  /**
   * Settled speech, tagged with the utterance it came from.
   *
   * Utterance identity, not word distance, is what makes a match stale. A rep routinely talks
   * for another ten words after an objection opener inside the same final transcript, so
   * "N words ago" expires evidence that is still current; "a previous utterance" does not.
   */
  private recent: Word[] = [];
  private history: number[] = [];
  /** Utterances seen so far. Interims belong to the one after these. */
  private utterances = 0;
  /**
   * An objection card holds its place until the rep says something deliberate again. Without
   * this the prospect's reply — matched by the Tier 2 trigger — would steal the card the rep
   * just opened.
   */
  private hold = false;
  /**
   * The interim transcript of the sentence in progress. Deepgram rewrites interims, so they are
   * held here and *replaced*, never appended — appending duplicated every phrase into the
   * window and made anchors look newer than they were.
   */
  private pendingInterim: Word[] = [];

  constructor(flow: ColdCallFlow = FLOW, startId?: number) {
    this.flow = flow;
    this.prepared = prepare(flow);
    this.active = startId ?? flow.center[0]?.id ?? 1;
    this.history = [this.active];
  }

  get activeId(): number {
    return this.active;
  }

  get trail(): readonly number[] {
    return this.history;
  }

  get cards(): FlowCard[] {
    return [...this.flow.center, ...this.flow.branches];
  }

  card(id: number): FlowCard | undefined {
    return this.cards.find((card) => card.id === id);
  }

  side(side: "left" | "right"): FlowCard[] {
    return this.flow.branches.filter((branch) => branch.side === side);
  }

  /** The turn the flow expects next: the card's `next`, else the following centre turn. */
  nextExpectedId(): number | null {
    const current = this.card(this.active);
    if (current?.next != null) return current.next;
    const index = this.flow.center.findIndex((line) => line.id === this.active);
    if (index === -1) return null;
    return this.flow.center[index + 1]?.id ?? null;
  }

  reset(startId?: number): void {
    this.active = startId ?? this.flow.center[0]?.id ?? 1;
    this.recent = [];
    this.pendingInterim = [];
    this.utterances = 0;
    this.hold = false;
    this.history = [this.active];
  }

  private pushHistory(id: number): void {
    this.history.push(id);
    if (this.history.length > 64) {
      this.history.shift();
    }
  }

  /** Manual tap: always wins, and is remembered so the auto-scroll can follow it. */
  jump(id: number): number {
    if (!this.card(id)) return this.active;
    this.active = id;
    // A deliberate tap releases any objection hold: the rep has chosen where to be.
    this.hold = false;
    // Clear pending interim buffer so speech uttered before the tap doesn't steal the card back
    this.pendingInterim = [];
    this.pushHistory(id);
    return this.active;
  }

  /** Moves to the expected next turn without waiting for speech. */
  advance(): number {
    const next = this.nextExpectedId();
    return next == null ? this.active : this.jump(next);
  }

  /** What the matcher can currently see: settled speech plus the in-progress sentence. */
  private get view(): Word[] {
    return this.pendingInterim.length === 0
      ? this.recent
      : [...this.recent, ...this.pendingInterim];
  }

  /** Everything currently in the window, for diagnostics and tests. */
  get heard(): readonly string[] {
    return this.view.map((word) => word.w);
  }

  /**
   * Classifies the most recent speech and records it.
   *
   * `isFinal` matters: a final supersedes the interim that predicted it, so the interim tail is
   * dropped rather than duplicated. Getting this wrong inflated the window and made the previous
   * phrase look like the most recent one.
   */
  decide(text: string, isFinal = true): Decision | null {
    const words = normalizeWords(text);
    if (isFinal) {
      if (words.length > 0) {
        this.utterances += 1;
        this.recent.push(...words.map((w) => ({ w, u: this.utterances })));
        if (this.recent.length > RECENT_LIMIT) {
          this.recent = this.recent.slice(-RECENT_LIMIT);
        }
      }
      this.pendingInterim = [];
    } else {
      this.pendingInterim = words.map((w) => ({ w, u: this.utterances + 1 }));
    }
    return this.classify();
  }

  /** Records the speech and moves the highlight. Returns the new card id when it moved. */
  observe(text: string, isFinal = true): number | null {
    const decision = this.decide(text, isFinal);
    if (!decision || decision.id === this.active) return null;
    this.active = decision.id;
    this.pushHistory(decision.id);
    return this.active;
  }

  /** Moves back one centre turn (arrow keys in the flowchart view). */
  retreat(): number {
    const index = this.flow.center.findIndex((line) => line.id === this.active);
    if (index <= 0) return this.active;
    return this.jump(this.flow.center[index - 1].id);
  }

  private classify(): Decision | null {
    const objections = this.bestIn(
      this.prepared.flatMap((entry) =>
        entry.objectionAnchors.map((anchor) => ({ entry, anchor }))
      ),
      TIER0_MIN_WORDS,
      false
    );
    const lines = this.bestIn(
      this.prepared.flatMap((entry) => entry.anchors.map((anchor) => ({ entry, anchor }))),
      TIER1_MIN_WORDS,
      false,
      (a, b) => this.preferLine(a, b)
    );
    const triggers = this.bestIn(
      this.prepared.flatMap((entry) => entry.triggers.map((anchor) => ({ entry, anchor }))),
      TIER2_MIN_WORDS,
      true
    );

    const candidates: { hit: Omit<Decision, "tier">; tier: Tier }[] = [];
    if (objections && this.carriesSignal(objections)) {
      candidates.push({ hit: objections, tier: 0 });
    }
    if (lines) candidates.push({ hit: lines, tier: 1 });
    // While an objection card is open, only deliberate rep speech moves off it: the prospect's
    // reply must not steal the card the rep just read.
    if (triggers && !this.hold) candidates.push({ hit: triggers, tier: 2 });
    if (candidates.length === 0) return null;

    // Newest utterance wins outright: an opener from the previous sentence is history, however
    // distinctive it was. Within one utterance, tier decides (the rep's own words beat the
    // prospect's), then the more recent match, then the longer one.
    candidates.sort(
      (a, b) =>
        b.hit.utterance - a.hit.utterance ||
        a.tier - b.tier ||
        b.hit.end - a.hit.end ||
        b.hit.wordsMatched - a.hit.wordsMatched
    );
    const winner = candidates[0];
    this.hold = winner.tier === 0;
    return { ...winner.hit, tier: winner.tier };
  }

  /**
   * Two words is the Tier 0 floor, but a pair of function words is not an opener: "got it"
   * appears in half the script and would jump to whichever card happened to be nearest. The
   * shortest allowed match must carry at least one content word ("neither do", "our research").
   */
  private carriesSignal(hit: { anchor: string; wordsMatched: number }): boolean {
    if (hit.wordsMatched > TIER0_MIN_WORDS) return true;
    return hit.anchor
      .split(" ")
      .slice(0, hit.wordsMatched)
      .some((word) => !STOPWORDS.has(word));
  }

  private bestIn(
    candidates: { entry: PreparedCard; anchor: string[] }[],
    minWords: number,
    phrase = false,
    tieBreak?: (
      a: { entry: PreparedCard; wordsMatched: number },
      b: { entry: PreparedCard; wordsMatched: number }
    ) => number
  ): Omit<Decision, "tier"> | null {
    type Scored = {
      entry: PreparedCard;
      id: number;
      wordsMatched: number;
      anchor: string;
      end: number;
      utterance: number;
    };
    let best: Scored | null = null;
    const view = this.view;
    const words = view.map((entry) => entry.w);
    const base = Math.max(0, view.length - MATCH_WINDOW);
    for (const candidate of candidates) {
      const hit = phrase
        ? findPhrase(words, candidate.anchor, minWords, MATCH_WINDOW)
        : findAnchor(words, candidate.anchor, minWords, MATCH_WINDOW);
      if (!hit) continue;
      const scored: Scored = {
        entry: candidate.entry,
        id: candidate.entry.card.id,
        wordsMatched: hit.words,
        anchor: candidate.anchor.join(" "),
        end: hit.end,
        utterance: view[base + hit.end - 1]?.u ?? 0,
      };
      if (!best) {
        best = scored;
        continue;
      }
      // Newest utterance, then the latest match, then the longer one, then the caller's policy.
      // `tieBreak` returns a negative number when its FIRST argument is the better choice, which
      // is why it is negated: this block replaces `best` only when the candidate is better.
      let comparison = scored.utterance - best.utterance;
      if (comparison === 0) comparison = scored.end - best.end;
      if (comparison === 0) comparison = scored.wordsMatched - best.wordsMatched;
      if (comparison === 0 && tieBreak) comparison = -tieBreak(best, scored);
      if (comparison > 0) best = scored;
    }
    if (!best) return null;
    return {
      id: best.id,
      wordsMatched: best.wordsMatched,
      anchor: best.anchor,
      end: best.end,
      utterance: best.utterance,
    };
  }

  /** Negative when `a` is the better choice. */
  private preferLine(
    a: { entry: PreparedCard; wordsMatched: number },
    b: { entry: PreparedCard; wordsMatched: number }
  ): number {
    if (a.wordsMatched !== b.wordsMatched) return b.wordsMatched - a.wordsMatched;
    const expected = this.nextExpectedId();
    const aNext = a.entry.card.id === expected ? 1 : 0;
    const bNext = b.entry.card.id === expected ? 1 : 0;
    if (aNext !== bNext) return bNext - aNext;
    const aCenter = a.entry.card.side ? 0 : 1;
    const bCenter = b.entry.card.side ? 0 : 1;
    if (aCenter !== bCenter) return bCenter - aCenter;
    const index = (id: number) => {
      const at = this.flow.center.findIndex((line) => line.id === id);
      return at === -1 ? Number.MAX_SAFE_INTEGER : at;
    };
    return Math.abs(index(a.entry.card.id) - index(this.active)) -
      Math.abs(index(b.entry.card.id) - index(this.active));
  }
}

/** True for the cards that quote a booking time, so the UI can show live day names. */
export function isBookingCard(card: FlowCard): boolean {
  return /tomorrow|thursday|15:30|12:30/i.test(card.raw);
}

const WEEKDAYS = [
  "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

export type AppointmentDay = {
  offset: number;
  weekday: string;
  /** e.g. "Sat 14 Nov" */
  short: string;
  iso: string;
};

/**
 * Tomorrow and the day after, computed at render time rather than baked into the copy: a script
 * that says "Thursday" is wrong six days out of seven, and the rep reads it aloud.
 */
export function appointmentDays(now: Date = new Date()): {
  tomorrow: AppointmentDay;
  dayAfter: AppointmentDay;
} {
  const at = (offset: number): AppointmentDay => {
    const date = new Date(now.getFullYear(), now.getMonth(), now.getDate() + offset);
    return {
      offset,
      weekday: WEEKDAYS[date.getDay()],
      short: `${WEEKDAYS[date.getDay()].slice(0, 3)} ${date.getDate()} ${MONTHS[date.getMonth()]}`,
      iso: `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(
        date.getDate()
      ).padStart(2, "0")}`,
    };
  };
  return { tomorrow: at(1), dayAfter: at(2) };
}
