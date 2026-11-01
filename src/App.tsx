import { useCallback, useEffect, useState } from "react";
import { cueChips, type Cue } from "./lib/cues";

type Block = {
  id: number;
  label: string;
  body: string;
  cue: Cue;
};

/**
 * Placeholder data only. Real blocks come from the SQLite script store (T3).
 * These exist so the overlay shell can be validated before any AI is wired up.
 */
const DEMO_BLOCKS: Block[] = [
  {
    id: 1,
    label: "Frame",
    body: "Before we dive in — how are you handling new leads right now?",
    cue: { tone: "down", pace: "slow", pauseAfterMs: 1800 },
  },
  {
    id: 2,
    label: "Speed-to-Lead Leak",
    body: "Most roofers lose the job in the first five minutes, not on price.",
    cue: { tone: "down", pace: "slow", pauseAfterMs: 1200 },
  },
  {
    id: 3,
    label: "Evening Admin Bleed",
    body: "How many hours a week are you quoting after 7pm?",
    cue: { tone: "neutral", pace: "normal", pauseAfterMs: 1800 },
  },
  {
    id: 4,
    label: "Cost of Inaction",
    body: "Two missed jobs a month is your entire software budget, twice over.",
    cue: { tone: "down", pace: "slow", pauseAfterMs: 0 },
  },
];

export default function App() {
  const [activeId, setActiveId] = useState(1);

  const nudge = useCallback((delta: number) => {
    setActiveId((current) => {
      const next = current + delta;
      return next < 1 || next > DEMO_BLOCKS.length ? current : next;
    });
  }, []);

  // Local-only affordance for validating the shell. Global shortcuts land in T7.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") nudge(1);
      if (e.key === "ArrowUp") nudge(-1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [nudge]);

  const active = DEMO_BLOCKS.find((b) => b.id === activeId) ?? DEMO_BLOCKS[0];

  return (
    <div className="hud">
      {DEMO_BLOCKS.map((block) => (
        <div className="block" key={block.id} data-active={block.id === activeId}>
          <span className="label">{block.label}</span>
          {block.body}
          {block.id === activeId && (
            <div className="cues">
              {cueChips(block.cue).map((chip) => (
                <span className="cue" key={chip}>
                  {chip}
                </span>
              ))}
            </div>
          )}
        </div>
      ))}
      <div className="footer">
        <span>shell only · fake data</span>
        <span>
          {active.id}/{DEMO_BLOCKS.length}
        </span>
      </div>
    </div>
  );
}
