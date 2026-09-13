import process from "node:process";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  // Keep rust compiler errors readable in the `tauri dev` terminal.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**", "**/crates/**"] },
  },
  build: {
    // Large documents are common here; keep the bundle free of sourcemap noise
    // but leave symbols readable when profiling.
    target: "es2022",
    chunkSizeWarningLimit: 1024,
  },
});
