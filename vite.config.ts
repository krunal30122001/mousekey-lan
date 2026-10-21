import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [vue(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/target/**", "**/crates/**"] },
  },
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
  },
});
