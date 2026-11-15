/**
 * Safe browser-compatible wrappers for Tauri APIs.
 * Allows the entire UI (teleprompter, flowchart, custom blocks) to run
 * seamlessly in regular web browsers (http://localhost:1420) as well as inside native Tauri.
 */

export const isTauri =
  typeof window !== "undefined" &&
  Boolean((window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);

if (typeof window !== "undefined" && !isTauri) {
  document.documentElement.classList.add("in-browser");
  document.body?.classList.add("in-browser");
}

export async function safeInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) {
    // Return sensible mock data in browser mode
    if (cmd === "start_session") {
      const dev = (args?.device as string) || "Default Microphone (Browser)";
      const eng = (args?.engine as string) || "apple";
      return {
        device: { device: dev },
        engine: eng,
        jev: false,
      } as unknown as T;
    }
    if (cmd === "list_devices") {
      return [
        "Default Microphone (Browser)",
        "System Audio Loopback (Browser)",
        "✨ CrystalSound Filter (Simulated)",
      ] as unknown as T;
    }
    if (cmd === "hud_state") {
      return { edit_mode: false } as unknown as T;
    }
    if (cmd === "audio_level") {
      return 0 as unknown as T;
    }
    if (cmd === "bare_keys_armed") {
      return true as unknown as T;
    }
    if (cmd === "bare_keys_start" || cmd === "bare_keys_stop") {
      return undefined as unknown as T;
    }
    if (cmd === "system_audio_start") {
      return "prospect audio active (browser emulated)" as unknown as T;
    }
    if (cmd === "system_audio_stop") {
      return undefined as unknown as T;
    }
    if (cmd === "call_context") {
      return null as unknown as T;
    }
    if (cmd === "select_script" || cmd === "stop_session" || cmd === "jump_to_line") {
      return undefined as unknown as T;
    }
    if (cmd === "nudge") {
      return 1 as unknown as T;
    }
    return undefined as unknown as T;
  }
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args).catch((err) => {
    console.warn(`[Tauri invoke error] ${cmd}:`, err);
    return undefined as unknown as T;
  });
}

export function safeListen<T>(
  event: string,
  handler: (e: { payload: T }) => void
): Promise<() => void> {
  if (!isTauri) {
    return Promise.resolve(() => {});
  }
  return import("@tauri-apps/api/event")
    .then(({ listen }) =>
      listen<T>(event, handler).catch((err) => {
        console.warn(`[Tauri listen error] ${event}:`, err);
        return () => {};
      })
    )
    .catch(() => () => {});
}
