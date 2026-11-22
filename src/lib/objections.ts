export type Objection = {
  index: number;
  label: string;
  rebuttal: string;
  cue: string;
};

/**
 * The side branches (objection boxes) from the Scalix Elite live cold call flowchart, in the order the
 * flowchart presents them. Press Ctrl+<index> or bare <index> during a call to surface one; the centre-line script
 * is never moved.
 */
export const COLD_CALL_OBJECTIONS: Objection[] = [
  {
    index: 1,
    label: "Price / Why is it free?",
    rebuttal:
      "Because we know 80% of established firms will adopt AI automation in the next 8 to 12 months. So we are building relationships and giving upfront value now, so if or when the time comes that you do want to plug those leaks, you've already got us as a trusted contact. Sounds fair?",
    cue: "🐢",
  },
  {
    index: 2,
    label: "Not interested / We're good",
    rebuttal:
      "I'm not surprised, [Name]—I haven't got to the good bit yet. Give me 60 seconds to explain why I called, and if you still think it's useless for your business after that, tell me no and I'll hang up myself. Fair enough?",
    cue: "⚡",
  },
  {
    index: 4,
    label: "Where did you get my number?",
    rebuttal:
      "Our research team pulls numbers from public Danish registries. But we're very exclusive in the criteria we look for. We only reach out to established firms with roughly [team size] staff and a solid local reputation—which I'm assuming applies to you guys?",
    cue: "🐢",
  },
  {
    index: 5,
    label: "Send me an email",
    rebuttal:
      "For sure, I can email you. But the reason we do the 10-minute look first is we only implement AI in a business if it can actually generate meaningful results—and the missed-call setup I told you about is only one of the things. So I'm not sure what to send you on email without it being irrelevant to you, and that's why we have that 10-minute call first—so we can see all the possible gaps in the business. So what do you say—should we grab 10 minutes tomorrow afternoon, or is Thursday morning better?",
    cue: "⚡",
  },
  {
    index: 7,
    label: "We don't use AI / old-school",
    rebuttal:
      "Well, using AI sounds unnecessary and fancy until you actually see how much evening time it saves you per week, or how it directly stops high-ticket calls from slipping to competitors. See, the thing is, there could be a lot to go over—and that's the exact reason we do the 10-minute review, so we can see all the possible gaps in your business without you committing to anything. Does tomorrow afternoon or Thursday morning work better to at least see what's possible in your case?",
    cue: "🐢",
  },
  {
    index: 9,
    label: "Audit requirement",
    rebuttal:
      "Yeah, so the audit basically requires just 10 to 15 minutes from you, where we understand at what places in your business you can leverage AI—either saving you hours per week, or directly adding more revenue to your business. Super simple. Is tomorrow or Thursday better for you?",
    cue: "🐢",
  },
  {
    index: 0,
    label: "Booked today / Lock for Monday",
    rebuttal:
      "I'm already booked back-to-back with a couple other contractors for the rest of today, so let's lock it in for Monday. Is 12:30 or 15:30 easier for you?",
    cue: "⚡",
  },
];

/**
 * Archived / deleted objection indices for cold calling (retained for tracking and recovery).
 * Pinned identically between Rust and TypeScript.
 */
export const COLD_DELETED_INDICES = [3, 6, 8] as const;

export const COLD_CALL_DELETED_OBJECTIONS: Objection[] = [
  {
    index: 3,
    label: "Don't take cold calls",
    rebuttal:
      "Neither do I, [Name], if I can possibly avoid it. But the problem is: your existing software or web guys aren't telling you about this, and you aren't going to go looking for it on your own because you don't know it exists yet. So looking past the phone call for 60 seconds—how about I quickly state the reason I called?",
    cue: "🐢",
  },
  {
    index: 6,
    label: "We already have a web/tech guy",
    rebuttal:
      "Good to hear. What are they handling right now—just the website, or site call intake too? Got it. We don't replace your web guy. And since the 10-minute look is free, it costs you nothing—after 10 minutes, you'll have a crystal-clear picture of whether there's a leak or if your current setup is airtight. Is tomorrow afternoon or Thursday morning better?",
    cue: "🤔",
  },
  {
    index: 8,
    label: "Booked out / plenty of work",
    rebuttal:
      "The issue when you're slammed isn't getting more work—it's missing out on cherry-picking the highest-margin 200,000 DKK jobs while wasting your evenings quoting tire-kickers. That's why we should connect for 10 minutes so you can get your evenings back—is tomorrow or Thursday better?",
    cue: "🐢",
  },
  {
    index: 9,
    label: "Busy / on site / driving",
    rebuttal:
      "Totally get it, [Name]—I can hear you're busy on site. I won't waste your site time. I can give you a quick ring back at 15:30 when you're off tools, or is tomorrow morning around 08:30 better?",
    cue: "🐢",
  },
  {
    index: 0,
    label: "Employee on site / Gatekeeper",
    rebuttal:
      "Alright, I'm trying to reach [Owner's First Name]'s direct mobile regarding an operations review, and I think I dialed the site line by mistake. What's the best phone number to catch him on directly?\n\nIf they ask: 'What is this regarding?'\n⚡ It's Gagan from Scalix. It's regarding the operations review for [Owner's First Name].\n\nIf they ask: 'Is he expecting the call?'\n🐢 Not personally—his firm was placed on our research shortlist for this quarter's reviews. When is he naturally less busy?",
    cue: "🐢",
  },
];

/**
 * Invariant closing objection deck for sales and closing calls (Call 1 Audit, Call 2 Close, Custom Pitches).
 * Keys 1-9, 0 are invariant archetypes.
 */
export const SALES_CALL_OBJECTIONS: Objection[] = [
  {
    index: 1,
    label: "Budget / Cash Flow Tight",
    rebuttal:
      "Totally understand cash flow is king in any business, [Name]. That's exactly why we don't bill full retainers until the pipeline is flowing. If we broke this into two staged tranches so the first phase funds itself from the first couple of saved jobs, would that work better for you?",
    cue: "🐢",
  },
  {
    index: 2,
    label: "Burned by Agency / Skeptical",
    rebuttal:
      "I hear that all the time, [Name], and honestly you should be skeptical—most agencies sell vanity metrics and disappear. That's why we don't ask you to trust our word. We build the system live, connect it directly to your numbers, and if it doesn't perform to the metric we agreed on, you don't pay. What did that previous agency do that you never want to see repeated?",
    cue: "⚡",
  },
  {
    index: 3,
    label: "Need to Consult Partner / Co-founder",
    rebuttal:
      "Makes total sense—I wouldn't expect you to make a major operational decision without your partner on board. Let's do this: what is the single biggest question or concern you think they'll have when you bring this up to them tonight?",
    cue: "🤔",
  },
  {
    index: 4,
    label: "Timing / Revisit Next Quarter",
    rebuttal:
      "I hear you on timing, [Name]. But here's the reality: next quarter you'll be just as busy, except another 30 to 50 inbound calls will have leaked to competitors. The setup takes less than 90 minutes of your time total—we handle the entire heavy lifting. Why not plug the leak now so next quarter's revenue is already protected?",
    cue: "⚡",
  },
  {
    index: 5,
    label: "Send Proposal / Contract to Review",
    rebuttal:
      "I can definitely send the agreement over right after we hang up. But proposals usually end up buried in an inbox. Before I send it, based on everything we walked through today—is there anything in the scope or pricing that would stop you from moving forward?",
    cue: "🐢",
  },
  {
    index: 6,
    label: "In-House / Can We Do This Ourselves?",
    rebuttal:
      "You definitely could have someone in-house try to piece this together. But between API maintenance, Danish telephony latency, and fine-tuning prompt rules, your team will spend 60+ hours just debugging edge cases instead of doing billable work. Is that the highest and best use of your staff's hours?",
    cue: "🤔",
  },
  {
    index: 7,
    label: "Setup Time / How Much Work For Me?",
    rebuttal:
      "Zero operational disruption, [Name]. Your total commitment is a 45-minute onboarding call where you hand us access, and a 20-minute sign-off review before we switch it live. We do 100% of the build, testing, and telephony integration ourselves.",
    cue: "🐢",
  },
  {
    index: 8,
    label: "Niche Fit / Will It Work For Us?",
    rebuttal:
      "Fair question. That's why we specifically built custom logic for residential contractors and service firms, rather than generic e-commerce bots. We already tuned the vocabulary, Danish numbers, and response speeds specifically for your exact workflow. Let me show you how it handles a real call from your niche.",
    cue: "⚡",
  },
  {
    index: 9,
    label: "Guarantee / Proof / Case Studies",
    rebuttal:
      "We back this with a written performance benchmark: if the system doesn't capture and route qualified calls within the agreed SLA in the first 30 days, we work for free until it does, or refund your setup fee in full. We take all the downside risk so you don't have to.",
    cue: "🐢",
  },
  {
    index: 0,
    label: "Contract Lock-in / Commitment",
    rebuttal:
      "There is no long-term lock-in, [Name]. We work on simple month-to-month terms. If we're not making you substantially more money than you're paying us, you should cancel. We have to earn your business every single 30 days.",
    cue: "⚡",
  },
];

export const OBJECTIONS: Objection[] = COLD_CALL_OBJECTIONS;

export function getObjectionsForCall(
  callType?: string | null,
  customDeck?: Objection[] | null
): Objection[] {
  if (customDeck && customDeck.length > 0) {
    // Fill any missing slots with sales defaults so slots 1-9, 0 are invariant
    return Array.from({ length: 10 }, (_, i) => {
      const idx = i === 9 ? 0 : i + 1;
      const custom = customDeck.find((o) => o.index === idx);
      if (custom) return custom;
      const fallback =
        SALES_CALL_OBJECTIONS.find((o) => o.index === idx) ||
        COLD_CALL_OBJECTIONS.find((o) => o.index === idx) ||
        COLD_CALL_DELETED_OBJECTIONS.find((o) => o.index === idx);
      return (
        fallback || {
          index: idx,
          label: `Objection ${idx}`,
          rebuttal: "",
          cue: "🐢",
        }
      );
    });
  }
  if (callType === "call1" || callType === "call2" || callType === "sales") {
    return SALES_CALL_OBJECTIONS;
  }
  return COLD_CALL_OBJECTIONS;
}

export function objectionFor(
  index: number,
  callType?: string | null,
  customDeck?: Objection[] | null
): Objection | undefined {
  const deck = getObjectionsForCall(callType, customDeck);
  return deck.find((o) => o.index === index);
}
