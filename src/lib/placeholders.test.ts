import { describe, expect, it } from "vitest";
import { fillName, needsName } from "./placeholders";

describe("the dialer's name placeholders", () => {
  it("fills both brackets with the one name the dialer sends", () => {
    expect(fillName("[Prospect Name], hi. It's Gagan from Scalix Elite.", "Mike")).toBe(
      "Mike, hi. It's Gagan from Scalix Elite.",
    );
    expect(fillName("I'm trying to reach [Owner's First Name]'s direct mobile", "Mike")).toBe(
      "I'm trying to reach Mike's direct mobile",
    );
  });

  it("ignores case and stray spacing", () => {
    expect(fillName("[prospect  name] hi", "Mike")).toBe("Mike hi");
    expect(fillName("[ OWNER\u2019S FIRST NAME ] hi", "Mike")).toBe("Mike hi");
  });

  it("leaves the rep's own bracketed instructions alone", () => {
    const copy =
      "Got it\u2014that's [spell back email address slowly], right? I'm sending it now.";
    expect(fillName(copy, "Mike")).toBe(copy);
  });

  it("changes nothing when no name has arrived", () => {
    const copy = "[Prospect Name], hi.";
    expect(fillName(copy, null)).toBe(copy);
    expect(fillName(copy, undefined)).toBe(copy);
    expect(fillName(copy, "   ")).toBe(copy);
  });

  it("inserts a name literally, even one containing replacement syntax", () => {
    expect(fillName("[Prospect Name] hi", "A$&B")).toBe("A$&B hi");
    expect(fillName("[Prospect Name] hi", "$1")).toBe("$1 hi");
  });

  it("fills every occurrence", () => {
    expect(fillName("[Prospect Name] and [Prospect Name]", "Mike")).toBe("Mike and Mike");
  });

  it("reports whether a name is still missing", () => {
    expect(needsName("[Prospect Name], hi")).toBe(true);
    expect(needsName("Mike, hi")).toBe(false);
    // Reusable: a stateful global regex would flip this answer on every second call.
    expect(needsName("[Prospect Name]")).toBe(true);
    expect(needsName("[Prospect Name]")).toBe(true);
  });

  it("substitutes employee count placeholders when provided", () => {
    expect(
      fillName(
        "I'm assuming you run a team of roughly [No of employees] right now?",
        "Mike",
        "6 to 8"
      )
    ).toBe("I'm assuming you run a team of roughly 6 to 8 right now?");

    expect(
      fillName(
        "[Prospect Name], with a team size of [crew size] guys...",
        "Thomas",
        "5"
      )
    ).toBe("Thomas, with a team size of 5 guys...");
  });

  it("falls back to '4 to 8' when employees is omitted or empty", () => {
    expect(
      fillName(
        "I'm assuming you guys run a team of roughly [team size] people right now 🤔?",
        "Mike"
      )
    ).toBe("I'm assuming you guys run a team of roughly 4 to 8 people right now 🤔?");

    expect(
      fillName(
        "established firms with roughly [No of employees] staff",
        "Mike",
        null
      )
    ).toBe("established firms with roughly 4 to 8 staff");

    expect(
      fillName(
        "crew of [headcount] people",
        "Mike",
        "   "
      )
    ).toBe("crew of 4 to 8 people");
  });
});
