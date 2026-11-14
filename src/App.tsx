import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";
import { cueChips } from "./lib/cues";
import { OBJECTIONS, objectionFor, type Objection } from "./lib/objections";
import { loadScript, parseAnnotations, type Script } from "./lib/script";
import call1Fixture from "./fixtures/call1_audit_script.json";
import call2Fixture from "./fixtures/call2_sales_script.json";
import coldcallFixture from "./fixtures/coldcall_scalix.json";
import ColdCallFlowchart from "./ColdCallFlowchart";
import { FLOW } from "./lib/coldcall";

const call1Script = loadScript(call1Fixture as never);
const call2Script = loadScript(call2Fixture as never);
const coldCallScript = loadScript(coldcallFixture as never);

type StatusEvent = { running: boolean; message: string };
type TranscriptEvent = { text: string; is_final: boolean };
type ActiveBlockEvent = { id: number; reason: string };

/** Owns the transcript subscription so 120ms updates never re-render the whole script list. */
function SpokenText() {
  const [text, setText] = useState("");
  useEffect(() => {
    const latest = { current: "" };
    const unlisten = listen<TranscriptEvent>("transcript", (e) => {
      latest.current = e.payload.text;
    });
    const id = window.setInterval(() => {
      setText((prev) => (prev === latest.current ? prev : latest.current));
    }, 150);
    return () => {
      unlisten.then((off) => off());
      window.clearInterval(id);
    };
  }, []);
  return <span className="spoken">{text || "waiting for speech"}</span>;
}

/** Owns the level polling so the meter tick cannot re-render the HUD. */
function LevelMeter() {
  const [level, setLevel] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      invoke<number>("audio_level")
        .then(setLevel)
        .catch(() => undefined);
    }, 200);
    return () => window.clearInterval(id);
  }, []);
  return (
    <div className="meter">
      <div className="meter-fill" style={{ width: `${Math.min(100, level * 400)}%` }} />
    </div>
  );
}

/** Shows what the tap is hearing from the call; own listener so it never re-renders the HUD. */
function ProspectEcho() {
  const [text, setText] = useState("");
  useEffect(() => {
    const un = listen<string>("prospect_transcript", (e) => setText(e.payload));
    return () => {
      un.then((off) => off());
    };
  }, []);
  return <div className="prospect-echo">👂 {text || "listening to the call…"}</div>;
}

export default function App() {
  const [selectedCall, setSelectedCall] = useState<"call1" | "call2" | "coldcall" | null>(() => {
    const saved = localStorage.getItem("tp.selectedCall");
    return (saved as "call1" | "call2" | "coldcall") || "coldcall";
  });
  const [script, setScript] = useState<Script>(() => {
    const saved = localStorage.getItem("tp.selectedCall");
    if (saved === "call1") return call1Script;
    if (saved === "call2") return call2Script;
    return coldCallScript;
  });
  const [activeId, setActiveId] = useState(() => {
    const saved = localStorage.getItem("tp.selectedCall");
    if (saved === "call1") return call1Script.lines[0]?.id ?? 1;
    if (saved === "call2") return call2Script.lines[0]?.id ?? 1;
    return coldCallScript.lines[0]?.id ?? 1;
  });
  const [reason, setReason] = useState("start");
  const [status, setStatus] = useState<StatusEvent>({ running: false, message: "idle" });
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
  // Selects the prospect's audio (the dialer call) and the note the backend reports for it.
  const [prospectAudio, setProspectAudio] = useState(false);
  const [prospectNote, setProspectNote] = useState("");
  // Bare 1-9/0 battlecard keys (Accessibility tap; defaults to ON).
  const [bareKeys, setBareKeys] = useState(() => {
    const saved = localStorage.getItem("tp.bareKeys");
    return saved !== null ? saved === "true" : true;
  });
  const [bareNote, setBareNote] = useState("");
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

  // The backend reports whether the system-audio (prospect) tap actually started, and why not.
  useEffect(() => {
    const un = listen<{ running: boolean; message: string }>("prospect_status", (e) => {
      setProspectAudio(e.payload.running);
      setProspectNote(e.payload.message);
    });
    return () => {
      un.then((off) => off());
    };
  }, []);

  // Reflect or auto-arm the bare keys tap on mount.
  useEffect(() => {
    invoke<boolean>("bare_keys_armed")
      .then((armed) => {
        if (armed) {
          setBareKeys(true);
          setBareNote("bare 1-0 active");
        } else {
          invoke("bare_keys_start")
            .then(() => {
              setBareKeys(true);
              setBareNote("bare 1-0 active");
              localStorage.setItem("tp.bareKeys", "true");
            })
            .catch((err) => {
              const saved = localStorage.getItem("tp.bareKeys");
              if (saved === "false") {
                setBareKeys(false);
                setBareNote("bare 1-0 off");
              } else {
                setBareNote(String(err));
              }
            });
        }
      })
      .catch(() => undefined);
  }, []);

  // The local Pulse Dialer pushes the prospect's context over a loopback webhook when a call
  // starts. Fetch once on mount too, in case the push arrived before this listener existed.
  useEffect(() => {
    type Inbound = { name?: string; company?: string; niche?: string; script?: string | null };
    const apply = (p: Inbound | null) => {
      if (!p) return;
      setProspect({ name: p.name ?? "", company: p.company ?? "", niche: p.niche ?? "" });
      if (p.script === "call1" || p.script === "call2" || p.script === "coldcall") {
        const chosen =
          p.script === "call2" ? call2Script : p.script === "coldcall" ? coldCallScript : call1Script;
        setSelectedCall(p.script);
        localStorage.setItem("tp.selectedCall", p.script);
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

  // The transcript and level subscriptions live in SpokenText/LevelMeter so their high-frequency
  // updates cannot re-render the whole script list.

  const start = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const info = await invoke<{ device: { device: string }; engine: string; jev: boolean }>(
        "start_session",
        { device, engine }
      );
      if (info.engine && info.engine !== engine) {
        setEngine(info.engine as "apple" | "deepgram");
      }
      const isCs = /crystal/i.test(info.device.device);
      setStatus({
        running: true,
        message: isCs ? `✨ ${info.device.device} (AI Filtered)` : `listening on ${info.device.device}`,
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [device, engine]);

  const selectCall = useCallback(
    async (type: "call1" | "call2" | "coldcall", chosenDevice?: string | null) => {
      setSelectedCall(type);
      localStorage.setItem("tp.selectedCall", type);
      const chosen =
        type === "call1" ? call1Script : type === "call2" ? call2Script : coldCallScript;
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
        if (info.engine && info.engine !== engine) {
          setEngine(info.engine as "apple" | "deepgram");
        }
        const isCs = /crystal/i.test(info.device.device);
        setStatus({
          running: true,
          message: isCs ? `✨ ${info.device.device} (AI Filtered)` : `listening on ${info.device.device}`,
        });
        if (type === "call1" || type === "call2") {
          invoke<string>("system_audio_start").catch(() => undefined);
        }
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
        const isCs = /crystal/i.test(info.device.device);
        setStatus({
          running: true,
          message: isCs ? `✨ ${info.device.device} (AI Filtered)` : `listening on ${info.device.device}`,
        });
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    }
  }, [engine, status.running, chooseEngine, device]);

  // Synchronize initial script selection with the backend and auto-start listening
  useEffect(() => {
    invoke("select_script", { callType: selectedCall }).catch(() => undefined);
    const timer = setTimeout(() => {
      start();
    }, 350);
    return () => clearTimeout(timer);
  }, [selectedCall, start]);

  // Toggle the prospect (system-audio) tap. Fully additive: the mic session keeps running either
  // way, and a permission refusal is reported back without breaking anything.
  const toggleProspectAudio = useCallback(async () => {
    if (prospectAudio) {
      await invoke("system_audio_stop").catch(() => undefined);
      setProspectAudio(false);
      setProspectNote("prospect audio off");
      return;
    }
    try {
      const note = await invoke<string>("system_audio_start");
      setProspectNote(note);
    } catch (e) {
      setProspectNote(String(e));
    }
  }, [prospectAudio]);

  // Bare digits: pressing 1-9/0 shows the matching battlecard with no Control key. The backend
  // only swallows digits while a microphone session is listening, so typing elsewhere is safe.
  const toggleBareKeys = useCallback(async () => {
    if (bareKeys) {
      await invoke("bare_keys_stop").catch(() => undefined);
      setBareKeys(false);
      localStorage.setItem("tp.bareKeys", "false");
      setBareNote("bare 1-0 off");
      return;
    }
    try {
      await invoke("bare_keys_start");
      setBareKeys(true);
      localStorage.setItem("tp.bareKeys", "true");
      setBareNote("bare 1-0 active");
    } catch (e) {
      setBareNote(String(e));
    }
  }, [bareKeys]);

  // Input devices list: actively prioritizes CrystalSound for AI voice isolation
  useEffect(() => {
    invoke<string[]>("list_devices")
      .then((list) => {
        setDevices(list);
        const saved = localStorage.getItem("tp.device");
        const cs = list.find((d) => /crystal/i.test(d));
        if (saved && /crystal/i.test(saved) && list.includes(saved)) {
          setDevice(saved);
        } else if (cs) {
          setDevice(cs);
          localStorage.setItem("tp.device", cs);
        } else if (saved && list.includes(saved)) {
          setDevice(saved);
        } else if (list.length > 0) {
          setDevice(list[0]);
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

  // Manual shortcuts & in-window objection triggers (1-9, 0).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setObjection(null);
        return;
      }
      // Ignore keys while typing in text input fields
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;

      // Digits 1-9, 0 trigger objection battlecards (both bare and Ctrl+digit)
      if (/^[0-9]$/.test(e.key) && !e.metaKey && !e.altKey && !e.shiftKey) {
        const idx = parseInt(e.key, 10);
        setObjection((prev) => (prev?.index === idx ? null : objectionFor(idx) ?? null));
        return;
      }

      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
      if (e.key === " " && (e.target as HTMLElement)?.tagName !== "INPUT") {
        e.preventDefault();
        return;
      }
      if (e.key === "ArrowDown" || e.key === "ArrowRight") {
        e.preventDefault();
        invoke("nudge", { delta: 1 })
          .then((id) => setActiveId(id as number))
          .catch(() => undefined);
      }
      if (e.key === "ArrowUp" || e.key === "ArrowLeft") {
        e.preventDefault();
        invoke("nudge", { delta: -1 })
          .then((id) => setActiveId(id as number))
          .catch(() => undefined);
      }
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
    if (name) {
      out = out.replace(/\[Name\]/gi, () => name);
      out = out.replace(/\[Prospect Name\]/gi, () => name);
      out = out.replace(/\[Owner's First Name\]/gi, () => name);
    }
    if (company) out = out.replace(/\[Company\]/gi, () => company);
    if (niche) out = out.replace(/\[Niche\]/gi, () => niche);
    const first = name.split(/\s+/)[0] ?? "";
    if (first) out = out.replace(/\[firstname\]/gi, () => first);
    const domain = company.toLowerCase().replace(/[^a-z0-9]+/g, "");
    if (domain) out = out.replace(/\[company domain\]/gi, () => domain);
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
          <div className="objection-body">{spokenLine(objection.rebuttal)}</div>
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

          <button className="call-card" onClick={() => selectCall("coldcall")}>
            <div className="call-card-name">📞 Cold Call: Scalix Elite</div>
            <div className="call-card-desc">
              Opening · Hook & Assumption · Leak & Offer · Lock the Time · Email & Sign-off
            </div>
          </button>

          <div className="mic-selection-box">
            <span className="mic-selection-label">🎙️ Microphone Input (Voice Filtered):</span>
            <select
              className="picker"
              value={device ?? ""}
              onChange={(e) => chooseDevice(e.target.value)}
              title="Microphone to listen to"
            >
              {devices.map((name) => (
                <option key={name} value={name}>
                  {/crystal/i.test(name) ? `✨ ${name} (AI Noise Filter)` : name}
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
              onClick={() => {
                const next =
                  selectedCall === "coldcall"
                    ? "call1"
                    : selectedCall === "call1"
                      ? "call2"
                      : "coldcall";
                selectCall(next);
              }}
              title="Click to cycle script (Cold Call ➔ Audit ➔ Close)"
            >
              {selectedCall === "call1"
                ? "🔍 Audit (Call 1) ▾"
                : selectedCall === "call2"
                  ? "💼 Close (Call 2) ▾"
                  : "📞 Cold Call ▾"}
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

          <LevelMeter />

          <div className="prospect-row">
            <button
              type="button"
              className={`prospect-toggle ${prospectAudio ? "on" : ""}`}
              onClick={toggleProspectAudio}
              title="Listen to the prospect's side of the call (needs Screen & System Audio Recording)"
            >
              {prospectAudio ? "👂 Prospect audio ON" : "👂 Prospect audio OFF"}
            </button>
            {prospectNote && <span className="prospect-note">{prospectNote}</span>}
            {!prospectAudio && (
              <button
                type="button"
                className="prospect-settings"
                onClick={() => invoke("open_screen_settings").catch(() => undefined)}
                title="Open Screen & System Audio Recording settings"
              >
                settings
              </button>
            )}
          </div>
          {prospectAudio && <ProspectEcho />}

          <div className="prospect-row">
            <button
              type="button"
              className={`prospect-toggle ${bareKeys ? "on" : ""}`}
              onClick={toggleBareKeys}
              title="Press 1-9/0 for battlecards without Control (needs Accessibility; only while listening)"
            >
              {bareKeys ? "1–0 Bare keys ON" : "1–0 Bare keys OFF"}
            </button>
            {bareNote && <span className="prospect-note">{bareNote}</span>}
            {!bareKeys && (
              <button
                type="button"
                className="prospect-settings"
                onClick={() => invoke("open_accessibility_settings").catch(() => undefined)}
                title="Open Accessibility settings"
              >
                settings
              </button>
            )}
          </div>

          <div className="objection-chips-row">
            {OBJECTIONS.map((obj) => (
              <button
                key={obj.index}
                type="button"
                className={`objection-chip ${objection?.index === obj.index ? "active" : ""}`}
                onClick={() => setObjection((prev) => (prev?.index === obj.index ? null : obj))}
                title={`Press ${obj.index} or ⌃${obj.index}: ${obj.label}`}
              >
                <span className="chip-key">{obj.index}</span>
                <span className="chip-label">{obj.label.split("/")[0].trim()}</span>
              </button>
            ))}
          </div>

          {selectedCall === "coldcall" ? (
            <ColdCallFlowchart
              flow={FLOW}
              activeId={activeId}
              onJump={(id: number) => {
                setActiveId(id);
                invoke("jump_to_line", { id }).catch(() => undefined);
              }}
              name={prospect?.name}
            />
          ) : (
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
                      <div className="line-core">
                        <span className="label">{line.label}</span>
                        <span className="line-text">{spokenLine(line.body)}</span>
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
                      {line.branches && line.branches.length > 0 && (
                        <div className="tree-branches">
                          {line.branches.map((b, bIdx) => (
                            <div className="tree-branch-node" key={bIdx}>
                              <div className="branch-cond">
                                <span className="branch-elbow">↳</span>
                                <span className="branch-cond-label">{b.condition}</span>
                              </div>
                              <div className="branch-reply">
                                {spokenLine(parseAnnotations(b.response).body)}
                              </div>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  </Fragment>
                );
              })}
            </div>
          )}

          <div className="footer">
            <SpokenText />
            <span>
              {active ? active.id : 1}/{script.lines.length}
            </span>
          </div>

          <div className="legend">
            <div>⌃1 Price · ⌃2 Not interested · ⌃3 Cold calls · ⌃4 Where number · ⌃5 Email</div>
            <div>⌃6 Web guy · ⌃7 No AI · ⌃8 Booked out · ⌃9 Busy · ⌃0 Employee</div>
            <div>line ⌘⇧⌥←→ · section ⌘⇧⌥↑↓ · re-sync ⌘⇧⌥R · close ⌘⇧⌥D</div>
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
