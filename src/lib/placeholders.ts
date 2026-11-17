/**
 * The dialer's placeholders.
 *
 * Pulse dials the business owner, so the name it sends is both "the prospect" and "the owner" —
 * one name resolves both brackets. Nothing else is substituted: the remaining brackets in the
 * copy are instructions to the rep (`[spell back email address slowly]`) and must survive
 * untouched, or the script stops being a script.
 */
const NAME_PLACEHOLDER =
  /\[\s*(?:prospect(?:'s)?\s+name|owner['\u2019]?s\s+first\s+name|name)\s*\]/gi;

const EMPLOYEES_PLACEHOLDER =
  /\[\s*(?:no\.?\s*of\s*employees|employees|number\s+of\s+employees|team\s*size|crew\s*size|staff\s*count|headcount)\s*\]/gi;

/**
 * Fills the name placeholders, leaving the text alone when the dialer has not sent a name yet.
 * Also fills employee/team size placeholders if provided.
 */
export function fillName(
  text: string,
  name: string | null | undefined,
  employees?: string | null | undefined
): string {
  let out = text;
  const cleanName = name?.trim();
  if (cleanName) {
    out = out.replace(NAME_PLACEHOLDER, () => cleanName);
  }
  const cleanEmployees = employees?.trim();
  if (cleanEmployees) {
    out = out.replace(EMPLOYEES_PLACEHOLDER, () => cleanEmployees);
  } else {
    out = out.replace(EMPLOYEES_PLACEHOLDER, () => "4 to 8");
  }
  return out;
}

/** True when the copy still contains an unfilled name placeholder. */
export function needsName(text: string): boolean {
  // `lastIndex` is stateful on a global regex, so this one is created per call.
  return /\[\s*(?:prospect(?:'s)?\s+name|owner['\u2019]?s\s+first\s+name)\s*\]/i.test(text);
}
