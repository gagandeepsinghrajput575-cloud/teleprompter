/**
 * The dialer's placeholders.
 *
 * Pulse dials the business owner, so the name it sends is both "the prospect" and "the owner" —
 * one name resolves both brackets.
 */
const NAME_PLACEHOLDER =
  /\[\s*(?:prospect(?:'s)?\s+name|owner['\u2019]?s\s+first\s+name|name)\s*\]/gi;

const EMPLOYEES_PLACEHOLDER =
  /\[\s*(?:no\.?\s*of\s*employees|employees|number\s+of\s+employees|team\s*size|crew\s*size|staff\s*count|headcount)\s*\]/gi;

const COMPANY_PLACEHOLDER =
  /\[\s*(?:company(?:\s+name)?|firm(?:\s+name)?)\s*\]/gi;

const NICHE_PLACEHOLDER =
  /\[\s*(?:niche|trade|industry)\s*\]/gi;

export type ProspectDetails = {
  name?: string | null;
  company?: string | null;
  niche?: string | null;
  employees?: string | null;
};

/**
 * Normalizes team size values sent by CRM or dialer:
 * - Strips leading qualifiers (e.g. "roughly 5-10" -> "5-10") so objection 4 doesn't render "roughly roughly 5-10 staff".
 * - Strips redundant trailing units (e.g. "8 people" or "8 staff staff" -> "8") so Turn 3 doesn't render "8 people people".
 * - Falls back to default "4 to 8" if empty.
 */
export function normalizeTeamSize(raw?: string | null): string {
  if (!raw) return "4 to 8";
  let s = raw.trim();
  if (!s) return "4 to 8";

  // Strip leading qualifiers
  s = s.replace(/^(?:roughly|about|around|approx(?:imately)?\.?|up\s+to|nearly|almost)\s+/i, "").trim();

  // Strip trailing unit repetitions bounded (up to 3 times)
  for (let i = 0; i < 3; i++) {
    const prev = s;
    s = s.replace(/\s*(?:people|staff|employees|workers|guys|members)\s*$/i, "").trim();
    if (s === prev) break;
  }

  return s || "4 to 8";
}

/**
 * Replaces all prospect-related placeholders:
 * - [Name], [Prospect Name], [Owner's First Name]
 * - [Company], [Company Name]
 * - [Niche], [Trade], [Industry]
 * - [team size], [employees], [no of employees], etc. (defaults to "4 to 8")
 *
 * Leaves rep bracket instructions (e.g. [spell back email address slowly]) intact.
 */
export function fillPlaceholders(
  text: string,
  details?: ProspectDetails | null
): string {
  let out = text;
  const cleanName = details?.name?.trim();
  const cleanCompany = details?.company?.trim();
  const cleanNiche = details?.niche?.trim();
  const cleanEmployees = details?.employees?.trim();

  if (cleanName) {
    out = out.replace(NAME_PLACEHOLDER, () => cleanName);
    const first = cleanName.split(/\s+/)[0] ?? "";
    if (first) {
      out = out.replace(/\[\s*firstname\s*\]/gi, () => first);
    }
  }

  if (cleanCompany) {
    out = out.replace(COMPANY_PLACEHOLDER, () => cleanCompany);
    const domain = cleanCompany.toLowerCase().replace(/[^a-z0-9]+/g, "");
    if (domain) {
      out = out.replace(/\[\s*company\s+domain\s*\]/gi, () => domain);
    }
  }

  if (cleanNiche) {
    out = out.replace(NICHE_PLACEHOLDER, () => cleanNiche);
  }

  if (cleanEmployees) {
    const normalized = normalizeTeamSize(cleanEmployees);
    out = out.replace(EMPLOYEES_PLACEHOLDER, () => normalized);
  } else {
    out = out.replace(EMPLOYEES_PLACEHOLDER, () => "4 to 8");
  }

  return out;
}

/**
 * Fills the name placeholders, leaving the text alone when the dialer has not sent a name yet.
 * Also fills employee/team size placeholders if provided.
 * (Backward compatible wrapper around fillPlaceholders).
 */
export function fillName(
  text: string,
  name: string | null | undefined,
  employees?: string | null | undefined,
  company?: string | null | undefined,
  niche?: string | null | undefined
): string {
  return fillPlaceholders(text, { name, employees, company, niche });
}

/** True when the copy still contains an unfilled name placeholder. */
export function needsName(text: string): boolean {
  // `lastIndex` is stateful on a global regex, so this one is created per call.
  return /\[\s*(?:prospect(?:'s)?\s+name|owner['\u2019]?s\s+first\s+name|name)\s*\]/i.test(text);
}
