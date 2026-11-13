import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The desktop app only ever needs the dev server on localhost, and Tauri expects a fixed port.
 *
 * A host-wide bind with the Host-header check disabled exists only for a remote preview proxy
 * (a sandbox or a phone on the same LAN). It is opt-in rather than the default: shipping
 * `host: "0.0.0.0"` + `allowedHosts: true` on a laptop would serve this dev build to every
 * device on the network and let any Host header through.
 */
const preview = process.env.STEALTH_PREVIEW === "1";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: preview ? "0.0.0.0" : undefined,
    port: 1420,
    strictPort: true,
    ...(preview ? { allowedHosts: true } : {}),
    watch: {
      // src-tauri is owned by cargo; watching it causes pointless reloads.
      ignored: ["**/src-tauri/**"],
    },
  },
});
