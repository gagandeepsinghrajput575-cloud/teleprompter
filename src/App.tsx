import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { cueChips } from "./lib/cues";
import { loadScript } from "./lib/script";
import fixture from "./fixtures/remodeler_script.json";

const script = loadScript(fixture as never);

type StatusEvent = { running: boolean; message: string };
type TranscriptEvent = { text: string; is_final: boolean };
type ActiveBlockEvent = { id: number; reason: string };

export default function App() {
  const [activeId, setActiveId] = useState(script.blocks[0]?.id ?? 1);
  const [reason, setReason] = useState("start");
  const [status, setStatus] = useState<StatusEvent>({ running: false, message: "idle" });
  const [spoken, setSpoken] = useState("");
  const [level, setLevel] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Live pipeline events emitted from Rust.
  useEffect(() => {
    const unlisteners = [
      listen<ActiveBlockEvent>("active_block", (e) => {
        setActiveId(e.payload.id);
        setReason(e.payload.reason);
      }),
      listen<StatusEvent>("status", (e) => setStatus(e.payload)),
      listen<TranscriptEvent>("transcript", (e) => setSpoken(e.payload.text)),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((off) => off()));
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
        { device: null }
      );
      setStatus({ running: true, message: `listening on ${info.device.device}` });
      if (!info.deepseek) setError("DeepSeek key missing: stage 2 disabled");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
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
      if (e.key === "ArrowDown") invoke("nudge", { delta: 1 }).then((id) => setActiveId(id as number)).catch(() => undefined);
      if (e.key === "ArrowUp") invoke("nudge", { delta: -1 }).then((id) => setActiveId(id as number)).catch(() => undefined);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const active = script.blocks.find((b) => b.id === activeId) ?? script.blocks[0];
  const activeRef = useRef<HTMLDivElement | null>(null);

  // The active block must stay centred, with neighbours visible above and below it.
  // `scrollIntoView` is what produces the smooth lerp between blocks.
  useEffect(() => {
    activeRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
  }, [activeId]);

  return (
    <div className="hud">
      <div className="bar">
        <span className={status.running ? "dot live" : "dot"} />
        <span className="status">{status.message}</span>
        <button className="btn" onClick={status.running ? stop : start} disabled={busy}>
          {status.running ? "Stop" : "Start"}
        </button>
      </div>

      <div className="meter">
        <div className="meter-fill" style={{ width: `${Math.min(100, level * 400)}%` }} />
      </div>

      <div className="blocks">
        {script.blocks.map((block) => (
          <div
            className="block"
            key={block.id}
            data-active={block.id === activeId}
            ref={block.id === activeId ? activeRef : undefined}
          >
            <span className="label">{block.label}</span>
            {block.body}
            {block.id === activeId && (
              <div className="cues">
                {cueChips(block.cue).map((chip) => (
                  <span className="cue" key={chip}>
                    {chip}
                  </span>
                ))}
                <span className="cue reason">{reason}</span>
              </div>
            )}
          </div>
        ))}
      </div>

      <div className="footer">
        <span className="spoken">{spoken || "waiting for speech"}</span>
        <span>
          {active.id}/{script.blocks.length}
        </span>
      </div>
      {error && <div className="error">{error}</div>}
    </div>
  );
}
