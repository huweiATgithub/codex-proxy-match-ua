import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

export default defineConfig({
  plugins: [vue()],
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
  build: {
    outDir: "ui/assets",
    lib: {
      entry: "ui/main.js",
      name: "UaRejections",
      formats: ["iife"],
      fileName: () => "rejections.js",
      cssFileName: "rejections",
    },
  },
});
