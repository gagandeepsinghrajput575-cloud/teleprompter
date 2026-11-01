import type { Cue, Pace, Tone } from "./cues";

/**
 * A delivery annotation lifted out of the script text, e.g. `[⬇️ DOWN]`, `[🐢 SLOW & LOW]`,
 * `[⚡ PACE UP]`, `[🤔 CURIOUS]`, `[⏸️ 1.5s]`.
 */
export type Annotation =
  | { kind: "tone"; value: Exclude<Tone, "neutral"> }
  | { kind: "pace"; value: Exclude<Pace, "normal"> }
  | { kind: "pause"; ms: number };

export type ScriptBlock = {
  id: number;
  phase: string;
  label: string;
  summary: string;
  /** Original text, annotations included. */
  raw: string;
  /** Spoken text with annotations stripped. */
  body: string;
  cue: Cue;
  annotations: Annotation[];
};

export type Script = {
  id: string;
  title: string;
  version: number;
  audience?: string;
  notes?: string;
  blocks: ScriptBlock[];
};

/**
 * Classifies one bracketed token. Returns null for non-annotation brackets such as the
 * `[Name]` placeholder, which must survive into the spoken text.
 */
export function classifyToken(token: string): Annotation[] | null {
  const pause = token.match(/^[\s\u23F8\uFE0F]*([0-9]*\.?[0-9]+)\s*s\s*$/i);
  if (pause) {
    const seconds = Number.parseFloat(pause[1]);
    if (Number.isFinite(seconds)) {
      return [{ kind: "pause", ms: Math.round(seconds * 1000) }];
    }
  }

  const normalised = token
    .replace(/[^A-Za-z&]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .toUpperCase();

  switch (normalised) {
    case "DOWN":
      return [{ kind: "tone", value: "down" }];
    case "SLOW & LOW":
    case "SLOW AND LOW":
      return [
        { kind: "pace", value: "slow" },
        { kind: "tone", value: "down" },
      ];
    case "PACE UP":
      return [{ kind: "pace", value: "fast" }];
    case "CURIOUS":
      return [{ kind: "tone", value: "up" }];
    default:
      return null;
  }
}

function applyAnnotation(cue: Cue, annotation: Annotation): void {
  switch (annotation.kind) {
    case "tone":
      cue.tone = annotation.value;
      break;
    case "pace":
      cue.pace = annotation.value;
      break;
    case "pause":
      // The longest requested hold wins; two holds in one block are not additive.
      cue.pauseAfterMs = Math.max(cue.pauseAfterMs, annotation.ms);
      break;
  }
}

/**
 * Folds annotations left-to-right, so the last delivery instruction in a block wins.
 * This is deliberate: a block that opens `[🐢 SLOW & LOW]` and ends with a question
 * correctly finishes on an upward inflection.
 */
export function parseAnnotations(raw: string): {
  body: string;
  cue: Cue;
  annotations: Annotation[];
} {
  const cue: Cue = { tone: "neutral", pace: "normal", pauseAfterMs: 0 };
  const annotations: Annotation[] = [];
  let body = raw;

  for (const match of raw.matchAll(/\[([^\]]*)\]/g)) {
    const parsed = classifyToken(match[1]);
    if (!parsed) continue;
    for (const annotation of parsed) {
      annotations.push(annotation);
      applyAnnotation(cue, annotation);
    }
    body = body.replace(match[0], " ");
  }

  return { body: body.replace(/\s+/g, " ").trim(), cue, annotations };
}

type RawBlock = Omit<ScriptBlock, "body" | "cue" | "annotations">;
type RawScript = Omit<Script, "blocks"> & { blocks: RawBlock[] };

/** Parses every block and derives the delivery cue from its raw text. */
export function loadScript(raw: RawScript): Script {
  return {
    ...raw,
    blocks: raw.blocks.map((block) => {
      const { body, cue, annotations } = parseAnnotations(block.raw);
      return { ...block, body, cue, annotations };
    }),
  };
}
