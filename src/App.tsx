import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";
import { cueChips } from "./lib/cues";
import { objectionFor, type Objection } from "./lib/objections";
import { loadScript, type Script } from "./lib/script";
import call1Fixture from "./fixtures/call1_audit_script.json";
import call2Fixture from "./fixtures/call2_sales_script.json";

const call1Script = loadScript(call1Fixture as never);
const call2Script = loadScript(call2Fixture as never);

type StatusEvent = { running: boolean; message: string };
type TranscriptEvent = { text: string; is_final: boolean };
type ActiveBlockEvent = { id: number; reason: string };

export default function App() {
  const [selectedCall, setSelectedCall] = useState<"call1" | "call2" | null>(null);
  const [script, setScript] = useState<Script>(call1Script);
  const [activeId, setActiveId] = useState(call1Script.lines[0]?.id ?? 1);
  const [reason, setReason] = useState("start");
  const [status, setStatus] = useState<StatusEvent>({ running: false, message: "idle" });
  const [spoken, setSpoken] = useState("");
  const [level, setLevel] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editMode, setEditMode] = useState(false);
  const [devices, setDevices] = useState<string[]>([]);
  const [device, setDevice] = useState<string | null>(() => localStorage.getItem("tp.device"));
  const [engine, setEngine] = useState<"apple" | "deepgram">(() => {
    const saved = localStorage.getItem("tp.engine");
    return saved === "deepgram" ? "deepgram" : "apple";
  });

  const chooseEngine = useCallback((next: "apple" | "deepgram") => {
    setEngine(next);
    localStorage.setItem("tp.engine", next);
  }, []);

  // Objection battlecard currently shown (⌃0–⌃9), and the prospect context card fields.
  const [objection, setObjection] = useState<Objection | null>(null);
  const [prospect, setProspect] = useState<{ name: string; company: string; niche: string }>(() => {
    try {
      const raw = localStorage.getItem("tp.prospect");
      if (raw) {
        const parsed = JSON.parse(raw) as Partial<{ name: string; company: string; niche: string }>;
        return {
          name: parsed.name ?? "",
          company: parsed.company ?? "",
          niche: parsed.niche ?? "",
        };
      }
    } catch {
      // ignore malformed storage
    }
    return { name: "", company: "", niche: "" };
  });

  useEffect(() => {
    localStorage.setItem("tp.prospect", JSON.stringify(prospect));
  }, [prospect]);

  useEffect(() => {
    const onShow = listen<number>("objection", (e) => {
      setObjection(objectionFor(e.payload) ?? null);
    });
    const onClose = listen("objection_dismiss", () => setObjection(null));
    return () => {
      onShow.then((off) => off());
      onClose.then((off) => off());
    };
  }, []);

  // The local Pulse Dialer pushes the prospect's context over a loopback webhook when a call
  // starts. Fetch once on mount too, in case the push arrived before this listener existed.
  useEffect(() => {
    type Inbound = { name?: string; company?: string; niche?: string; script?: string | null };
    const apply = (p: Inbound | null) => {
      if (!p) return;
      setProspect({ name: p.name ?? "", company: p.company ?? "", niche: p.niche ?? "" });
      if (p.script === "call1" || p.script === "call2") {
        const chosen = p.script === "call2" ? call2Script : call1Script;
        setSelectedCall(p.script);
        setScript(chosen);
        setActiveId(chosen.lines[0]?.id ?? 1);
        invoke("select_script", { callType: p.script }).catch(() => undefined);
      }
    };
    const un = listen<Inbound>("call_context", (e) => apply(e.payload));
    // Poll briefly: a push that lands while the webview is still booting would otherwise be
    // missed by both the event (not subscribed yet) and the one-shot fetch (nothing stored yet).
    let stopped = false;
    let tries = 0;
    let timer = 0;
    const poll = () => {
      invoke<Inbound | null>("call_context")
        .then((p) => {
          if (stopped) return;
          if (p) apply(p);
          else if (tries++ < 8) timer = window.setTimeout(poll, 500);
        })
        .catch(() => undefined);
    };
    poll();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
      un.then((off) => off());
    };
  }, []);

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

  // Reports the first pointer event that reaches the webview.
  useEffect(() => {
    const probe = () => {
      invoke("input_probe", { kind: "pointerdown" }).catch(() => undefined);
    };
    window.addEventListener("pointerdown", probe, { capture: true, once: true });
    return () => window.removeEventListener("pointerdown", probe, { capture: true });
  }, []);

  // Live pipeline events emitted from Rust. Active-line changes are applied immediately (they
  // drive the highlight and the scroll); the spoken transcript is throttled below because it
  // fires on every interim word and re-rendering the whole list that often adds visible lag.
  useEffect(() => {
    const unlisteners = [
      listen<ActiveBlockEvent>("active_block", (e) => {
        setActiveId(e.payload.id);
        setReason(e.payload.reason);
      }),
      listen<StatusEvent>("status", (e) => setStatus(e.payload)),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((off) => off()));
    };
  }, []);

  const spokenRef = useRef("");
  useEffect(() => {
    const unlisten = listen<TranscriptEvent>("transcript", (e) => {
      spokenRef.current = e.payload.text;
    });
    const id = window.setInterval(() => {
      setSpoken((prev) => (prev === spokenRef.current ? prev : spokenRef.current));
    }, 120);
    return () => {
      unlisten.then((off) => off());
      window.clearInterval(id);
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
      const info = await invoke<{ device: { device: string }; engine: string; jev: boolean }>(
        "start_session",
        { device, engine }
      );
      setStatus({ running: true, message: `listening on ${info.device.device}` });
      if (!info.jev) setError("Jev key missing: stage 2 disabled");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [device, engine]);

  const selectCall = useCallback(
    async (type: "call1" | "call2", chosenDevice?: string | null) => {
      setSelectedCall(type);
      const chosen = type === "call1" ? call1Script : call2Script;
      setScript(chosen);
      setActiveId(chosen.lines[0]?.id ?? 1);
      try {
        await invoke("select_script", { callType: type });
      } catch (err) {
        console.error(err);
      }
      setBusy(true);
      setError(null);
      const targetDevice = chosenDevice !== undefined ? chosenDevice : device;
      try {
        const info = await invoke<{ device: { device: string }; engine: string; jev: boolean }>(
          "start_session",
          { device: targetDevice, engine }
        );
        setStatus({ running: true, message: `listening on ${info.device.device}` });
        if (!info.jev) setError("Jev key missing: stage 2 disabled");
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [device, engine]
  );

  const toggleEngine = useCallback(async () => {
    const next = engine === "apple" ? "deepgram" : "apple";
    chooseEngine(next);
    if (status.running) {
      setBusy(true);
      try {
        await invoke("stop_session");
        const info = await invoke<{ device: { device: string }; engine: string; jev: boolean }>(
          "start_session",
          { device, engine: next }
        );
        setStatus({ running: true, message: `listening on ${info.device.device}` });
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    }
  }, [engine, status.running, chooseEngine, device]);

  // Input devices list.
  useEffect(() => {
    invoke<string[]>("list_devices")
      .then((list) => {
        setDevices(list);
        const saved = localStorage.getItem("tp.device");
        if (saved && list.includes(saved)) {
          setDevice(saved);
        } else {
          const cs = list.find((d) => /crystal/i.test(d));
          if (cs) {
            setDevice(cs);
            localStorage.setItem("tp.device", cs);
          } else if (list.length > 0) {
            setDevice(list[0]);
          }
        }
      })
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

  // Manual nudge shortcuts.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setObjection(null);
        return;
      }
      // Ignore arrows while typing in a field, and ignore modified keys so the global
      // ⌘⇧⌥ section/line hotkeys don't also trigger the local nudge.
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
      if (e.key === "ArrowDown")
        invoke("nudge", { delta: 1 })
          .then((id) => setActiveId(id as number))
          .catch(() => undefined);
      if (e.key === "ArrowUp")
        invoke("nudge", { delta: -1 })
          .then((id) => setActiveId(id as number))
          .catch(() => undefined);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const activeIndex = script.lines.findIndex((line) => line.id === activeId);
  const active = script.lines[activeIndex] ?? script.lines[0];
  const activeRef = useRef<HTMLDivElement | null>(null);
  const blocksRef = useRef<HTMLDivElement | null>(null);

  // Keep the active line in the vertical optical centre of the scroll container. This assigns
  // `scrollTop` directly — a property the webview cannot ignore, unlike `scrollTo({ behavior })`
  // which can be a no-op — and runs in a layout effect so it measures after the active line's
  // cue badges have expanded. `.blocks` carries `scroll-behavior: smooth`, so it animates where
  // supported and snaps instantly where it is not (either way, it *moves*).
  useLayoutEffect(() => {
    const container = blocksRef.current;
    const el = activeRef.current;
    if (!container || !el || container.clientHeight === 0) return;
    const cRect = container.getBoundingClientRect();
    const eRect = el.getBoundingClientRect();
    const centring = (container.clientHeight - eRect.height) / 2;
    const target = Math.max(
      0,
      container.scrollTop + (eRect.top - cRect.top) - centring
    );
    container.scrollTop = target;
  }, [activeId, selectedCall]);

  // Display-only placeholder substitution: the lead's details replace the placeholders in the
  // visible script so the rep never has to decode them. The tracker's own body is untouched, so
  // bracketed placeholders still never count towards line completion.
  const spokenLine = (text: string) => {
    let out = text;
    const name = prospect.name.trim();
    const company = prospect.company.trim();
    const niche = prospect.niche.trim();
    if (name) out = out.replace(/\[Name\]/gi, () => name);
    if (company) out = out.replace(/\[Company\]/gi, () => company);
    if (niche) out = out.replace(/\[Niche\]/gi, () => niche);
    return out;
  };

  const stageLabel = reason.startsWith("stage2")
    ? "STAGE 2 (JEV)"
    : reason.startsWith("stage1")
      ? "STAGE 1 (LOCAL)"
      : reason.startsWith("line-")
        ? "AUTO"
        : "MANUAL";

  const onDragStart = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!editMode || e.button !== 0) return;
      if ((e.target as HTMLElement).closest("button, input, select, textarea, a")) return;

      const win = getCurrentWindow();
      const originX = e.clientX;
      const originY = e.clientY;
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
            () => undefined
          );
        });
      };

      const move = (ev: PointerEvent) => {
        if (!start) return;
        pending = new PhysicalPosition(
          Math.round(start.x + (ev.clientX - originX) * dpr),
          Math.round(start.y + (ev.clientY - originY) * dpr)
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
            () => undefined
          );
          try {
            e.currentTarget.setPointerCapture(e.pointerId);
          } catch {
            // pointer capture fallback
          }
          window.addEventListener("pointermove", move);
          window.addEventListener("pointerup", up, { once: true });
        })
        .catch((err) => {
          invoke("drag_probe", { mode: "no-position", detail: String(err) }).catch(
            () => undefined
          );
        });
    },
    [editMode]
  );

  return (
    <div className="hud" data-edit={editMode} onPointerDown={onDragStart}>
      {editMode && (
        <div className="grab" title="Drag to reposition">
          <span className="grab-dots" />
        </div>
      )}

      {objection && (
        <div className="objection-card">
          <div className="objection-head">
            <span className="objection-key">⌃{objection.index}</span>
            <span className="objection-label">{objection.label}</span>
            <span className="objection-dismiss">Esc to close</span>
          </div>
          <div className="objection-body">{objection.rebuttal}</div>
          <div className="objection-cue">{objection.cue}</div>
        </div>
      )}

      {/* Screen 1: Prompt asking what call you are doing */}
      {selectedCall === null ? (
        <div className="call-modal">
          <div className="call-modal-title">What call are you doing?</div>
          <div className="call-modal-sub">Select your sales call to start listening</div>

          {(prospect.name || prospect.company || prospect.niche) && (
            <div className="context-card">
              {prospect.name && <span className="ctx-name">{prospect.name}</span>}
              {prospect.company && <span>{prospect.company}</span>}
              {prospect.niche && <span className="ctx-niche">{prospect.niche}</span>}
            </div>
          )}

          <div className="prospect-box">
            <input
              className="picker"
              placeholder="Prospect name"
              value={prospect.name}
              onChange={(e) => setProspect({ ...prospect, name: e.target.value })}
            />
            <input
              className="picker"
              placeholder="Company"
              value={prospect.company}
              onChange={(e) => setProspect({ ...prospect, company: e.target.value })}
            />
            <input
              className="picker"
              placeholder="Niche"
              value={prospect.niche}
              onChange={(e) => setProspect({ ...prospect, niche: e.target.value })}
            />
          </div>

          <button className="call-card" onClick={() => selectCall("call1")}>
            <div className="call-card-name">🔍 Call 1: The 10-Minute Audit</div>
            <div className="call-card-desc">
              Consultative Frame · 4-Bucket Walkthrough · Clinical Diagnosis · Call 2 Booking
            </div>
          </button>

          <button className="call-card" onClick={() => selectCall("call2")}>
            <div className="call-card-name">💼 Call 2: 20-Min Prototype & Close</div>
            <div className="call-card-desc">
              Pain Re-Anchor · 3 Live Prototype Pillars · Soft Close · Starbucks Cashier Pitch
            </div>
          </button>

          <div className="mic-selection-box">
            <span className="mic-selection-label">🎙️ Microphone Input:</span>
            <select
              className="picker"
              value={device ?? ""}
              onChange={(e) => chooseDevice(e.target.value)}
              title="Microphone to listen to"
            >
              {devices.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </select>
          </div>

          <div className="engine-selection-box">
            <span className="engine-selection-label">⚡ Speech Recognition Engine:</span>
            <div className="engine-toggle-group">
              <button
                type="button"
                className={`engine-toggle-btn ${engine === "apple" ? "active" : ""}`}
                onClick={() => chooseEngine("apple")}
              >
                🍏 Apple STT (Local)
              </button>
              <button
                type="button"
                className={`engine-toggle-btn ${engine === "deepgram" ? "active" : ""}`}
                onClick={() => chooseEngine("deepgram")}
              >
                ☁️ Deepgram (Cloud)
              </button>
            </div>
          </div>
        </div>
      ) : (
        /* Screen 2: Active Teleprompter HUD */
        <>
          <div className="bar">
            <span className={status.running ? "dot live" : "dot"} />
            <span className="status">{status.message}</span>
            <button
              className="call-pill"
              onClick={() => setSelectedCall(null)}
              title="Click to switch call"
            >
              {selectedCall === "call1" ? "🔍 Audit (Call 1) ▾" : "💼 Close (Call 2) ▾"}
            </button>
            <button
              className="engine-pill"
              data-engine={engine}
              onClick={toggleEngine}
              title={`Active Engine: ${engine === "apple" ? "Apple On-Device Neural Engine" : "Deepgram Nova-3 Cloud"}. Click to toggle.`}
            >
              {engine === "apple" ? "🍏 Apple" : "☁️ Deepgram"}
            </button>
            <span className="stage" data-stage={stageLabel.includes("2") ? "2" : "1"}>
              {stageLabel}
            </span>
            {!editMode && (
              <span
                className="locked"
                title="Click-through is ON. Unlock with ⌘⇧⌥L or the menu bar item."
              >
                🔒 LOCKED
              </span>
            )}
            <button className="btn" onClick={status.running ? stop : start} disabled={busy}>
              {status.running ? "Stop" : "Start"}
            </button>
          </div>

          {(prospect.name || prospect.company || prospect.niche) && (
            <div className="context-card">
              {prospect.name && <span className="ctx-name">{prospect.name}</span>}
              {prospect.company && <span>{prospect.company}</span>}
              {prospect.niche && <span className="ctx-niche">{prospect.niche}</span>}
            </div>
          )}

          <div className="meter">
            <div className="meter-fill" style={{ width: `${Math.min(100, level * 400)}%` }} />
          </div>

          <div className="blocks" ref={blocksRef}>
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
                    {spokenLine(line.body)}
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

          <div className="footer">
            <span className="spoken">{spoken || "waiting for speech"}</span>
            <span>
              {active ? active.id : 1}/{script.lines.length}
            </span>
          </div>

          <div className="legend">
            ⌘⇧⌥←/→ line · ⌘⇧⌥↑/↓ section · ⌘⇧⌥R re-sync · ⌘⇧⌥D close · ⌃1–0 objection
          </div>
        </>
      )}

      {error && (
        <div className="error">
          <span>{error}</span>
          {/speech recognition/i.test(error) ? (
            <button
              className="btn"
              onClick={() => invoke("open_speech_settings").catch(() => undefined)}
            >
              Open Settings
            </button>
          ) : /microphone/i.test(error) ? (
            <button
              className="btn"
              onClick={() => invoke("open_mic_settings").catch(() => undefined)}
            >
              Open Settings
            </button>
          ) : null}
        </div>
      )}
    </div>
  );
}
