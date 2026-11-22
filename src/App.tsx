import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { isTauri, safeInvoke as invoke, safeListen as listen, safeStartDragging } from "./lib/tauri";
import { cueChips } from "./lib/cues";
import { getObjectionsForCall, objectionFor, type Objection } from "./lib/objections";
import { stepLine } from "./lib/navigation";
import { loadScript, parseAnnotations, type Script } from "./lib/script";
import { fillPlaceholders } from "./lib/placeholders";
import {
  deleteSavedScript,
  getSavedScripts,
  parseCustomScript,
  saveCustomScript,
  type SavedScriptEntry,
} from "./lib/customScript";
import call1Fixture from "./fixtures/call1_audit_script.json";
import call2Fixture from "./fixtures/call2_sales_script.json";
import coldcallFixture from "./fixtures/coldcall_scalix.json";
import ColdCallFlowchart from "./ColdCallFlowchart";
import { FLOW } from "./lib/coldcall";
import { EyeScrollController } from "./lib/eyeScroll";

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
        .then((lvl) => {
          if (typeof lvl === "number") setLevel(lvl);
        })
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

const getStorage = (key: string): string | null => {
  if (typeof localStorage === "undefined") return null;
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};

const setStorage = (key: string, value: string): void => {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(key, value);
  } catch {
    // ignore
  }
};

export default function App() {
  const [selectedCall, setSelectedCall] = useState<"call1" | "call2" | "coldcall" | null>(() => {
    if (typeof window !== "undefined") {
      const sp = new URLSearchParams(window.location.search);
      const q = sp.get("script") || sp.get("call");
      if (q === "call1" || q === "call2" || q === "coldcall") return q;
    }
    const saved = getStorage("tp.selectedCall");
    return (saved as "call1" | "call2" | "coldcall") || "coldcall";
  });
  const selectedCallRef = useRef(selectedCall);
  useEffect(() => {
    selectedCallRef.current = selectedCall;
  }, [selectedCall]);
  const [script, setScript] = useState<Script>(() => {
    if (typeof window !== "undefined") {
      const sp = new URLSearchParams(window.location.search);
      const q = sp.get("script") || sp.get("call");
      if (q === "call1") return call1Script;
      if (q === "call2") return call2Script;
      if (q === "coldcall") return coldCallScript;
    }
    const saved = getStorage("tp.selectedCall");
    if (saved === "call1") return call1Script;
    if (saved === "call2") return call2Script;
    return coldCallScript;
  });
  const [activeId, setActiveId] = useState(() => {
    if (typeof window !== "undefined") {
      const sp = new URLSearchParams(window.location.search);
      const q = sp.get("script") || sp.get("call");
      if (q === "call1") return call1Script.lines[0]?.id ?? 1;
      if (q === "call2") return call2Script.lines[0]?.id ?? 1;
      if (q === "coldcall") return coldCallScript.lines[0]?.id ?? 1;
    }
    const saved = getStorage("tp.selectedCall");
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
  const [device, setDevice] = useState<string | null>(() => getStorage("tp.device"));
  const [engine, setEngine] = useState<"apple" | "deepgram">(() => {
    const saved = getStorage("tp.engine");
    return saved === "deepgram" ? "deepgram" : "apple";
  });

  const chooseEngine = useCallback((next: "apple" | "deepgram") => {
    setEngine(next);
    setStorage("tp.engine", next);
  }, []);

  // Objection battlecard currently shown (⌃0–⌃9), and the prospect context card fields.
  const [objection, setObjection] = useState<Objection | null>(null);
  const objectionOpen = Boolean(objection);

  // Suggested Jump Protocol State (Zero auto-scroll)
  const [suggestedJump, setSuggestedJump] = useState<{
    page: string;
    label: string;
    targetId: number;
  } | null>(null);

  // Sales Call modal state & custom scripts
  const [salesSubmenu, setSalesSubmenu] = useState(false);
  const [isDraggingFile, setIsDraggingFile] = useState(false);
  const [savedScripts, setSavedScripts] = useState<SavedScriptEntry[]>(() => getSavedScripts());
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const activeRef = useRef<HTMLDivElement | null>(null);
  const blocksRef = useRef<HTMLDivElement | null>(null);
  const scriptRef = useRef(script);
  useEffect(() => {
    scriptRef.current = script;
  }, [script]);


  const executeJump = useCallback((targetId: number) => {
    setActiveId(targetId);
    setSuggestedJump(null);
    if (isTauri) {
      invoke("jump_to_line", { id: targetId }).catch(() => undefined);
    }
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest("input, textarea, select")) return;
      if (!suggestedJump) return;
      if (e.key === "m" || e.key === "M") {
        e.preventDefault();
        executeJump(suggestedJump.targetId);
      } else if (e.key === "n" || e.key === "N") {
        e.preventDefault();
        setSuggestedJump(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [suggestedJump, executeJump]);

  const loadCustomScript = useCallback((custom: Script) => {
    setSelectedCall("call2");
    setStorage("tp.selectedCall", "call2");
    setScript(custom);
    setActiveId(custom.lines[0]?.id ?? 1);
    setSalesSubmenu(false);
  }, []);

  const handleProcessScriptFile = useCallback(
    (file: File) => {
      const reader = new FileReader();
      reader.onload = (e) => {
        const text = e.target?.result;
        if (typeof text === "string") {
          const parsed = parseCustomScript(file.name, text);
          const updated = saveCustomScript(file.name, parsed);
          setSavedScripts(updated);
          loadCustomScript(parsed);
        }
      };
      reader.readAsText(file);
    },
    [loadCustomScript]
  );

  const handleFileSelect = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const file = e.target.files?.[0];
      if (file) handleProcessScriptFile(file);
    },
    [handleProcessScriptFile]
  );

  const handleFileDrop = useCallback(
    (e: React.DragEvent<HTMLDivElement>) => {
      e.preventDefault();
      setIsDraggingFile(false);
      const file = e.dataTransfer.files?.[0];
      if (file) handleProcessScriptFile(file);
    },
    [handleProcessScriptFile]
  );
  // Selects the prospect's audio (the dialer call) and the note the backend reports for it.
  const [prospectAudio, setProspectAudio] = useState(false);
  const [prospectNote, setProspectNote] = useState("");
  // Bare 1-9/0 battlecard keys (Accessibility tap; defaults to ON).
  const [bareKeys, setBareKeys] = useState(() => {
    const saved = getStorage("tp.bareKeys");
    return saved !== null ? saved === "true" : true;
  });
  const [bareNote, setBareNote] = useState("");
  const [prospect, setProspect] = useState<{
    name: string;
    company: string;
    niche: string;
    employees?: string;
  }>(() => {
    try {
      const raw = getStorage("tp.prospect");
      if (raw) {
        const parsed = JSON.parse(raw) as Partial<{
          name: string;
          company: string;
          niche: string;
          employees?: string;
        }>;
        return {
          name: parsed.name ?? "",
          company: parsed.company ?? "",
          niche: parsed.niche ?? "",
          employees: parsed.employees ?? "",
        };
      }
    } catch {
      // ignore malformed storage
    }
    return { name: "", company: "", niche: "", employees: "" };
  });

  useEffect(() => {
    setStorage("tp.prospect", JSON.stringify(prospect));
  }, [prospect]);

  const eyeScrollRef = useRef<EyeScrollController | null>(null);
  if (!eyeScrollRef.current) {
    eyeScrollRef.current = new EyeScrollController({ eyeLineRatio: 0.24 });
  }
  const prevScrollScriptRef = useRef<Script | null>(null);
  const prevScrollCallRef = useRef<typeof selectedCall>(null);

  // Pin the active card's top reading line to the upper webcam eye-level band.
  // Snaps instantaneously on mount or script switch; glides smoothly on line advances.
  useLayoutEffect(() => {
    const controller = eyeScrollRef.current!;
    if (selectedCall === "coldcall" || selectedCall === null) {
      controller.cancel();
      prevScrollCallRef.current = selectedCall;
      return;
    }
    const container = blocksRef.current;
    const el = activeRef.current;
    if (!container || !el) return;

    const scriptChanged =
      prevScrollScriptRef.current !== script ||
      prevScrollCallRef.current !== selectedCall;
    prevScrollScriptRef.current = script;
    prevScrollCallRef.current = selectedCall;

    // Explicit line change always resumes eye-level tracking even after a manual wheel peek.
    controller.clearUserHold();
    controller.align(container, el, { immediate: scriptChanged });
  }, [activeId, selectedCall, script]);

  // Keep the active line locked to eye level across window resizes and dynamic placeholder
  // text wrapping (without fighting the rep when they manually wheel-scroll to peek ahead).
  useEffect(() => {
    const controller = eyeScrollRef.current!;
    if (selectedCall === "coldcall" || selectedCall === null) {
      controller.cancel();
      return;
    }
    const container = blocksRef.current;
    if (!container) return;

    const onUserScroll = () => controller.noteUserScroll();
    container.addEventListener("wheel", onUserScroll, { passive: true });
    container.addEventListener("touchstart", onUserScroll, { passive: true });

    let observer: ResizeObserver | null = null;
    if (typeof ResizeObserver !== "undefined") {
      observer = new ResizeObserver(() => {
        const el = activeRef.current;
        if (el) {
          controller.align(container, el, {
            immediate: true,
            respectUserHold: true,
          });
        }
      });
      observer.observe(container);
      if (activeRef.current) {
        observer.observe(activeRef.current);
      }
    }

    return () => {
      container.removeEventListener("wheel", onUserScroll);
      container.removeEventListener("touchstart", onUserScroll);
      observer?.disconnect();
    };
  }, [selectedCall, activeId, prospect.name, prospect.company, prospect.niche, prospect.employees]);

  // Cancel any pending rAF loop cleanly on unmount.
  useEffect(() => {
    return () => {
      eyeScrollRef.current?.cancel();
    };
  }, []);

  const clearLead = useCallback(() => {
    const empty = { name: "", company: "", niche: "", employees: "" };
    setProspect(empty);
    setStorage("tp.prospect", JSON.stringify(empty));
    if (isTauri) {
      invoke("clear_context").catch(() => undefined);
    }
  }, []);

  useEffect(() => {
    const onShow = listen<number>("objection", (e) => {
      setObjection(
        objectionFor(e.payload, selectedCallRef.current, scriptRef.current?.objections) ?? null
      );
    });
    const onClose = listen("objection_dismiss", () => setObjection(null));
    return () => {
      onShow.then((off) => off());
      onClose.then((off) => off());
    };
  }, []);

  useEffect(() => {
    invoke("set_objection_active", { active: Boolean(objection) }).catch(() => undefined);
  }, [objection]);

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
    type Inbound = {
      name?: string;
      company?: string;
      niche?: string;
      employees?: string;
      script?: string | null;
    };
    const apply = (p: Inbound | null) => {
      if (!p) return;
      setProspect((prev) => ({
        name: p.name ?? prev.name,
        company: p.company ?? prev.company,
        niche: p.niche ?? prev.niche,
        employees: p.employees ?? prev.employees ?? "",
      }));
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
    const unScript = listen<string>("script_selected", (e) => {
      const type = e.payload as "call1" | "call2" | "coldcall";
      if (type === "call1" || type === "call2" || type === "coldcall") {
        const chosen =
          type === "call2" ? call2Script : type === "coldcall" ? coldCallScript : call1Script;
        setSelectedCall(type);
        setStorage("tp.selectedCall", type);
        setScript(chosen);
        setActiveId(chosen.lines[0]?.id ?? 1);
      }
    });
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
      unScript.then((off) => off());
    };
  }, []);

  // Edit mode = click-through is off: the window is unlocked, draggable and clickable.
  useEffect(() => {
    invoke<{ edit_mode: boolean }>("hud_state")
      .then((s) => {
        if (s && typeof s.edit_mode === "boolean") setEditMode(s.edit_mode);
      })
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

  // Ensure the window has adequate height (>= 720px) so the 3-card stack (past, active, next) fits cleanly
  useEffect(() => {
    if (isTauri) {
      import("@tauri-apps/api/window")
        .then(({ getCurrentWindow }) => {
          getCurrentWindow()
            .innerSize()
            .then((size) => {
              if (size.height < 680) {
                import("@tauri-apps/api/dpi")
                  .then(({ LogicalSize }) => {
                    getCurrentWindow()
                      .setSize(new LogicalSize(Math.max(size.width, 1080), 720))
                      .catch(() => {});
                  })
                  .catch(() => {});
              }
            })
            .catch(() => {});
        })
        .catch(() => {});
    }
  }, [isTauri]);

  // Live pipeline events emitted from Rust. Active-line changes are applied immediately (they
  // drive the highlight and the scroll); the spoken transcript is throttled below because it
  // fires on every interim word and re-rendering the whole list that often adds visible lag.
  useEffect(() => {
    const unlisteners = [
      listen<ActiveBlockEvent>("active_block", (e) => {
        // Cold call script is tight with overlapping objection phrases:
        // Fully disable auto-picker in cold call mode so rep manually navigates cards.
        if (selectedCallRef.current === "coldcall") {
          return;
        }
        setActiveId(e.payload.id);
        setReason(e.payload.reason);
      }),
      listen<{ page: string; label: string; targetId: number }>("suggested_jump", (e) => {
        setSuggestedJump(e.payload);
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
      if (info && info.device) {
        if (info.engine && info.engine !== engine) {
          setEngine(info.engine as "apple" | "deepgram");
        }
        const isCs = /crystal/i.test(info.device.device);
        setStatus({
          running: true,
          message: isCs ? `✨ ${info.device.device} (AI Filtered)` : `listening on ${info.device.device}`,
        });
      }
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
        if (info && info.device) {
          if (info.engine && info.engine !== engine) {
            setEngine(info.engine as "apple" | "deepgram");
          }
          const isCs = /crystal/i.test(info.device.device);
          setStatus({
            running: true,
            message: isCs ? `✨ ${info.device.device} (AI Filtered)` : `listening on ${info.device.device}`,
          });
        }
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
        if (info && info.device) {
          const isCs = /crystal/i.test(info.device.device);
          setStatus({
            running: true,
            message: isCs ? `✨ ${info.device.device} (AI Filtered)` : `listening on ${info.device.device}`,
          });
        }
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    }
  }, [engine, status.running, chooseEngine, device]);

  // Synchronize initial script selection with the backend and auto-start listening
  useEffect(() => {
    if (!selectedCall) return;
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
        if (!Array.isArray(list)) return;
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
      if (e.isComposing) return;
      if (e.key === "Escape") {
        setObjection(null);
        return;
      }
      // Ignore keys while typing in text input fields or contenteditable
      const target = e.target as HTMLElement | null;
      if (target?.isContentEditable) return;
      const tag = target?.tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;

      // Digits 1-9, 0 trigger objection battlecards (both bare and Ctrl+digit)
      if (/^[0-9]$/.test(e.key) && !e.metaKey && !e.altKey && !e.shiftKey) {
        const idx = parseInt(e.key, 10);
        setObjection((prev) =>
          prev?.index === idx
            ? null
            : objectionFor(idx, selectedCallRef.current, scriptRef.current?.objections) ?? null
        );
        return;
      }

      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
      if (e.key === " " && (e.target as HTMLElement)?.tagName !== "INPUT") {
        e.preventDefault();
        return;
      }
      if (e.key === "ArrowDown" || e.key === "ArrowRight") {
        e.preventDefault();
        const currentScript = scriptRef.current;
        setActiveId((prev) => {
          const res = stepLine(currentScript.lines, prev, "down");
          if (res.moved && isTauri) {
            invoke("jump_to_line", { id: res.nextId }).catch(() => undefined);
          }
          return res.nextId;
        });
      }
      if (e.key === "ArrowUp" || e.key === "ArrowLeft") {
        e.preventDefault();
        const currentScript = scriptRef.current;
        setActiveId((prev) => {
          const res = stepLine(currentScript.lines, prev, "up");
          if (res.moved && isTauri) {
            invoke("jump_to_line", { id: res.nextId }).catch(() => undefined);
          }
          return res.nextId;
        });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const activeIndex = script.lines.findIndex((line) => line.id === activeId);
  const active = script.lines[activeIndex] ?? script.lines[0];

  // Display-only placeholder substitution: the lead's details replace the placeholders in the
  // visible script so the rep never has to decode them. The tracker's own body is untouched, so
  // bracketed placeholders still never count towards line completion.
  const spokenLine = (text: string) => fillPlaceholders(text, prospect);

  const stageLabel = reason.startsWith("stage2")
    ? "STAGE 2 (JEV)"
    : reason.startsWith("stage1")
      ? "STAGE 1 (LOCAL)"
      : reason.startsWith("line-")
        ? "AUTO"
        : "MANUAL";

  const handleWindowDrag = useCallback((e: React.PointerEvent) => {
    if (e.button !== 0) return;
    if (
      (e.target as HTMLElement).closest(
        "button, input, select, textarea, a, .objection-chip, .line, .flowchart-step, .tree-branch-node, .saved-script-card, .script-dropzone"
      )
    ) {
      return;
    }
    safeStartDragging();
  }, []);

  return (
    <div className="hud" data-edit={editMode}>
      <div
        className="drag-handle"
        data-tauri-drag-region
        onPointerDown={handleWindowDrag}
        title="Drag to reposition window"
      >
        <span className="drag-dots" data-tauri-drag-region />
      </div>

      {objection && (
        <div className="objection-card">
          <div className="objection-head">
            <span className="objection-key">⌃{objection.index}</span>
            <span className="objection-label">{objection.label}</span>
            <span className="objection-dismiss">Esc to close</span>
          </div>
          <div className="objection-body">
            {fillPlaceholders(spokenLine(objection.rebuttal), {
              name: prospect.name,
              employees: prospect.employees,
              company: prospect.company,
              niche: prospect.niche,
            })}
          </div>
          <div className="objection-cue">{objection.cue}</div>
        </div>
      )}

      {/* Screen 1: Prompt asking what call you are doing */}
      {selectedCall === null ? (
        <div className="call-modal">
          <div className="call-modal-title">What call are you doing?</div>
          <div className="call-modal-sub">Select your sales call to start listening</div>

          {(prospect.name || prospect.company || prospect.niche || prospect.employees) && (
            <div className="context-card">
              {prospect.name && <span className="ctx-name">{prospect.name}</span>}
              {prospect.company && <span>{prospect.company}</span>}
              {prospect.niche && <span className="ctx-niche">{prospect.niche}</span>}
              {prospect.employees && (
                <span className="ctx-employees">👥 {prospect.employees} staff</span>
              )}
              <button
                type="button"
                className="ctx-clear-btn"
                title="Clear prospect context"
                onClick={clearLead}
              >
                ✕
              </button>
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
            <input
              className="picker"
              placeholder="Employees / team size"
              value={prospect.employees ?? ""}
              onChange={(e) => setProspect({ ...prospect, employees: e.target.value })}
            />
          </div>

          {!salesSubmenu ? (
            <>
              <button className="call-card" onClick={() => selectCall("coldcall")}>
                <div className="call-card-name">📞 Cold Call: Scalix Elite</div>
                <div className="call-card-desc">
                  Opening · Hook & Assumption · Leak & Offer · Lock the Time · Email & Sign-off
                </div>
              </button>

              <button className="call-card" onClick={() => selectCall("call1")}>
                <div className="call-card-name">🔍 Audit Call (Call 1: The 10-Minute Audit)</div>
                <div className="call-card-desc">
                  Consultative Frame · 4-Bucket Walkthrough · Clinical Diagnosis · Call 2 Booking
                </div>
              </button>

              <button className="call-card" onClick={() => setSalesSubmenu(true)}>
                <div className="call-card-name">💼 Sales Call (Call 2: 20-Min Prototype & Close)</div>
                <div className="call-card-desc">
                  Pain Re-Anchor · 3 Live Prototype Pillars · Custom Scripts (.md/.html)
                </div>
              </button>
            </>
          ) : (
            <div className="sales-submenu">
              <button
                type="button"
                className="sales-back-btn"
                onClick={() => setSalesSubmenu(false)}
              >
                ← Back to call types
              </button>

              <div className="sales-actions-row">
                <button
                  type="button"
                  className="sales-action-btn"
                  onClick={() => {
                    selectCall("call2");
                    setSalesSubmenu(false);
                  }}
                >
                  ⭐ Default Sales Script
                </button>
                <button
                  type="button"
                  className="sales-action-btn"
                  onClick={() => fileInputRef.current?.click()}
                >
                  📂 Load New Script (.md/.html)
                </button>
              </div>

              <input
                type="file"
                ref={fileInputRef}
                accept=".md,.html,.txt"
                style={{ display: "none" }}
                onChange={handleFileSelect}
              />

              <div
                className={`script-dropzone ${isDraggingFile ? "dragging" : ""}`}
                onDragOver={(e) => {
                  e.preventDefault();
                  setIsDraggingFile(true);
                }}
                onDragLeave={(e) => {
                  e.preventDefault();
                  setIsDraggingFile(false);
                }}
                onDrop={handleFileDrop}
                onClick={() => fileInputRef.current?.click()}
              >
                <div className="dropzone-title">📥 Drop .md or .html script here</div>
                <div className="dropzone-sub">or click to browse from computer</div>
              </div>

              {savedScripts.length > 0 && (
                <div className="saved-scripts-container">
                  <div className="saved-scripts-heading">Previously Saved Custom Scripts</div>
                  <div className="saved-scripts-list">
                    {savedScripts.map((entry) => (
                      <div
                        key={entry.id}
                        className="saved-script-card"
                        onClick={() => loadCustomScript(entry.script)}
                      >
                        <div className="saved-script-info">
                          <span className="saved-script-name">{entry.name}</span>
                          <span className="saved-script-meta">
                            {entry.script.lines.length} lines ·{" "}
                            {new Date(entry.savedAt).toLocaleDateString()}
                          </span>
                        </div>
                        <button
                          type="button"
                          className="saved-script-del"
                          onClick={(e) => {
                            e.stopPropagation();
                            const remaining = deleteSavedScript(entry.id);
                            setSavedScripts(remaining);
                          }}
                          title="Delete script"
                        >
                          ✕
                        </button>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}

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
          <div className="bar" data-tauri-drag-region onPointerDown={handleWindowDrag}>
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
              type="button"
              className="call-change-btn"
              onClick={() => {
                setSelectedCall(null);
                localStorage.removeItem("tp.selectedCall");
              }}
              title="Choose another call or change prospect"
            >
              Change
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

          {(prospect.name || prospect.company || prospect.niche || prospect.employees) && (
            <div className="context-card">
              {prospect.name && <span className="ctx-name">{prospect.name}</span>}
              {prospect.company && <span>{prospect.company}</span>}
              {prospect.niche && <span className="ctx-niche">{prospect.niche}</span>}
              {prospect.employees && (
                <span className="ctx-employees">👥 {prospect.employees} staff</span>
              )}
              <button
                type="button"
                className="ctx-clear-btn"
                title="Clear prospect context"
                onClick={clearLead}
              >
                ✕
              </button>
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
            {getObjectionsForCall(selectedCall, script?.objections).map((obj) => (
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
              employees={prospect?.employees}
              company={prospect?.company}
              niche={prospect?.niche}
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
                      className={`line ${state === "active" && objectionOpen ? "anchor-beacon" : ""}`}
                      data-state={state}
                      data-line-id={line.id}
                      ref={line.id === activeId ? activeRef : undefined}
                      onClick={() => {
                        setActiveId(line.id);
                        if (isTauri) {
                          invoke("jump_to_line", { id: line.id }).catch(() => undefined);
                        }
                      }}
                      title="Click to jump to this line"
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
                            {objectionOpen && (
                              <span className="anchor-beacon-badge">
                                <span className="beacon-pulse-dot" />
                                Resume here: Step {line.id}
                              </span>
                            )}
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

      {suggestedJump && (
        <div
          className="suggested-jump-pill"
          onClick={() => executeJump(suggestedJump.targetId)}
          role="button"
          tabIndex={0}
        >
          <span className="suggested-jump-text">
            👉 Suggested: <strong>{suggestedJump.page}</strong> · "{suggestedJump.label}"
          </span>
          <span className="suggested-jump-actions">
            <button
              type="button"
              className="jump-btn jump-confirm"
              onClick={(e) => {
                e.stopPropagation();
                executeJump(suggestedJump.targetId);
              }}
            >
              M: Jump
            </button>
            <button
              type="button"
              className="jump-btn jump-dismiss"
              onClick={(e) => {
                e.stopPropagation();
                setSuggestedJump(null);
              }}
            >
              N: Dismiss
            </button>
          </span>
        </div>
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
