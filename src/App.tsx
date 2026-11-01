import { useCallback, useEffect, useState } from "react";
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

  const index = script.blocks.findIndex((b) => b.id === activeId);
  const active = script.blocks[index] ?? script.blocks[0];
  const previous = index > 0 ? script.blocks[index - 1] : undefined;
  const next = index >= 0 ? script.blocks[index + 1] : undefined;

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

      {previous && <div className="peek">{previous.label}</div>}

      <div className="block" data-active="true">
        <span className="label">{active.label}</span>
        {active.body}
        <div className="cues">
          {cueChips(active.cue).map((chip) => (
            <span className="cue" key={chip}>
              {chip}
            </span>
          ))}
          <span className="cue reason">{reason}</span>
        </div>
      </div>

      {next && <div className="peek">{next.label}</div>}

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
