import { useEffect, useRef } from "react";
import { fillPlaceholders } from "./lib/placeholders";
import {
  appointmentDays,
  isBookingCard,
  type ColdCallFlow,
  type FlowCard,
} from "./lib/coldcall";

type Props = {
  flow: ColdCallFlow;
  activeId: number;
  onJump: (id: number) => void;
  /** The business owner's name, injected by the dialer. Absent until a call connects. */
  name?: string | null;
  /** Team / employee count from dialer */
  employees?: string | null;
  company?: string | null;
  niche?: string | null;
  /** Injectable for deterministic tests and screenshots. */
  now?: Date;
};

/**
 * Three-column live call flowchart.
 *
 * `1fr 380px 1fr`: objection cards on the left, the centre path fixed at 380px in the middle,
 * objection cards on the right. The centre column is the gold line — the only place the
 * highlight ever lives — so the rep's eye has one fixed home and never has to find it.
 *
 * Scrolling is bidirectional on purpose. A call does not only move forward: a bounced
 * objection sends the rep back up the flow. Every column therefore scrolls its own active card
 * to centre with `block: "center"` and `behavior: "smooth"`, in whichever direction that is.
 */
export default function ColdCallFlowchart({
  flow,
  activeId,
  onJump,
  name = null,
  employees = null,
  company = null,
  niche = null,
  now = new Date(),
}: Props) {
  const days = appointmentDays(now);
  const cardRefs = useRef(new Map<number, HTMLDivElement>());
  const centerColRef = useRef<HTMLDivElement | null>(null);
  const leftColRef = useRef<HTMLDivElement | null>(null);
  const rightColRef = useRef<HTMLDivElement | null>(null);

  // Visual centering: smoothly scroll the column container so the active card is centered in view
  useEffect(() => {
    const activeEl = cardRefs.current.get(activeId);
    if (!activeEl) return;
    const col = activeEl.closest(".flow-col") as HTMLElement | null;
    if (col) {
      const colRect = col.getBoundingClientRect();
      const activeRect = activeEl.getBoundingClientRect();
      const currentRelTop = activeRect.top - colRect.top + col.scrollTop;
      const targetScroll = currentRelTop - col.clientHeight / 2 + activeEl.offsetHeight / 2;
      col.scrollTo({ top: Math.max(0, targetScroll), behavior: "smooth" });
    }
  }, [activeId]);

  const register = (id: number) => (node: HTMLDivElement | null) => {
    if (node) cardRefs.current.set(id, node);
    else cardRefs.current.delete(id);
  };

  const details = { name, employees, company, niche };

  const renderCard = (card: FlowCard, variant: "center" | "branch") => {
    const active = card.id === activeId;
    const activeIndex = flow.center.findIndex((c) => c.id === activeId);
    const cardIndex = flow.center.findIndex((c) => c.id === card.id);
    const isPast =
      variant === "center" && activeIndex !== -1 && cardIndex !== -1 && cardIndex < activeIndex;
    const isNext =
      variant === "center" && activeIndex !== -1 && cardIndex === activeIndex + 1;
    const state = active ? "active" : isPast ? "past" : "future";

    return (
      <div
        key={card.id}
        ref={register(card.id)}
        className={`flow-card flow-card--${variant}`}
        data-active={active ? "true" : "false"}
        data-state={state}
        data-next={isNext ? "true" : "false"}
        role="button"
        tabIndex={0}
        aria-current={active ? "step" : undefined}
        onClick={() => onJump(card.id)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onJump(card.id);
          }
        }}
      >
        <div className="flow-card-head">
          {card.turn && <span className="flow-turn">{card.turn}</span>}
          <span className="flow-label">{card.label}</span>
        </div>

        {variant === "branch" && card.trigger && (
          <div className="flow-trigger">Prospect: “{fillPlaceholders(card.trigger, details)}”</div>
        )}

        <div className="flow-copy">
          {card.beats.map((beat, index) => {
            const formattedBeat = isBookingCard(card)
              ? beat.replace(/\bThursday\b/g, days.dayAfter.weekday)
              : beat;
            return (
              <p key={index} className="flow-beat">
                {fillPlaceholders(formattedBeat, details)}
              </p>
            );
          })}
        </div>

        {isBookingCard(card) && (
          <div className="flow-days" aria-label="Live appointment days">
            <span>
              Tomorrow · <strong>{days.tomorrow.weekday}</strong>
            </span>
            <span>
              Day after · <strong>{days.dayAfter.weekday}</strong>
            </span>
          </div>
        )}

        {card.next != null && (
          <div className="flow-next">
            {variant === "branch" ? "Return to" : "Then"} {turnName(flow, card.next)}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="flow" data-active-id={activeId}>
      <div className="flow-col flow-col--side" ref={leftColRef} aria-label="Left objections">
        {flow.branches
          .filter((branch) => branch.side === "left" && !branch.deleted)
          .map((branch) => renderCard(branch, "branch"))}
      </div>

      <div className="flow-col flow-col--center" ref={centerColRef} aria-label="Centre path">
        {flow.center.map((line) => renderCard(line, "center"))}
      </div>

      <div className="flow-col flow-col--side" ref={rightColRef} aria-label="Right objections">
        {flow.branches
          .filter((branch) => branch.side === "right" && !branch.deleted)
          .map((branch) => renderCard(branch, "branch"))}
      </div>
    </div>
  );
}

function turnName(flow: ColdCallFlow, id: number): string {
  const card =
    flow.center.find((line) => line.id === id) ?? flow.branches.find((branch) => branch.id === id);
  if (!card) return `#${id}`;
  return card.turn ?? card.label;
}
