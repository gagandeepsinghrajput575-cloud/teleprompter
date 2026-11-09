import type { Cue, Pace, Tone } from "./cues";

/**
 * A delivery annotation lifted out of the script text, e.g. `[⬇️ DOWN]`, `[🐢 SLOW & LOW]`,
 * `[⚡ PACE UP]`, `[🤔 CURIOUS]`, `[⏸️ 1.5s]`.
 */
export type Annotation =
  | { kind: "tone"; value: Exclude<Tone, "neutral"> }
  | { kind: "pace"; value: Exclude<Pace, "normal"> }
  | { kind: "pause"; ms: number };

export type ScriptLine = {
  id: number;
  phase: string;
  label: string;
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
  lines: ScriptLine[];
};

/**
 * Classifies one bracketed token. Returns null for non-annotation brackets such as the
 * `[Name]` placeholder, which must survive into the spoken text.
 */
export function classifyToken(token: string): Annotation[] | null {
  const pauseMatch = token.match(/([0-9]*\.?[0-9]+)\s*s\b/i);
  if (pauseMatch) {
    const seconds = Number.parseFloat(pauseMatch[1]);
    if (Number.isFinite(seconds)) {
      return [{ kind: "pause", ms: Math.round(seconds * 1000) }];
    }
  } else if (/PAUSE/i.test(token) || token.includes("⏸")) {
    return [{ kind: "pause", ms: 3000 }];
  }

  const annotations: Annotation[] = [];

  // Check emoji markers first (with or without descriptive words)
  if (token.includes("🐢")) {
    annotations.push({ kind: "pace", value: "slow" }, { kind: "tone", value: "down" });
  } else if (token.includes("⚡")) {
    annotations.push({ kind: "pace", value: "fast" });
  }

  if (token.includes("⬇")) {
    if (!annotations.some((a) => a.kind === "tone")) {
      annotations.push({ kind: "tone", value: "down" });
    }
  } else if (token.includes("🤔")) {
    annotations.push({ kind: "tone", value: "up" });
  }

  if (annotations.length > 0) {
    return annotations;
  }

  const normalised = token
    .replace(/[^A-Za-z&]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .toUpperCase();

  if (normalised.includes("SLOW & LOW") || normalised.includes("SLOW AND LOW")) {
    annotations.push({ kind: "pace", value: "slow" }, { kind: "tone", value: "down" });
  } else if (normalised.includes("PACE UP")) {
    annotations.push({ kind: "pace", value: "fast" });
  }

  if (normalised.includes("DOWN") && !normalised.includes("SLOW")) {
    annotations.push({ kind: "tone", value: "down" });
  } else if (normalised.includes("CURIOUS")) {
    annotations.push({ kind: "tone", value: "up" });
  }

  return annotations.length > 0 ? annotations : null;
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

  // Strip any loose cue emojis that may exist outside brackets
  body = body.replace(/[\u2B07\uFE0F\u2B07\u{1F914}\u{1F422}\u26A1\u23F8\uFE0F\u23F8]/gu, " ");

  return { body: body.replace(/\s+/g, " ").trim(), cue, annotations };
}

type RawLine = Omit<ScriptLine, "body" | "cue" | "annotations">;
type RawScript = Omit<Script, "lines"> & { lines: RawLine[] };

/** Parses every line and derives the delivery cue from its raw text. */
export function loadScript(raw: RawScript): Script {
  return {
    ...raw,
    lines: raw.lines.map((line) => {
      const { body, cue, annotations } = parseAnnotations(line.raw);
      return {
        id: line.id,
        phase: line.phase,
        label: line.label,
        raw: line.raw,
        body,
        cue,
        annotations,
      };
    }),
  };
}
