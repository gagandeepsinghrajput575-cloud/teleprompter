import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { fillName } from "./lib/placeholders";
import {
  appointmentDays,
  isBookingCard,
  type ColdCallFlow,
  type FlowCard,
} from "./lib/coldcall";

export type SlotPosition = "top" | "right" | "bottom" | "left";

export type FlowEdge = {
  id: string;
  fromId: number;
  fromSlot: SlotPosition;
  toId: number;
  toSlot: SlotPosition;
};

export type NodePosition = {
  x: number;
  y: number;
};

type Props = {
  flow: ColdCallFlow;
  activeId: number;
  onJump: (id: number) => void;
  /** The business owner's name, injected by the dialer. Absent until a call connects. */
  name?: string | null;
  /** Injectable for deterministic tests and screenshots. */
  now?: Date;
};

// Default layout generator with spacious non-overlapping positions
function buildDefaultPositions(allCards: FlowCard[]): Record<number, NodePosition> {
  const positions: Record<number, NodePosition> = {};
  const leftX = 40;
  const centerX = 500;
  const rightX = 960;

  // Center spine turns 1 to 7 (spaced out with room for tall cards)
  const spineY: Record<number, number> = {
    1: 40,
    2: 320,
    3: 650,
    4: 980,
    5: 1450,
    6: 1800,
    7: 2100,
  };

  // Branch positioning: carefully spaced so cards never overlap
  const branchMap: Record<number, { x: number; y: number }> = {
    // Top section: Gatekeeper & Site Objections
    101: { x: leftX, y: 40 },     // Branch A · Employee on site
    102: { x: rightX, y: 40 },    // What is this regarding?
    103: { x: rightX, y: 220 },   // Is he expecting the call?

    // Turn 2 section
    112: { x: leftX, y: 320 },    // Booked out / plenty of work
    115: { x: rightX, y: 420 },   // Objection · What is this about?

    // Turn 3 section
    104: { x: leftX, y: 650 },    // Path B · Don't take cold calls
    105: { x: rightX, y: 880 },   // Path D · Early reflex brush-off
    106: { x: rightX, y: 1100 },  // Path C · Where did you get my number?

    // Turn 4 section
    108: { x: leftX, y: 950 },    // Already have a web / tech guy
    109: { x: leftX, y: 1150 },   // We don't replace your web guy
    113: { x: leftX, y: 1400 },   // General "Not interested" after the speech
    114: { x: leftX, y: 1620 },   // Follow-up once they answer

    // Turn 5 section
    107: { x: rightX, y: 1320 },  // Send me an email first
    110: { x: rightX, y: 1600 },  // We don't use AI
    111: { x: rightX, y: 1880 },  // Why is it free? / What's the catch?
  };

  for (const card of allCards) {
    if (card.id in spineY) {
      positions[card.id] = { x: centerX, y: spineY[card.id] };
    } else if (card.id in branchMap) {
      positions[card.id] = branchMap[card.id];
    } else {
      // Custom card fallback
      const idx = allCards.indexOf(card);
      positions[card.id] = {
        x: card.side === "left" ? leftX : card.side === "right" ? rightX : centerX,
        y: 100 + idx * 240,
      };
    }
  }

  return positions;
}

function buildDefaultEdges(): FlowEdge[] {
  return [
    // Spine flow
    { id: "e1-2", fromId: 1, fromSlot: "bottom", toId: 2, toSlot: "top" },
    { id: "e2-3", fromId: 2, fromSlot: "bottom", toId: 3, toSlot: "top" },
    { id: "e3-4", fromId: 3, fromSlot: "bottom", toId: 4, toSlot: "top" },
    { id: "e4-5", fromId: 4, fromSlot: "bottom", toId: 5, toSlot: "top" },
    { id: "e5-6", fromId: 5, fromSlot: "bottom", toId: 6, toSlot: "top" },
    { id: "e6-7", fromId: 6, fromSlot: "bottom", toId: 7, toSlot: "top" },

    // Gatekeeper & Early Inquiries
    { id: "e101-1", fromId: 101, fromSlot: "right", toId: 1, toSlot: "left" },
    { id: "e102-103", fromId: 102, fromSlot: "bottom", toId: 103, toSlot: "top" },
    { id: "e103-3", fromId: 103, fromSlot: "left", toId: 3, toSlot: "right" },
    { id: "e115-4", fromId: 115, fromSlot: "left", toId: 4, toSlot: "right" },

    // Row 2 / Site objections
    { id: "e112-2", fromId: 112, fromSlot: "right", toId: 2, toSlot: "left" },

    // Early Reflex & Hook Objections
    { id: "e104-4", fromId: 104, fromSlot: "right", toId: 4, toSlot: "left" },
    { id: "e105-4", fromId: 105, fromSlot: "left", toId: 4, toSlot: "right" },
    { id: "e106-4", fromId: 106, fromSlot: "left", toId: 4, toSlot: "right" },

    // Tech guy chain
    { id: "e108-109", fromId: 108, fromSlot: "bottom", toId: 109, toSlot: "top" },
    { id: "e109-5", fromId: 109, fromSlot: "right", toId: 5, toSlot: "left" },

    // General not interested chain
    { id: "e113-114", fromId: 113, fromSlot: "bottom", toId: 114, toSlot: "top" },
    { id: "e114-5", fromId: 114, fromSlot: "right", toId: 5, toSlot: "left" },

    // Offer & Time Lock Objections
    { id: "e107-5", fromId: 107, fromSlot: "left", toId: 5, toSlot: "right" },
    { id: "e110-5", fromId: 110, fromSlot: "left", toId: 5, toSlot: "right" },
    { id: "e111-5", fromId: 111, fromSlot: "left", toId: 5, toSlot: "right" },
  ];
}

export default function ColdCallFlowchart({
  flow,
  activeId,
  onJump,
  name = null,
  now = new Date(),
}: Props) {
  const days = appointmentDays(now);
  const cardRefs = useRef(new Map<number, HTMLDivElement>());
  const canvasRef = useRef<HTMLDivElement>(null);

  // View mode: default to columns in SSR/tests, or loaded from localStorage in browser
  const [viewMode, setViewMode] = useState<"canvas" | "columns">(() => {
    if (typeof window === "undefined") return "columns";
    return (localStorage.getItem("tp.flow.viewMode") as "canvas" | "columns") || "canvas";
  });

  // Custom cards created by the user with "+ New Block"
  const [customCards, setCustomCards] = useState<FlowCard[]>(() => {
    if (typeof window === "undefined") return [];
    try {
      const saved = localStorage.getItem("tp.flow.customCards");
      return saved ? JSON.parse(saved) : [];
    } catch {
      return [];
    }
  });

  // In-place edits to existing cards made via the ✏️ pencil button
  const [editedCards, setEditedCards] = useState<Record<number, Partial<FlowCard>>>(() => {
    if (typeof window === "undefined") return {};
    try {
      const saved = localStorage.getItem("tp.flow.editedCards");
      return saved ? JSON.parse(saved) : {};
    } catch {
      return {};
    }
  });

  // Merged cards list (original flow + user edits + custom cards)
  const allCards = useMemo(() => {
    const rawAll = [...flow.center, ...flow.branches, ...customCards];
    return rawAll.map((card) => {
      const edit = editedCards[card.id];
      return edit ? { ...card, ...edit } : card;
    });
  }, [flow, customCards, editedCards]);

  // Positions on canvas
  const [positions, setPositions] = useState<Record<number, NodePosition>>(() => {
    const defaults = buildDefaultPositions(allCards);
    if (typeof window === "undefined") return defaults;
    try {
      const saved = localStorage.getItem("tp.flow.positions.v2");
      return saved ? { ...defaults, ...JSON.parse(saved) } : defaults;
    } catch {
      return defaults;
    }
  });

  // Connections (edges) between block slots
  const [edges, setEdges] = useState<FlowEdge[]>(() => {
    const defaults = buildDefaultEdges();
    if (typeof window === "undefined") return defaults;
    try {
      const saved = localStorage.getItem("tp.flow.edges.v2");
      return saved ? JSON.parse(saved) : defaults;
    } catch {
      return defaults;
    }
  });

  // Node dimensions cache for accurate slot coordinate calculations
  const [nodeDimensions, setNodeDimensions] = useState<Record<number, { w: number; h: number }>>({});

  // Canvas zoom & pan
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 20, y: 20 });
  const [isPanning, setIsPanning] = useState(false);
  const panStartRef = useRef({ x: 0, y: 0, panX: 0, panY: 0 });

  // Dragging a node
  const [draggingNode, setDraggingNode] = useState<{
    id: number;
    startX: number;
    startY: number;
    nodeStartX: number;
    nodeStartY: number;
  } | null>(null);

  // Connecting slots
  const [connecting, setConnecting] = useState<{
    fromId: number;
    fromSlot: SlotPosition;
    mouseX: number;
    mouseY: number;
  } | null>(null);

  // Modal states: ✏️ Edit Card or ➕ New Block
  const [editModalCard, setEditModalCard] = useState<FlowCard | null>(null);
  const [isCreatingNew, setIsCreatingNew] = useState(false);

  // Synchronize state changes to localStorage
  const savePositions = useCallback((newPos: Record<number, NodePosition>) => {
    setPositions(newPos);
    try {
      localStorage.setItem("tp.flow.positions.v2", JSON.stringify(newPos));
    } catch {
      // ignore
    }
  }, []);

  const saveEdges = useCallback((newEdges: FlowEdge[]) => {
    setEdges(newEdges);
    try {
      localStorage.setItem("tp.flow.edges.v2", JSON.stringify(newEdges));
    } catch {
      // ignore
    }
  }, []);

  const saveCustomCards = useCallback((newCards: FlowCard[]) => {
    setCustomCards(newCards);
    try {
      localStorage.setItem("tp.flow.customCards", JSON.stringify(newCards));
    } catch {
      // ignore
    }
  }, []);

  const saveEditedCards = useCallback((newEdits: Record<number, Partial<FlowCard>>) => {
    setEditedCards(newEdits);
    try {
      localStorage.setItem("tp.flow.editedCards", JSON.stringify(newEdits));
    } catch {
      // ignore
    }
  }, []);

  const toggleViewMode = (mode: "canvas" | "columns") => {
    setViewMode(mode);
    try {
      localStorage.setItem("tp.flow.viewMode", mode);
    } catch {
      // ignore
    }
  };

  // Measure card sizes on layout
  const register = useCallback((id: number) => (node: HTMLDivElement | null) => {
    if (node) {
      cardRefs.current.set(id, node);
      const rect = node.getBoundingClientRect();
      if (rect.width > 0 && rect.height > 0) {
        setNodeDimensions((prev) => {
          if (prev[id]?.w === rect.width && prev[id]?.h === rect.height) return prev;
          return { ...prev, [id]: { w: rect.width, h: rect.height } };
        });
      }
    } else {
      cardRefs.current.delete(id);
    }
  }, []);

  // Center the active card in column view
  useEffect(() => {
    if (viewMode === "columns") {
      const element = cardRefs.current.get(activeId);
      element?.scrollIntoView({ behavior: "smooth", block: "center", inline: "nearest" });
    }
  }, [activeId, viewMode]);

  // Canvas Pan & Zoom handlers
  const handleWheel = (e: React.WheelEvent) => {
    // If pinch-to-zoom or Ctrl+wheel is detected, adjust zoom
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const zoomDelta = -e.deltaY * 0.002;
      setZoom((z) => Math.min(2.0, Math.max(0.3, parseFloat((z + zoomDelta).toFixed(2)))));
    } else {
      // Normal mouse wheel or 2-finger trackpad swipe pans the canvas!
      e.preventDefault();
      setPan((p) => ({
        x: p.x - e.deltaX,
        y: p.y - e.deltaY,
      }));
    }
  };

  const handleCanvasMouseDown = (e: React.MouseEvent) => {
    // Allow panning unless clicking directly on an interactive control (card, button, input, slot)
    const target = e.target as HTMLElement;
    if (target.closest(".flow-card, .flow-btn, .slot-port, button, input, textarea, select")) {
      return;
    }
    setIsPanning(true);
    panStartRef.current = {
      x: e.clientX,
      y: e.clientY,
      panX: pan.x,
      panY: pan.y,
    };
  };

  const handleMouseMove = (e: React.MouseEvent) => {
    if (isPanning) {
      const dx = e.clientX - panStartRef.current.x;
      const dy = e.clientY - panStartRef.current.y;
      setPan({ x: panStartRef.current.panX + dx, y: panStartRef.current.panY + dy });
      return;
    }

    if (draggingNode) {
      const dx = (e.clientX - draggingNode.startX) / zoom;
      const dy = (e.clientY - draggingNode.startY) / zoom;
      const newX = Math.round(draggingNode.nodeStartX + dx);
      const newY = Math.round(draggingNode.nodeStartY + dy);
      setPositions((prev) => ({
        ...prev,
        [draggingNode.id]: { x: newX, y: newY },
      }));
      return;
    }

    if (connecting && canvasRef.current) {
      const rect = canvasRef.current.getBoundingClientRect();
      const mouseX = (e.clientX - rect.left - pan.x) / zoom;
      const mouseY = (e.clientY - rect.top - pan.y) / zoom;
      setConnecting((prev) => (prev ? { ...prev, mouseX, mouseY } : null));
    }
  };

  const handleMouseUp = () => {
    if (isPanning) {
      setIsPanning(false);
    }
    if (draggingNode) {
      savePositions(positions);
      setDraggingNode(null);
    }
    if (connecting) {
      setConnecting(null);
    }
  };

  // Node drag start
  const startDragNode = (id: number, e: React.MouseEvent) => {
    const target = e.target as HTMLElement;
    if (target.closest("button, input, textarea, select, .slot-port, .flow-btn-icon")) {
      return;
    }
    e.stopPropagation();
    const current = positions[id] || { x: 0, y: 0 };
    setDraggingNode({
      id,
      startX: e.clientX,
      startY: e.clientY,
      nodeStartX: current.x,
      nodeStartY: current.y,
    });
  };

  // Slot connect start & finish
  const startConnect = (nodeId: number, slot: SlotPosition, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!canvasRef.current) return;
    const rect = canvasRef.current.getBoundingClientRect();
    const mouseX = (e.clientX - rect.left - pan.x) / zoom;
    const mouseY = (e.clientY - rect.top - pan.y) / zoom;
    setConnecting({ fromId: nodeId, fromSlot: slot, mouseX, mouseY });
  };

  const finishConnect = (toId: number, toSlot: SlotPosition, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!connecting) return;
    if (connecting.fromId === toId) {
      setConnecting(null);
      return;
    }

    const edgeId = `e-${connecting.fromId}-${connecting.fromSlot}-${toId}-${toSlot}`;
    // Don't add duplicate edge
    if (!edges.some((edge) => edge.id === edgeId)) {
      const newEdge: FlowEdge = {
        id: edgeId,
        fromId: connecting.fromId,
        fromSlot: connecting.fromSlot,
        toId,
        toSlot,
      };
      saveEdges([...edges, newEdge]);
    }
    setConnecting(null);
  };

  const deleteEdge = (edgeId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    saveEdges(edges.filter((edge) => edge.id !== edgeId));
  };

  // Calculate coordinates of a slot on canvas
  const getSlotCoord = useCallback(
    (nodeId: number, slot: SlotPosition) => {
      const pos = positions[nodeId] || { x: 0, y: 0 };
      const dims = nodeDimensions[nodeId] || { w: 360, h: 180 };
      switch (slot) {
        case "top":
          return { x: pos.x + dims.w / 2, y: pos.y };
        case "right":
          return { x: pos.x + dims.w, y: pos.y + dims.h / 2 };
        case "bottom":
          return { x: pos.x + dims.w / 2, y: pos.y + dims.h };
        case "left":
          return { x: pos.x, y: pos.y + dims.h / 2 };
      }
    },
    [positions, nodeDimensions]
  );

  // Compute smooth cubic bezier path between two slots
  const computeBezierPath = (
    p1: { x: number; y: number },
    s1: SlotPosition,
    p2: { x: number; y: number },
    s2: SlotPosition
  ) => {
    const dx = Math.abs(p2.x - p1.x);
    const dy = Math.abs(p2.y - p1.y);
    const offset = Math.max(30, Math.min(140, Math.hypot(dx, dy) * 0.35));

    let cp1x = p1.x;
    let cp1y = p1.y;
    let cp2x = p2.x;
    let cp2y = p2.y;

    if (s1 === "right") cp1x += offset;
    else if (s1 === "left") cp1x -= offset;
    else if (s1 === "bottom") cp1y += offset;
    else if (s1 === "top") cp1y -= offset;

    if (s2 === "right") cp2x += offset;
    else if (s2 === "left") cp2x -= offset;
    else if (s2 === "bottom") cp2y += offset;
    else if (s2 === "top") cp2y -= offset;

    const midX = (p1.x + p2.x) / 2;
    const midY = (p1.y + p2.y) / 2;

    return {
      path: `M ${p1.x} ${p1.y} C ${cp1x} ${cp1y}, ${cp2x} ${cp2y}, ${p2.x} ${p2.y}`,
      midX,
      midY,
    };
  };

  // Reset to auto-arranged layout
  const resetLayout = () => {
    const defaults = buildDefaultPositions(allCards);
    savePositions(defaults);
    saveEdges(buildDefaultEdges());
    setPan({ x: 20, y: 20 });
    setZoom(1);
  };

  // Delete a card
  const deleteCard = (id: number) => {
    saveCustomCards(customCards.filter((c) => c.id !== id));
    const newEdits = { ...editedCards };
    delete newEdits[id];
    saveEditedCards(newEdits);
    saveEdges(edges.filter((e) => e.fromId !== id && e.toId !== id));
    setEditModalCard(null);
  };

  // Renders a card in either mode
  const renderCardContent = (card: FlowCard, isCanvas: boolean) => {
    const active = card.id === activeId;
    const pos = positions[card.id] || { x: 0, y: 0 };
    const variant = card.side ? "branch" : "center";

    return (
      <div
        key={card.id}
        ref={register(card.id)}
        className={`flow-card flow-card--${variant} ${isCanvas ? "flow-card--canvas-node" : ""}`}
        data-active={active ? "true" : "false"}
        style={isCanvas ? { transform: `translate3d(${pos.x}px, ${pos.y}px, 0)`, cursor: "grab" } : undefined}
        role="button"
        tabIndex={0}
        aria-current={active ? "step" : undefined}
        onMouseDown={isCanvas ? (e) => startDragNode(card.id, e) : undefined}
        onClick={() => onJump(card.id)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onJump(card.id);
          }
        }}
      >
        {/* 4 Connection Slots (Top, Right, Bottom, Left) */}
        {isCanvas && (
          <>
            <div
              className="slot-port slot-port--top"
              onMouseDown={(e) => startConnect(card.id, "top", e)}
              onMouseUp={(e) => finishConnect(card.id, "top", e)}
              title="Slot: Top"
            />
            <div
              className="slot-port slot-port--right"
              onMouseDown={(e) => startConnect(card.id, "right", e)}
              onMouseUp={(e) => finishConnect(card.id, "right", e)}
              title="Slot: Right"
            />
            <div
              className="slot-port slot-port--bottom"
              onMouseDown={(e) => startConnect(card.id, "bottom", e)}
              onMouseUp={(e) => finishConnect(card.id, "bottom", e)}
              title="Slot: Bottom"
            />
            <div
              className="slot-port slot-port--left"
              onMouseDown={(e) => startConnect(card.id, "left", e)}
              onMouseUp={(e) => finishConnect(card.id, "left", e)}
              title="Slot: Left"
            />
          </>
        )}

        {/* Card Header with drag handle and ✏️ pencil button */}
        <div
          className="flow-card-head"
          onMouseDown={isCanvas ? (e) => startDragNode(card.id, e) : undefined}
          style={isCanvas ? { cursor: "grab" } : undefined}
        >
          {card.turn && <span className="flow-turn">{card.turn}</span>}
          <span className="flow-label">{card.label}</span>

          <div className="flow-card-actions">
            {/* ✏️ Pencil Button to edit block */}
            <button
              type="button"
              className="flow-btn-icon"
              onClick={(e) => {
                e.stopPropagation();
                setEditModalCard(card);
              }}
              title="Edit block copy"
            >
              ✏️
            </button>
          </div>
        </div>

        {card.trigger && (
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
            {card.side ? "Return to" : "Then"} {turnName(allCards, card.next)}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="flowchart-container" onMouseMove={handleMouseMove} onMouseUp={handleMouseUp}>
      {/* Top Controls Toolbar */}
      <div className="flow-toolbar">
        <div className="flow-toolbar-left">
          {/* + New Button to create new block */}
          <button
            type="button"
            className="flow-btn flow-btn--primary"
            onClick={() => setIsCreatingNew(true)}
            title="Add a new script block or objection"
          >
            ➕ New Block
          </button>

          {/* 🔄 Reset Layout button */}
          {viewMode === "canvas" && (
            <>
              <button
                type="button"
                className="flow-btn"
                onClick={resetLayout}
                title="Auto-arrange blocks into default layout"
              >
                🔄 Auto-Arrange
              </button>
              <button
                type="button"
                className="flow-btn"
                onClick={() => {
                  setPan({ x: 40, y: 30 });
                  setZoom(1);
                }}
                title="Reset scroll and zoom to center"
              >
                🎯 Center View
              </button>
            </>
          )}
        </div>

        <div className="flow-toolbar-right">
          {viewMode === "canvas" && (
            <div className="flow-zoom-controls">
              <button
                type="button"
                className="flow-btn-small"
                onClick={() => setZoom((z) => Math.max(0.4, z - 0.1))}
                title="Zoom Out"
              >
                −
              </button>
              <span className="flow-zoom-label">{Math.round(zoom * 100)}%</span>
              <button
                type="button"
                className="flow-btn-small"
                onClick={() => setZoom((z) => Math.min(1.8, z + 0.1))}
                title="Zoom In"
              >
                +
              </button>
              <button
                type="button"
                className="flow-btn-small"
                onClick={() => {
                  setZoom(1);
                  setPan({ x: 20, y: 20 });
                }}
                title="Reset View"
              >
                100%
              </button>
            </div>
          )}

          {/* View Mode Toggle: Canvas vs 3-Column */}
          <div className="flow-mode-switch">
            <button
              type="button"
              className={`flow-mode-btn ${viewMode === "canvas" ? "active" : ""}`}
              onClick={() => toggleViewMode("canvas")}
              title="Interactive draggable flowchart canvas with 4 connection slots"
            >
              🔀 Canvas
            </button>
            <button
              type="button"
              className={`flow-mode-btn ${viewMode === "columns" ? "active" : ""}`}
              onClick={() => toggleViewMode("columns")}
              title="Classic 3-column live reading view"
            >
              📋 Columns
            </button>
          </div>
        </div>
      </div>

      {/* VIEW MODE 1: Interactive Canvas */}
      {viewMode === "canvas" ? (
        <div
          ref={canvasRef}
          className="flow-canvas-viewport"
          onMouseDown={handleCanvasMouseDown}
          onWheel={handleWheel}
          style={{ cursor: isPanning ? "grabbing" : "grab" }}
        >
          <div
            className="flow-canvas-content"
            style={{
              transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
              transformOrigin: "0 0",
            }}
          >
            {/* SVG Layer for Dynamic Cable Connections */}
            <svg className="flow-svg-layer">
              <defs>
                <marker
                  id="flow-arrow"
                  viewBox="0 0 10 10"
                  refX="6"
                  refY="5"
                  markerWidth="6"
                  markerHeight="6"
                  orient="auto-start-reverse"
                >
                  <path d="M 0 1 L 9 5 L 0 9 z" fill="#c9a24b" />
                </marker>
                <marker
                  id="flow-arrow-active"
                  viewBox="0 0 10 10"
                  refX="6"
                  refY="5"
                  markerWidth="7"
                  markerHeight="7"
                  orient="auto-start-reverse"
                >
                  <path d="M 0 1 L 9 5 L 0 9 z" fill="#ffd76a" />
                </marker>
              </defs>

              {/* Render Existing Edges */}
              {edges.map((edge) => {
                const p1 = getSlotCoord(edge.fromId, edge.fromSlot);
                const p2 = getSlotCoord(edge.toId, edge.toSlot);
                const { path, midX, midY } = computeBezierPath(p1, edge.fromSlot, p2, edge.toSlot);
                const isActive = edge.fromId === activeId || edge.toId === activeId;

                return (
                  <g key={edge.id} className="flow-edge-group">
                    <path
                      d={path}
                      className={`flow-edge-path ${isActive ? "flow-edge-path--active" : ""}`}
                      markerEnd={isActive ? "url(#flow-arrow-active)" : "url(#flow-arrow)"}
                    />
                    {/* Hover delete connection button */}
                    <g
                      className="flow-edge-delete"
                      transform={`translate(${midX}, ${midY})`}
                      onClick={(e) => deleteEdge(edge.id, e)}
                    >
                      <circle r="9" className="flow-edge-delete-circle" />
                      <text dy="3.5" textAnchor="middle" className="flow-edge-delete-text">
                        ×
                      </text>
                    </g>
                  </g>
                );
              })}

              {/* In-progress connecting line */}
              {connecting && (
                (() => {
                  const p1 = getSlotCoord(connecting.fromId, connecting.fromSlot);
                  const p2 = { x: connecting.mouseX, y: connecting.mouseY };
                  const { path } = computeBezierPath(p1, connecting.fromSlot, p2, "left");
                  return <path d={path} className="flow-edge-path flow-edge-path--draft" />;
                })()
              )}
            </svg>

            {/* Draggable Cards on Canvas */}
            {allCards.map((card) => renderCardContent(card, true))}
          </div>
        </div>
      ) : (
        /* VIEW MODE 2: Classic 3-Column Scrollable Flow */
        <div className="flow" data-active-id={activeId}>
          <div className="flow-col flow-col--side" aria-label="Left objections">
            {allCards
              .filter((c) => c.side === "left")
              .map((c) => renderCardContent(c, false))}
          </div>

          <div className="flow-col flow-col--center" aria-label="Centre path">
            {allCards
              .filter((c) => !c.side)
              .map((c) => renderCardContent(c, false))}
          </div>

          <div className="flow-col flow-col--side" aria-label="Right objections">
            {allCards
              .filter((c) => c.side === "right")
              .map((c) => renderCardContent(c, false))}
          </div>
        </div>
      )}

      {/* Modal: ✏️ Edit Block or ➕ New Block */}
      {(editModalCard || isCreatingNew) && (
        <CardEditorModal
          initialCard={editModalCard}
          isNew={isCreatingNew}
          allCards={allCards}
          onSave={(cardData) => {
            if (isCreatingNew) {
              const newId = Date.now();
              const blockLabel = cardData.label?.trim() || "Untitled Block";
              const newCard: FlowCard = {
                id: newId,
                side: cardData.side,
                turn: cardData.turn,
                label: blockLabel,
                raw: cardData.beats.join(" "),
                beats: cardData.beats,
                anchors: [blockLabel.toLowerCase()],
                objectionAnchors: cardData.side ? [blockLabel.toLowerCase()] : [],
                trigger: cardData.trigger,
                next: cardData.next ?? null,
              };
              saveCustomCards([...customCards, newCard]);
              // Place new card near pan offset
              const newPos = {
                x: Math.round((-pan.x + 300) / zoom),
                y: Math.round((-pan.y + 200) / zoom),
              };
              savePositions({ ...positions, [newId]: newPos });
              setIsCreatingNew(false);
            } else if (editModalCard) {
              saveEditedCards({
                ...editedCards,
                [editModalCard.id]: cardData,
              });
              setEditModalCard(null);
            }
          }}
          onDelete={editModalCard ? () => deleteCard(editModalCard.id) : undefined}
          onClose={() => {
            setEditModalCard(null);
            setIsCreatingNew(false);
          }}
        />
      )}
    </div>
  );
}

function turnName(cards: FlowCard[], id: number): string {
  const card = cards.find((c) => c.id === id);
  if (!card) return `#${id}`;
  return card.turn ?? card.label;
}

// Modal component for creating or editing blocks
type CardEditorProps = {
  initialCard: FlowCard | null;
  isNew: boolean;
  allCards: FlowCard[];
  onSave: (data: Partial<FlowCard> & { beats: string[] }) => void;
  onDelete?: () => void;
  onClose: () => void;
};

function CardEditorModal({ initialCard, isNew, allCards, onSave, onDelete, onClose }: CardEditorProps) {
  const [label, setLabel] = useState(initialCard?.label ?? "");
  const [turn, setTurn] = useState(initialCard?.turn ?? "");
  const [side, setSide] = useState<"left" | "right" | undefined>(initialCard?.side);
  const [trigger, setTrigger] = useState(initialCard?.trigger ?? "");
  const [rawText, setRawText] = useState(initialCard?.beats.join("\n\n") ?? "");
  const [nextId, setNextId] = useState<number | "">(initialCard?.next ?? "");

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    const beats = rawText
      .split(/\n\n+/)
      .map((b) => b.trim())
      .filter(Boolean);

    onSave({
      label: label.trim() || "Untitled Block",
      turn: turn.trim() || undefined,
      side,
      trigger: trigger.trim() || undefined,
      beats: beats.length > 0 ? beats : [rawText],
      next: nextId === "" ? null : Number(nextId),
    });
  };

  return (
    <div className="flow-modal-overlay" onClick={onClose}>
      <div className="flow-modal" onClick={(e) => e.stopPropagation()}>
        <div className="flow-modal-header">
          <h3>{isNew ? "➕ Create New Script Block" : "✏️ Edit Script Block"}</h3>
          <button type="button" className="flow-modal-close" onClick={onClose}>
            ✕
          </button>
        </div>

        <form onSubmit={handleSubmit} className="flow-modal-form">
          <div className="flow-form-row">
            <label>Block Title / Label</label>
            <input
              type="text"
              className="flow-input"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="e.g. Objection · Why is it free?"
              required
            />
          </div>

          <div className="flow-form-row flow-form-cols">
            <div>
              <label>Block Type / Placement</label>
              <select
                className="flow-select"
                value={side ?? "center"}
                onChange={(e) => {
                  const val = e.target.value;
                  setSide(val === "left" || val === "right" ? val : undefined);
                }}
              >
                <option value="center">Center Path (Main Pitch Spine)</option>
                <option value="left">Left Branch (Objection Card)</option>
                <option value="right">Right Branch (Objection Card)</option>
              </select>
            </div>

            <div>
              <label>Turn Name (optional)</label>
              <input
                type="text"
                className="flow-input"
                value={turn}
                onChange={(e) => setTurn(e.target.value)}
                placeholder="e.g. Turn 4"
              />
            </div>
          </div>

          <div className="flow-form-row">
            <label>Prospect Trigger / Objection (optional)</label>
            <input
              type="text"
              className="flow-input"
              value={trigger}
              onChange={(e) => setTrigger(e.target.value)}
              placeholder='e.g. "Just send me an email with some info"'
            />
          </div>

          <div className="flow-form-row">
            <label>Spoken Script (use emojis: 🐢, ⚡, ⬇️, ⏸️, 🤔, 🛑)</label>
            <textarea
              className="flow-textarea"
              rows={6}
              value={rawText}
              onChange={(e) => setRawText(e.target.value)}
              placeholder='🐢 For sure, I can email you ⬇️. ⏸️ ⚡ But the reason we do the 10-minute look first...'
              required
            />
          </div>

          <div className="flow-form-row">
            <label>Next Step / Return To (optional)</label>
            <select
              className="flow-select"
              value={nextId}
              onChange={(e) => setNextId(e.target.value === "" ? "" : Number(e.target.value))}
            >
              <option value="">None / Final</option>
              {allCards.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.turn ? `${c.turn}: ` : ""}{c.label} (#{c.id})
                </option>
              ))}
            </select>
          </div>

          <div className="flow-modal-footer">
            {onDelete && (
              <button
                type="button"
                className="flow-btn flow-btn--danger"
                onClick={() => {
                  if (confirm("Delete this block?")) onDelete();
                }}
              >
                🗑️ Delete
              </button>
            )}
            <div className="flow-modal-actions-right">
              <button type="button" className="flow-btn" onClick={onClose}>
                Cancel
              </button>
              <button type="submit" className="flow-btn flow-btn--primary">
                Save Changes
              </button>
            </div>
          </div>
        </form>
      </div>
    </div>
  );
}
