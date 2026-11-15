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
    if (cmd === "bare_keys_armed") return false as unknown as T;
    if (cmd === "call_context") return null as unknown as T;
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
