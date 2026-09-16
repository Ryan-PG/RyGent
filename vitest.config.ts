import { defineConfig } from "vitest/config";

// Frontend-only harness: components are rendered against a jsdom DOM with no
// Rust core present, so tests must not call the Tauri `invoke` bridge.
export default defineConfig({
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
  },
});
