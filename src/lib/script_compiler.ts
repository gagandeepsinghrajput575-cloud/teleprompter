import type { Script, ScriptLine } from "./script";
import { parseAnnotations } from "./script";
import type { Objection } from "./objections";
import { SALES_CALL_OBJECTIONS } from "./objections";

/**
 * Sanitizes raw HTML or Markdown:
 * - Strips tags, tables, styling, scripts, spans, and web entities
 * - Preserves structural paragraphs and line boundaries
 */
export function sanitizeRawContent(content: string): string {
  let text = content;

  // If HTML, strip script/style tags and their contents
  text = text.replace(/<script\b[^<]*(?:(?!<\/script>)<[^<]*)*<\/script>/gi, "");
  text = text.replace(/<style\b[^<]*(?:(?!<\/style>)<[^<]*)*<\/style>/gi, "");

  // Convert break and paragraph tags to linebreaks
  text = text.replace(/<(br|p|div|tr|h[1-6])\b[^>]*>/gi, "\n");
  text = text.replace(/<\/(p|div|tr|h[1-6])>/gi, "\n");

  // Strip remaining HTML tags
  text = text.replace(/<[^>]+>/g, " ");

  // Decode common HTML entities
  text = text
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/gi, "'")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">");

  // Normalize multi-newlines and whitespace
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .join("\n");
}

/**
 * Deterministic local script compiler:
 * Converts sanitized Markdown or HTML text into a structured StealthPrompter Script & Objections.
 * Enforces the strict Two-Track Separation:
 * Track 1: Linear presentation lines (Spine)
 * Track 2: Objections, pushbacks, FAQ mapped to invariant slots 1-9, 0 (Deck)
 */
export function compileScriptDeterministically(
  rawContent: string,
  docTitle = "Custom Sales Script"
): { script: Script; objections: Objection[] } {
  const clean = sanitizeRawContent(rawContent);
  const lines = clean.split("\n");

  let currentPhase = "PHASE 1: OPENING & FRAME";
  const spineLines: ScriptLine[] = [];
  const extractedObjections: Partial<Objection>[] = [];

  let isObjectionSection = false;
  let currentObjCategory: string | null = null;
  let currentObjText: string[] = [];

  let nextLineId = 1;

  for (let i = 0; i < lines.length; i++) {
    const rawLine = lines[i].trim();
    if (!rawLine) continue;

    // Detect section headers / phases
    const headerMatch =
      rawLine.match(/^#{1,4}\s*(.+)$/) ||
      rawLine.match(/^(phase\s*\d+|turn\s*\d+|part\s*\d+|section\s*\d+):?\s*(.+)$/i);
    if (headerMatch) {
      if (currentObjCategory && currentObjText.length > 0) {
        extractedObjections.push({
          label: currentObjCategory,
          rebuttal: currentObjText.join(" "),
        });
        currentObjCategory = null;
        currentObjText = [];
      }
      const headerText = (headerMatch[1] || headerMatch[2] || rawLine).toUpperCase();
      if (
        headerText.includes("OBJECTION") ||
        headerText.includes("PUSHBACK") ||
        headerText.includes("FAQ") ||
        headerText.includes("IF THEY SAY")
      ) {
        isObjectionSection = true;
      } else {
        isObjectionSection = false;
        currentPhase = headerText;
      }
      continue;
    }

    // Detect inline conditional objection markers: "If they say X:", "Q:", "Objection:"
    const lower = rawLine.toLowerCase();
    const isObjMarker =
      lower.startsWith("if they say") ||
      lower.startsWith("if prospect says") ||
      lower.startsWith("objection") ||
      lower.startsWith("pushback") ||
      lower.startsWith("q:");

    if (isObjMarker) {
      if (currentObjCategory && currentObjText.length > 0) {
        extractedObjections.push({
          label: currentObjCategory,
          rebuttal: currentObjText.join(" "),
        });
        currentObjText = [];
      }
      // Extract category after prefix
      const cleanedPrefix = rawLine
        .replace(/^(if they say|if prospect says|objection|pushback|q:?)\s*[:\-]?\s*["']?/i, "")
        .replace(/["']?:?\s*$/, "")
        .trim();
      currentObjCategory = cleanedPrefix || "General Pushback";
      continue;
    }

    if (currentObjCategory) {
      // If we are currently collecting an objection rebuttal, lines continue into it
      // unless we hit a new section header or empty break
      if (rawLine.startsWith("#") || rawLine.toLowerCase().startsWith("phase")) {
        extractedObjections.push({
          label: currentObjCategory,
          rebuttal: currentObjText.join(" "),
        });
        currentObjCategory = null;
        currentObjText = [];
      } else {
        currentObjText.push(rawLine);
        continue;
      }
    }

    if (isObjectionSection) {
      // Everything in an objection section belongs to objections
      continue;
    }

    // Process center spine line
    // Strip bullet markers (- , * , 1. )
    let body = rawLine.replace(/^[-*•]\s+/, "").replace(/^\d+\.\s+/, "").trim();
    if (!body || body.length < 3) continue;

    // Insert natural rhythm pause clauses ⏸ on long compound sentences (>14 words)
    if (!body.includes("⏸") && body.split(" ").length > 14) {
      if (/([.?!,])\s+/.test(body)) {
        body = body.replace(/([.?!,])\s+/g, "$1 ⏸️ ");
      } else {
        // Insert pause before conjunctions
        body = body.replace(/\b(and|because|which|so that|while|across)\b/i, "⏸️ $1");
      }
    }

    const { body: stripped, cue, annotations } = parseAnnotations(body);

    spineLines.push({
      id: nextLineId++,
      phase: currentPhase,
      label: body.slice(0, 32).replace(/\[.*?\]/g, "").trim() || `Turn ${nextLineId}`,
      raw: body,
      body: stripped,
      cue,
      annotations,
      branches: [],
    });
  }

  // Flush remaining objection
  if (currentObjCategory && currentObjText.length > 0) {
    extractedObjections.push({
      label: currentObjCategory,
      rebuttal: currentObjText.join(" "),
    });
  }

  // Map extracted objections into invariant slots 1-9, 0, filling missing slots from SALES_CALL_OBJECTIONS
  const finalObjections: Objection[] = SALES_CALL_OBJECTIONS.map((def) => {
    // Check if any extracted objection matches this slot's archetype
    const match = extractedObjections.find((e) => {
      const lbl = (e.label || "").toLowerCase();
      if (def.index === 1 && (lbl.includes("price") || lbl.includes("budget") || lbl.includes("cash") || lbl.includes("cost"))) return true;
      if (def.index === 2 && (lbl.includes("burned") || lbl.includes("agency") || lbl.includes("skeptic") || lbl.includes("trust"))) return true;
      if (def.index === 3 && (lbl.includes("partner") || lbl.includes("wife") || lbl.includes("founder") || lbl.includes("board"))) return true;
      if (def.index === 4 && (lbl.includes("timing") || lbl.includes("quarter") || lbl.includes("busy") || lbl.includes("summer"))) return true;
      if (def.index === 5 && (lbl.includes("proposal") || lbl.includes("contract") || lbl.includes("email") || lbl.includes("writing"))) return true;
      if (def.index === 6 && (lbl.includes("in-house") || lbl.includes("inhouse") || lbl.includes("diy") || lbl.includes("ourselves"))) return true;
      if (def.index === 7 && (lbl.includes("setup") || lbl.includes("time") || lbl.includes("effort") || lbl.includes("hours"))) return true;
      if (def.index === 8 && (lbl.includes("niche") || lbl.includes("fit") || lbl.includes("unique") || lbl.includes("industry"))) return true;
      if (def.index === 9 && (lbl.includes("guarantee") || lbl.includes("proof") || lbl.includes("case") || lbl.includes("results"))) return true;
      if (def.index === 0 && (lbl.includes("term") || lbl.includes("lock") || lbl.includes("month") || lbl.includes("commit"))) return true;
      return false;
    });

    if (match && match.rebuttal) {
      return {
        index: def.index,
        label: match.label || def.label,
        rebuttal: match.rebuttal,
        cue: def.cue,
      };
    }
    return def;
  });

  const script: Script = {
    id: `custom-${Date.now()}`,
    title: docTitle,
    version: 1,
    lines: spineLines,
  };

  return { script, objections: finalObjections };
}

/**
 * DeepSeek AI Script Compiler:
 * Uses DeepSeek Flash at high speed to structure messy HTML/Markdown into
 * the exact StealthPrompter runtime JSON schema with strict Two-Track Separation.
 */
export async function compileWithDeepSeek(
  rawContent: string,
  apiKey: string,
  docTitle = "Custom Sales Script"
): Promise<{ script: Script; objections: Objection[] }> {
  const sanitized = sanitizeRawContent(rawContent);

  const prompt = `You are an elite sales engineering compiler.
Take the following raw sales script and convert it into a structured StealthPrompter JSON format.

CRITICAL ARCHITECTURAL RULES:
1. STRICT TWO-TRACK SEPARATION:
   - Track 1 (the 'center' array): ONLY pure linear presentation lines spoken by the rep (Opening, Agenda, Discovery, Pitch, Prototype demo, Close).
   - NEVER place conditional branches, FAQs, pushbacks, or "if they say X" lines into 'center'.
2. THE OBJECTION DECK (slots 1-9, and 0):
   - Extract all objections and pushbacks into 'objections'.
   - Invariant slot mapping:
     1: Budget / Cash flow
     2: Burned before by agency / trust
     3: Consult partner / co-founder
     4: Timing / revisit next quarter
     5: Send proposal / contract
     6: In-house / DIY
     7: Setup time / effort
     8: Niche fit / scope
     9: Guarantee / case study proof
     0: Contract lock-in / terms
   - If an objection is missing in the text, synthesize a high-converting rebuttal tailored to the offer in the script.
3. RHYTHM PAUSES:
   - Insert '⏸️' on compound sentences (>15 words) for natural breath pauses so STT doesn't stall.
4. CUE STRIPPING:
   - Place stage directions like [SHARE SCREEN], (wait for nod) in cue tags, keep spoken body clean.

Return STRICT JSON matching:
{
  "title": "${docTitle}",
  "lines": [
    {
      "id": 1,
      "phase": "PHASE 1: SET THE FRAME",
      "label": "Short 2-4 word summary",
      "raw": "The full spoken text with [cues] and ⏸️ pauses"
    }
  ],
  "objections": [
    {
      "index": 1,
      "label": "Budget / Cash flow",
      "rebuttal": "Rebuttal text",
      "cue": "🐢"
    }
  ]
}

RAW SCRIPT:
${sanitized.slice(0, 14000)}
`;

  try {
    const response = await fetch("https://api.deepseek.com/v1/chat/completions", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${apiKey}`,
      },
      body: JSON.stringify({
        model: "deepseek-chat",
        messages: [{ role: "user", content: prompt }],
        response_format: { type: "json_object" },
        temperature: 0.2,
      }),
    });

    if (!response.ok) {
      throw new Error(`DeepSeek API error: ${response.status} ${response.statusText}`);
    }

    const data = await response.json();
    const contentText = data.choices?.[0]?.message?.content;
    const parsed = JSON.parse(contentText);

    const lines: ScriptLine[] = (parsed.lines || []).map((l: any, idx: number) => {
      const { body, cue, annotations } = parseAnnotations(l.raw || "");
      return {
        id: l.id ?? idx + 1,
        phase: l.phase || "PHASE 1: PRESENTATION",
        label: l.label || `Step ${idx + 1}`,
        raw: l.raw || "",
        body,
        cue,
        annotations,
        branches: [],
      };
    });

    // Merge objections with invariant slots 1-9, 0
    const objList: Objection[] = SALES_CALL_OBJECTIONS.map((def) => {
      const found = (parsed.objections || []).find((o: any) => o.index === def.index);
      if (found && found.rebuttal) {
        return {
          index: def.index,
          label: found.label || def.label,
          rebuttal: found.rebuttal,
          cue: found.cue || def.cue,
        };
      }
      return def;
    });

    const script: Script = {
      id: `deepseek-${Date.now()}`,
      title: parsed.title || docTitle,
      version: 1,
      lines,
    };

    return { script, objections: objList };
  } catch (err) {
    console.warn("[DeepSeek compile fallback to deterministic parser]", err);
    return compileScriptDeterministically(rawContent, docTitle);
  }
}

const STORAGE_CUSTOM_KEY = "tp.saved_custom_scripts";

export type SavedScriptEntry = {
  id: string;
  title: string;
  script: Script;
  objections: Objection[];
  savedAt: number;
};

export function getSavedScripts(): SavedScriptEntry[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = localStorage.getItem(STORAGE_CUSTOM_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
}

export function saveCustomScript(
  script: Script,
  objections: Objection[]
): void {
  if (typeof window === "undefined") return;
  try {
    const existing = getSavedScripts();
    const filtered = existing.filter((e) => e.title !== script.title);
    filtered.unshift({
      id: script.id,
      title: script.title,
      script,
      objections,
      savedAt: Date.now(),
    });
    localStorage.setItem(STORAGE_CUSTOM_KEY, JSON.stringify(filtered.slice(0, 10)));
  } catch (err) {
    console.error("[saveCustomScript error]", err);
  }
}
