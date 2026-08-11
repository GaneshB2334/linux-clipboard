import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Tauri controls the terminal; don't let Vite wipe its output.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "esnext",
    // The popup must paint fast; a single small bundle beats many requests.
    chunkSizeWarningLimit: 1024,
  },
});
