import { useEffect, useRef } from "react";
import { fillName } from "./lib/placeholders";
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
  now = new Date(),
}: Props) {
  const days = appointmentDays(now);
  const cardRefs = useRef(new Map<number, HTMLDivElement>());

  // Centre the active card in its own column. `scrollIntoView` walks up to the nearest
  // scrollable ancestor, so the three columns stay independent.
  useEffect(() => {
    const element = cardRefs.current.get(activeId);
    element?.scrollIntoView({ behavior: "smooth", block: "center", inline: "nearest" });
  }, [activeId]);

  const register = (id: number) => (node: HTMLDivElement | null) => {
    if (node) cardRefs.current.set(id, node);
    else cardRefs.current.delete(id);
  };

  const renderCard = (card: FlowCard, variant: "center" | "branch") => {
    const active = card.id === activeId;
    return (
      <div
        key={card.id}
        ref={register(card.id)}
        className={`flow-card flow-card--${variant}`}
        data-active={active ? "true" : "false"}
        role="button"
        tabIndex={0}
        aria-current={active ? "step" : undefined}
        onClick={() => onJump(card.id)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            // Space scrolls the column by default, which fights the centred auto-scroll.
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
          <div className="flow-trigger">Prospect: “{fillName(card.trigger, name)}”</div>
        )}

        <div className="flow-copy">
          {card.beats.map((beat, index) => (
            <p key={index} className="flow-beat">
              {fillName(beat, name)}
            </p>
          ))}
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
      <div className="flow-col flow-col--side" aria-label="Left objections">
        {flow.branches
          .filter((branch) => branch.side === "left")
          .map((branch) => renderCard(branch, "branch"))}
      </div>

      <div className="flow-col flow-col--center" aria-label="Centre path">
        {flow.center.map((line) => renderCard(line, "center"))}
      </div>

      <div className="flow-col flow-col--side" aria-label="Right objections">
        {flow.branches
          .filter((branch) => branch.side === "right")
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
