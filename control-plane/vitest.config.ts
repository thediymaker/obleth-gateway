import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Test config for the control-plane. `jsdom` gives the companion hooks a DOM
// (window/document/localStorage); Vite resolves the `@/*` tsconfig alias used
// across the app natively. Pure-function/node-style tests run fine under jsdom.
export default defineConfig({
  plugins: [react()],
  resolve: { tsconfigPaths: true },
  test: {
    environment: "jsdom",
    globals: true,
    include: ["{app,lib,components}/**/*.{test,spec}.{ts,tsx}"],
  },
});
