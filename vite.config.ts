import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed port and must not have the dev server clear the screen.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // src-tauri is owned by cargo; watching it causes pointless reloads.
      ignored: ["**/src-tauri/**"],
    },
  },
});
