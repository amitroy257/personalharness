import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In the browser (headless mode / dev) the UI talks to the local server.
const api = process.env.HARNESS_URL || "http://127.0.0.1:4141";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    proxy: {
      "/__ui": { target: api, changeOrigin: true },
      "/v1": { target: api, changeOrigin: true },
      "/health": { target: api, changeOrigin: true },
    },
  },
  build: {
    target: "safari15",
    outDir: "dist",
    emptyOutDir: true,
  },
});
