import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";
import { cueChips } from "./lib/cues";
import { loadScript } from "./lib/script";
import { ColdCallTracker, FLOW } from "./lib/coldcall";
import { fillName } from "./lib/placeholders";
import ColdCallFlowchart from "./ColdCallFlowchart";
import fixture from "./fixtures/remodeler_script.json";

const script = loadScript(fixture as never);
/** One HUD, one flowchart state. */
const flowTracker = new ColdCallTracker(FLOW);

type StatusEvent = { running: boolean; message: string };
type TranscriptEvent = { text: string; is_final: boolean };
type ActiveBlockEvent = { id: number; reason: string };

export default function App() {
  const [activeId, setActiveId] = useState(script.lines[0]?.id ?? 1);
  const [reason, setReason] = useState("start");
  const [status, setStatus] = useState<StatusEvent>({ running: false, message: "idle" });
  const [spoken, setSpoken] = useState("");
  const [level, setLevel] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editMode, setEditMode] = useState(false);
  const [devices, setDevices] = useState<string[]>([]);
  const [device, setDevice] = useState<string | null>(() => localStorage.getItem("tp.device"));
  // Two surfaces: the cold-call flowchart (default) and the original linear list, which is still
  // what the Call 1 / Call 2 sales scripts use.
  const [view, setView] = useState<"flow" | "list">("flow");
  const [flowActive, setFlowActive] = useState(FLOW.center[0]?.id ?? 1);
  const [flowTier, setFlowTier] = useState<number | null>(null);
  // The business owner's name for this call, injected by the dialer over loopback.
  const [leadName, setLeadName] = useState<string | null>(null);
  // Module-level instance (below) rather than per-render: this app re-renders several times a
  // second for the level meter, and rebuilding the matcher each time would churn needlessly.
  const viewRef = useRef(view);
  useEffect(() => {
    viewRef.current = view;
  }, [view]);

  // Edit mode = click-through is off: the window is unlocked, draggable and clickable.
  useEffect(() => {
    invoke<{ edit_mode: boolean }>("hud_state")
      .then((s) => setEditMode(s.edit_mode))
      .catch(() => undefined);
    const off = listen<boolean>("edit_mode", (e) => setEditMode(e.payload));
    return () => {
      off.then((f) => f());
    };
  }, []);

  // Reports the first pointer event that reaches the webview. This is the only proof that
  // the OS delivered a click, as opposed to merely permitting one, and it needs no
  // Accessibility permission to synthesise input.
  useEffect(() => {
    const probe = () => {
      invoke("input_probe", { kind: "pointerdown" }).catch(() => undefined);
    };
    window.addEventListener("pointerdown", probe, { capture: true, once: true });
    return () => window.removeEventListener("pointerdown", probe, { capture: true });
  }, []);

  // Live pipeline events emitted from Rust.
  useEffect(() => {
    const unlisteners = [
      listen<ActiveBlockEvent>("active_block", (e) => {
        setActiveId(e.payload.id);
        setReason(e.payload.reason);
      }),
      listen<StatusEvent>("status", (e) => setStatus(e.payload)),
      listen<TranscriptEvent>("transcript", (e) => {
        setSpoken(e.payload.text);
        // The flowchart matcher is rep-first and tiered; it runs on the same transcripts the
        // Rust tracker already produces, so no native change is needed to drive this view.
        if (viewRef.current !== "flow") return;
        const tracker = flowTracker;
        // `is_final` matters: an interim is rewritten by Deepgram, so it must replace the
        // in-progress tail rather than be appended to the settled speech.
        const decision = tracker.decide(e.payload.text, e.payload.is_final);
        if (!decision) return;
        setFlowTier(decision.tier);
        if (decision.id !== tracker.activeId) setFlowActive(tracker.jump(decision.id));
      }),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((off) => off()));
    };
  }, []);

  // The dialer's name for the call in progress. The dialer dials the owner, so one name fills
  // both [Prospect Name] and [Owner's First Name].
  useEffect(() => {
    invoke<string | null>("call_context")
      .then(setLeadName)
      .catch(() => undefined);
    const off = listen<{ name: string | null }>("call_context", (e) =>
      setLeadName(e.payload.name)
    );
    return () => {
      // Rejects when the page is served outside the Tauri shell (the browser preview).
      off.then((f) => f()).catch(() => undefined);
    };
  }, []);

  // Level meter polled from the capture callback.
  useEffect(() => {
    const id = window.setInterval(() => {
      invoke<number>("audio_level")
        .then(setLevel)
        .catch(() => undefined);
    }, 150);
    return () => window.clearInterval(id);
  }, []);

  const start = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const info = await invoke<{ device: { device: string }; deepseek: boolean }>(
        "start_session",
        { device }
      );
      setStatus({ running: true, message: `listening on ${info.device.device}` });
      if (!info.deepseek) setError("DeepSeek key missing: stage 2 disabled");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [device]);

  // Input devices. The default device is not always the one you are speaking into (a USB
  // speakerphone or a virtual loopback can win), so it must be overridable.
  useEffect(() => {
    invoke<string[]>("list_devices")
      .then((list) => setDevices(list))
      .catch(() => undefined);
  }, []);

  const chooseDevice = useCallback((name: string) => {
    const next = name === "" ? null : name;
    setDevice(next);
    if (next) localStorage.setItem("tp.device", next);
    else localStorage.removeItem("tp.device");
  }, []);

  const stop = useCallback(async () => {
    try {
      await invoke("stop_session");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // Manual override. Global shortcuts land in T7.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // In the flowchart, arrows walk the centre path. Before this, they still called the Rust
      // `nudge`, which moved the *linear* tracker behind the flowchart's back.
      if (viewRef.current === "flow") {
        if (e.key === "ArrowDown" || e.key === "ArrowRight") {
          e.preventDefault();
          setFlowActive(flowTracker.advance());
        }
        if (e.key === "ArrowUp" || e.key === "ArrowLeft") {
          e.preventDefault();
          setFlowActive(flowTracker.retreat());
        }
        return;
      }
      if (e.key === "ArrowDown") invoke("nudge", { delta: 1 }).then((id) => setActiveId(id as number)).catch(() => undefined);
      if (e.key === "ArrowUp") invoke("nudge", { delta: -1 }).then((id) => setActiveId(id as number)).catch(() => undefined);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const jumpTo = useCallback((id: number) => {
    setFlowActive(flowTracker.jump(id));
  }, []);

  const activeIndex = script.lines.findIndex((line) => line.id === activeId);
  const active = script.lines[activeIndex] ?? script.lines[0];
  const activeRef = useRef<HTMLDivElement | null>(null);

  // The active block must stay centred, with neighbours visible above and below it.
  // `scrollIntoView` is what produces the smooth lerp between blocks.
  useEffect(() => {
    activeRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
  }, [activeId]);

  const stageLabel =
    view === "flow"
      ? flowTier === null
        ? "FLOW"
        : `TIER ${flowTier}`
      : reason.startsWith("stage2")
        ? "STAGE 2 (DEEPSEEK)"
        : reason.startsWith("stage1")
          ? "STAGE 1 (LOCAL)"
          : "MANUAL";

  // Dragging is implemented here rather than with `data-tauri-drag-region`, because that
  // attribute is handled by an injected script that calls `plugin:window|start_dragging`
  // without awaiting it. That call is denied unless `core:window:allow-start-dragging` is
  // granted, and the denial was completely silent: the attribute matched, the mousedown
  // fired, and nothing moved with no error anywhere. Moving the window ourselves is a few
  // lines, needs only `core:window:allow-set-position`, and can be observed and logged.
  const onDragStart = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!editMode || e.button !== 0) return;
      // Let real controls win: dragging must never swallow a button press.
      if ((e.target as HTMLElement).closest("button, input, select, textarea, a")) return;

      const win = getCurrentWindow();
      const originX = e.clientX;
      const originY = e.clientY;
      // Pointer deltas are CSS pixels; window positions are physical pixels.
      const dpr = window.devicePixelRatio || 1;
      let start: PhysicalPosition | null = null;
      let pending: PhysicalPosition | null = null;
      let frame = 0;

      const flush = () => {
        frame = 0;
        if (!pending) return;
        const next = pending;
        pending = null;
        win.setPosition(next).catch((err) => {
          invoke("drag_probe", { mode: "manual-error", detail: String(err) }).catch(
            () => undefined,
          );
        });
      };

      const move = (ev: PointerEvent) => {
        if (!start) return;
        pending = new PhysicalPosition(
          Math.round(start.x + (ev.clientX - originX) * dpr),
          Math.round(start.y + (ev.clientY - originY) * dpr),
        );
        if (!frame) frame = requestAnimationFrame(flush);
      };

      const up = () => {
        if (frame) cancelAnimationFrame(frame);
        frame = 0;
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        flush();
      };

      win
        .outerPosition()
        .then((pos) => {
          start = pos;
          invoke("drag_probe", { mode: "manual", detail: `from ${pos.x},${pos.y}` }).catch(
            () => undefined,
          );
          try {
            e.currentTarget.setPointerCapture(e.pointerId);
          } catch {
            // Capture is a nicety; window-level listeners still track the drag.
          }
          window.addEventListener("pointermove", move);
          window.addEventListener("pointerup", up, { once: true });
        })
        .catch((err) => {
          invoke("drag_probe", { mode: "no-position", detail: String(err) }).catch(
            () => undefined,
          );
        });
    },
    [editMode],
  );

  return (
    <div className="hud" data-edit={editMode} onPointerDown={onDragStart}>
      {editMode && (
        <div className="grab" title="Drag to reposition">
          <span className="grab-dots" />
        </div>
      )}
      <div className="bar">
        <span className={status.running ? "dot live" : "dot"} />
        <span className="status">{status.message}</span>
        {/* Proof the dialer's name landed, without needing to scroll to Turn 1. */}
        {leadName && (
          <span className="lead-name" title="Injected by the dialer for this call">
            {leadName}
          </span>
        )}
        <span className="stage" data-stage={stageLabel.includes("2") ? "2" : "1"}>
          {stageLabel}
        </span>
        {/* Click-through is silent by nature: every click goes to the window behind, so without
            this the HUD simply looks broken. It is the most important label in the app. */}
        {!editMode && (
          <span
            className="locked"
            title="Click-through is ON: clicks pass to the app behind. Unlock with ⌘⇧⌥L or the menu bar item."
          >
            🔒 LOCKED
          </span>
        )}
        <button
          className="btn"
          onClick={() => setView(view === "flow" ? "list" : "flow")}
          title="Switch between the 3-column flowchart and the linear list"
        >
          {view === "flow" ? "Flow" : "List"}
        </button>
        <button className="btn" onClick={status.running ? stop : start} disabled={busy}>
          {status.running ? "Stop" : "Start"}
        </button>
      </div>

      <div className="meter">
        <div className="meter-fill" style={{ width: `${Math.min(100, level * 400)}%` }} />
      </div>

      {/* The default input is not always the microphone you are speaking into. */}
      {!status.running && devices.length > 0 && (
        <select
          className="picker"
          value={device ?? ""}
          onChange={(e) => chooseDevice(e.target.value)}
          title="Microphone to listen to"
        >
          <option value="">System default</option>
          {devices.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
      )}

      {view === "flow" ? (
        <ColdCallFlowchart
          flow={FLOW}
          activeId={flowActive}
          onJump={jumpTo}
          name={leadName}
        />
      ) : (
      <div className="blocks">
        {script.lines.map((line, i) => {
          const state = line.id === activeId ? "active" : i < activeIndex ? "past" : "future";
          const showPhase = i === 0 || script.lines[i - 1].phase !== line.phase;
          return (
            <Fragment key={line.id}>
              {showPhase && <div className="phase">{line.phase}</div>}
              <div
                className="line"
                data-state={state}
                ref={line.id === activeId ? activeRef : undefined}
              >
                <span className="label">{line.label}</span>
                {fillName(line.body, leadName)}
                {state === "active" && (
                  <div className="cues">
                    {cueChips(line.cue).map((chip) => (
                      <span className="cue" key={chip}>
                        {chip}
                      </span>
                    ))}
                    <span className="cue reason">{reason}</span>
                  </div>
                )}
              </div>
            </Fragment>
          );
        })}
      </div>
      )}

      <div className="footer">
        <span className="spoken">{spoken || "waiting for speech"}</span>
        <span>
          {view === "flow"
            ? `${flowActive} · ${flowTracker.card(flowActive)?.label ?? ""}`
            : `${active.id}/${script.lines.length}`}
        </span>
      </div>
      {error && (
        <div className="error">
          <span>{error}</span>
          {/* A denied microphone is permanent until changed in System Settings. */}
          {/microphone access/i.test(error) && (
            <button className="btn" onClick={() => invoke("open_mic_settings").catch(() => undefined)}>
              Open Settings
            </button>
          )}
        </div>
      )}
    </div>
  );
}
