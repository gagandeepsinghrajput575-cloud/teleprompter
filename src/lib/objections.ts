export type Objection = {
  index: number;
  label: string;
  rebuttal: string;
  cue: string;
};

/**
 * Pre-written objection battlecards. Press ⌃<index> during a call to surface one; it appears
 * beside the script and never moves the script position. These are authored, not generated.
 */
export const OBJECTIONS: Objection[] = [
  {
    index: 1,
    label: "Price",
    rebuttal:
      "Totally fair — and if it were only about cost I'd say no. The real question is what it's costing you to keep doing it by hand every week.",
    cue: "🐢 SLOW & LOW",
  },
  {
    index: 2,
    label: "Timing / not now",
    rebuttal:
      "I hear you, everyone's slammed. That's exactly why this is a 10-minute call, not a project — let's just see if it's worth a second one.",
    cue: "⬇️ DOWN",
  },
  {
    index: 3,
    label: "Trust / who are you",
    rebuttal:
      "Fair to be skeptical, you don't know me yet. I'm not asking for trust, just 10 minutes to show you — then you judge.",
    cue: "🤔 CURIOUS",
  },
  {
    index: 4,
    label: "Authority / ask partner",
    rebuttal:
      "Makes sense, you'd want your partner in this. Let's get the picture clear now so it's a 2-minute conversation when you bring it to them.",
    cue: "⬇️ DOWN",
  },
  {
    index: 5,
    label: "Need / why now",
    rebuttal:
      "You're right, it's not urgent today. But the hours you're losing every week are permanent — that's the real cost of waiting.",
    cue: "🐢 SLOW & LOW",
  },
  {
    index: 6,
    label: "Competitor",
    rebuttal:
      "Good, you've already got someone. I'm not here to replace them — I'm here to see if there's one gap they're not covering.",
    cue: "⬇️ DOWN",
  },
  {
    index: 7,
    label: "Send info",
    rebuttal:
      "Happy to — but a PDF won't know your business. Give me 10 minutes and I'll send something built for you instead.",
    cue: "⚡ PACE UP",
  },
  {
    index: 8,
    label: "Budget / approval",
    rebuttal:
      "Understood. Let's not talk price until we know it's worth it — if it doesn't save you more than it costs, you shouldn't buy it.",
    cue: "⬇️ DOWN",
  },
  {
    index: 9,
    label: "Too busy",
    rebuttal:
      "Totally get it. This is the one call that saves you time — 10 minutes now, or keep losing hours every week.",
    cue: "⚡ PACE UP",
  },
  {
    index: 0,
    label: "Not interested",
    rebuttal:
      "Fair enough. Can I ask what would have to be true for this to be worth 10 minutes? If it's a no, I'll leave you alone.",
    cue: "🤔 CURIOUS",
  },
];

export function objectionFor(index: number): Objection | undefined {
  return OBJECTIONS.find((o) => o.index === index);
}
