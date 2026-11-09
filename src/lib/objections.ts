export type Objection = {
  index: number;
  label: string;
  rebuttal: string;
  cue: string;
};

/**
 * The side branches (objection boxes) from the Scalix Elite live call flowchart, in the order the
 * flowchart presents them. Press Ctrl+<index> during a call to surface one; the centre-line script
 * is never moved. These are the exact authored rebuttals, not generated text.
 */
export const OBJECTIONS: Objection[] = [
  {
    index: 1,
    label: "Price / Why is it free?",
    rebuttal:
      "Because we know 80% of established firms will adopt AI automation in the next 8 to 12 months. So we are building relationships and giving upfront value now, so if or when the time comes that you do want to plug those leaks, you've already got us as a trusted contact. Sounds fair?",
    cue: "🐢 SLOW & LOW",
  },
  {
    index: 2,
    label: "Not interested / We're good",
    rebuttal:
      "I'm not surprised, [Name]—I haven't got to the good bit yet. Give me 60 seconds to explain why I called, and if you still think it's useless for your business after that, tell me no and I'll hang up myself. Fair enough?",
    cue: "⚡ PACE UP",
  },
  {
    index: 3,
    label: "Don't take cold calls",
    rebuttal:
      "Neither do I, [Name], if I can possibly avoid it. But the problem is: your existing software or web guys aren't telling you about this, and you aren't going to go looking for it on your own because you don't know it exists yet. So looking past the phone call for 60 seconds—how about I quickly state the reason I called?",
    cue: "🐢 SLOW & LOW",
  },
  {
    index: 4,
    label: "Where did you get my number?",
    rebuttal:
      "Our research team pulls numbers from public Danish registries. But we're very exclusive in the criteria we look for. We only reach out to established firms with roughly 4 to 8 staff and a solid local reputation—which I'm assuming applies to you guys?",
    cue: "🐢 SLOW & LOW",
  },
  {
    index: 5,
    label: "Send me an email",
    rebuttal:
      "For sure, I can email you. But the reason we do the 10-minute look first is we only implement AI in a business if it can actually generate meaningful results—and the missed-call setup I told you about is only one of the things. So I'm not sure what to send you on email without it being irrelevant to you, and that's why we have that 10-minute call first—so we can see all the possible gaps in the business. So what do you say—should we grab 10 minutes tomorrow afternoon, or is Thursday morning better?",
    cue: "⚡ PACE UP",
  },
  {
    index: 6,
    label: "We already have a web/tech guy",
    rebuttal:
      "Good to hear. What are they handling right now—just the website, or site call intake too? Got it. We don't replace your web guy. And since the 10-minute look is free, it costs you nothing—after 10 minutes, you'll have a crystal-clear picture of whether there's a leak or if your current setup is airtight. Is tomorrow afternoon or Thursday morning better?",
    cue: "🤔 CURIOUS",
  },
  {
    index: 7,
    label: "We don't use AI / old-school",
    rebuttal:
      "Well, using AI sounds unnecessary and fancy until you actually see how much evening time it saves you per week, or how it directly stops high-ticket calls from slipping to competitors. See, the thing is, there could be a lot to go over—and that's the exact reason we do the 10-minute review, so we can see all the possible gaps in your business without you committing to anything. Does tomorrow afternoon or Thursday morning work better to at least see what's possible in your case?",
    cue: "🐢 SLOW & LOW",
  },
  {
    index: 8,
    label: "Booked out / plenty of work",
    rebuttal:
      "The issue when you're slammed isn't getting more work—it's missing out on cherry-picking the highest-margin 200,000 DKK jobs while wasting your evenings quoting tire-kickers. That's why we should connect for 10 minutes so you can get your evenings back—is tomorrow or Thursday better?",
    cue: "🐢 SLOW & LOW",
  },
  {
    index: 9,
    label: "Busy / on site / driving",
    rebuttal:
      "Totally get it, [Name]—I can hear you're busy on site. I won't waste your site time. I can give you a quick ring back at 15:30 when you're off tools, or is tomorrow morning around 08:30 better?",
    cue: "🐢 SLOW & LOW",
  },
  {
    index: 0,
    label: "Employee on site / not the owner",
    rebuttal:
      "Alright, I'm trying to reach [Name]'s direct mobile regarding an operations review, and I think I dialed the site line by mistake. What's the best phone number to catch him on directly?",
    cue: "🐢 SLOW & LOW",
  },
];

export function objectionFor(index: number): Objection | undefined {
  return OBJECTIONS.find((o) => o.index === index);
}
