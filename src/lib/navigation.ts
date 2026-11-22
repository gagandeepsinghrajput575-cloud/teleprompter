export type StepDirection = "down" | "up";

export interface StepLineResult {
  nextId: number;
  moved: boolean;
}

/**
 * Pure function for step navigation in the flowchart / script line list.
 *
 * Rules:
 * 1. If `currentId` is not found in `lines` (e.g. rep is on a side branch or unlisted card),
 *    holding position symmetrically is enforced (moved: false). It NEVER teleports to lines[0].
 * 2. Moving down advances by 1 if within bounds, otherwise holds position.
 * 3. Moving up retreats by 1 if within bounds, otherwise holds position.
 */
export function stepLine(
  lines: Array<{ id: number }>,
  currentId: number,
  direction: StepDirection
): StepLineResult {
  if (!lines || lines.length === 0) {
    return { nextId: currentId, moved: false };
  }

  const idx = lines.findIndex((l) => l.id === currentId);
  if (idx === -1) {
    // Unlisted card or branch node -> hold position symmetrically
    return { nextId: currentId, moved: false };
  }

  if (direction === "down") {
    if (idx < lines.length - 1) {
      return { nextId: lines[idx + 1].id, moved: true };
    }
    return { nextId: currentId, moved: false };
  } else {
    if (idx > 0) {
      return { nextId: lines[idx - 1].id, moved: true };
    }
    return { nextId: currentId, moved: false };
  }
}
