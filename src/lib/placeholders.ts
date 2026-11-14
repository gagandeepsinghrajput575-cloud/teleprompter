/**
 * The dialer's placeholders.
 *
 * Pulse dials the business owner, so the name it sends is both "the prospect" and "the owner" —
 * one name resolves both brackets. Nothing else is substituted: the remaining brackets in the
 * copy are instructions to the rep (`[spell back email address slowly]`) and must survive
 * untouched, or the script stops being a script.
 */
const NAME_PLACEHOLDER =
  /\[\s*(?:prospect(?:'s)?\s+name|owner['\u2019]?s\s+first\s+name)\s*\]/gi;

/**
 * Fills the name placeholders, leaving the text alone when the dialer has not sent a name yet.
 *
 * The replacement is a function so a name is inserted literally: `String.replace` would
 * otherwise treat `$&`, `$1` and friends inside the name as substitution syntax.
 */
export function fillName(text: string, name: string | null | undefined): string {
  const clean = name?.trim();
  if (!clean) return text;
  return text.replace(NAME_PLACEHOLDER, () => clean);
}

/** True when the copy still contains an unfilled name placeholder. */
export function needsName(text: string): boolean {
  // `lastIndex` is stateful on a global regex, so this one is created per call.
  return /\[\s*(?:prospect(?:'s)?\s+name|owner['\u2019]?s\s+first\s+name)\s*\]/i.test(text);
}
