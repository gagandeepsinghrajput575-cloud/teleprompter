import { parseAnnotations, type Script, type ScriptLine } from "./script";

export type SavedScriptEntry = {
  id: string;
  name: string;
  savedAt: number;
  script: Script;
};

/**
 * Parses markdown or HTML script file content into a structured Script object.
 */
export function parseCustomScript(fileName: string, content: string): Script {
  const isHtml = fileName.toLowerCase().endsWith(".html") || /<[a-z][\s\S]*>/i.test(content);
  
  if (isHtml) {
    return parseHtmlScript(fileName, content);
  }
  return parseMarkdownScript(fileName, content);
}

function parseMarkdownScript(fileName: string, markdown: string): Script {
  const lines = markdown.split(/\r?\n/);
  const scriptLines: ScriptLine[] = [];
  
  let currentPhase = "General";
  let lineId = 1;
  let detectedTitle = fileName.replace(/\.[^/.]+$/, "");

  const addLine = (rawText: string) => {
    const raw = rawText.trim();
    if (!raw) return;

    // Detect label if format is "Label: text" or "**Label**: text"
    let label = `Step ${lineId}`;
    let textToParse = raw;
    const labelMatch = raw.match(/^(?:\*\*([^*]+)\*\*|([A-Za-z0-9\s_-]+)):\s*(.*)$/);
    if (labelMatch) {
      label = (labelMatch[1] || labelMatch[2]).trim();
      textToParse = labelMatch[3].trim();
    }

    const { body, cue, annotations } = parseAnnotations(textToParse);
    scriptLines.push({
      id: lineId++,
      phase: currentPhase,
      label,
      raw: textToParse,
      body,
      cue,
      annotations,
      branches: [],
    });
  };

  for (const rawLine of lines) {
    const trimmed = rawLine.trim();
    if (!trimmed) continue;

    // Markdown headers
    const headerMatch = trimmed.match(/^(#{1,6})\s+(.*)$/);
    if (headerMatch) {
      const level = headerMatch[1].length;
      const titleText = headerMatch[2].trim();
      if (level === 1 && lineId === 1) {
        detectedTitle = titleText;
      }
      currentPhase = titleText;
      continue;
    }

    // Bullet points / numbered lists
    const listMatch = trimmed.match(/^(?:[-*+]|\d+\.)\s+(.*)$/);
    if (listMatch) {
      addLine(listMatch[1]);
      continue;
    }

    addLine(trimmed);
  }

  if (scriptLines.length === 0) {
    const { body, cue, annotations } = parseAnnotations(fileName);
    scriptLines.push({
      id: 1,
      phase: "General",
      label: "Start",
      raw: fileName,
      body,
      cue,
      annotations,
    });
  }

  return {
    id: `custom-${Date.now()}`,
    title: detectedTitle,
    version: 1,
    lines: scriptLines,
  };
}

function parseHtmlScript(fileName: string, html: string): Script {
  let detectedTitle = fileName.replace(/\.[^/.]+$/, "");
  const scriptLines: ScriptLine[] = [];
  let lineId = 1;

  if (typeof DOMParser !== "undefined") {
    try {
      const parser = new DOMParser();
      const doc = parser.parseFromString(html, "text/html");
      const titleEl = doc.querySelector("title, h1");
      if (titleEl && titleEl.textContent) {
        detectedTitle = titleEl.textContent.trim();
      }

      let currentPhase = "General";
      const blockElements = doc.querySelectorAll("h1, h2, h3, h4, p, li, blockquote, div.step");
      
      blockElements.forEach((el) => {
        const tag = el.tagName.toLowerCase();
        const text = (el.textContent || "").trim();
        if (!text) return;

        if (tag.startsWith("h")) {
          currentPhase = text;
        } else {
          let label = `Step ${lineId}`;
          let raw = text;
          const labelMatch = text.match(/^([A-Za-z0-9\s_-]+):\s*(.*)$/);
          if (labelMatch) {
            label = labelMatch[1].trim();
            raw = labelMatch[2].trim();
          }

          const { body, cue, annotations } = parseAnnotations(raw);
          scriptLines.push({
            id: lineId++,
            phase: currentPhase,
            label,
            raw,
            body,
            cue,
            annotations,
            branches: [],
          });
        }
      });
    } catch {
      // DOMParser fallback
    }
  }

  if (scriptLines.length === 0) {
    const titleMatch = html.match(/<title[^>]*>([^<]+)<\/title>/i) || html.match(/<h1[^>]*>([^<]+)<\/h1>/i);
    if (titleMatch) {
      detectedTitle = titleMatch[1].trim();
    }

    let currentPhase = "General";
    const tagRegex = /<(h[1-4]|p|li)[^>]*>([\s\S]*?)<\/\1>/gi;
    let match: RegExpExecArray | null;

    while ((match = tagRegex.exec(html)) !== null) {
      const tag = match[1].toLowerCase();
      const rawText = match[2].replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
      if (!rawText) continue;

      if (tag.startsWith("h")) {
        currentPhase = rawText;
      } else {
        let label = `Step ${lineId}`;
        let raw = rawText;
        const labelMatch = rawText.match(/^([A-Za-z0-9\s_-]+):\s*(.*)$/);
        if (labelMatch) {
          label = labelMatch[1].trim();
          raw = labelMatch[2].trim();
        }

        const { body, cue, annotations } = parseAnnotations(raw);
        scriptLines.push({
          id: lineId++,
          phase: currentPhase,
          label,
          raw,
          body,
          cue,
          annotations,
          branches: [],
        });
      }
    }
  }

  if (scriptLines.length === 0) {
    const stripped = html.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim() || fileName;
    const { body, cue, annotations } = parseAnnotations(stripped);
    scriptLines.push({
      id: 1,
      phase: "General",
      label: "Start",
      raw: stripped,
      body,
      cue,
      annotations,
    });
  }

  return {
    id: `custom-${Date.now()}`,
    title: detectedTitle,
    version: 1,
    lines: scriptLines,
  };
}

const STORAGE_KEY = "tp.savedCustomScripts";

function getStorageSafe(): Storage | null {
  if (typeof localStorage !== "undefined" && localStorage) return localStorage;
  if (typeof window !== "undefined" && window.localStorage) return window.localStorage;
  return null;
}

export function getSavedScripts(): SavedScriptEntry[] {
  const storage = getStorageSafe();
  if (!storage) return [];
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return [];
    return JSON.parse(raw);
  } catch {
    return [];
  }
}

export function saveCustomScript(name: string, script: Script): SavedScriptEntry[] {
  const storage = getStorageSafe();
  if (!storage) return [];
  try {
    const existing = getSavedScripts().filter((s) => s.id !== script.id && s.name !== name);
    const entry: SavedScriptEntry = {
      id: script.id,
      name,
      savedAt: Date.now(),
      script,
    };
    const updated = [entry, ...existing];
    storage.setItem(STORAGE_KEY, JSON.stringify(updated));
    return updated;
  } catch {
    return [];
  }
}

export function deleteSavedScript(id: string): SavedScriptEntry[] {
  const storage = getStorageSafe();
  if (!storage) return [];
  try {
    const existing = getSavedScripts().filter((s) => s.id !== id);
    storage.setItem(STORAGE_KEY, JSON.stringify(existing));
    return existing;
  } catch {
    return [];
  }
}
