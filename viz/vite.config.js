import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Dev server proxies nothing; the React app talks to the bridge WebSocket
// directly at ws://localhost:8086. `vite build` emits to dist/, which the
// bridge (server.js) also serves in production.
export default defineConfig({
  plugins: [react()],
  server: { port: 5173 },
  build: { outDir: "dist" },
});
