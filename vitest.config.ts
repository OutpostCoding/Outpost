import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Frontend test harness — `npm test` runs these first, then the cargo suite
// (run-tests.mjs). happy-dom over jsdom: same API surface for hook and
// component tests, a fraction of the install weight.
//
// globals stay OFF: tests import describe/it/expect from "vitest" explicitly,
// so tsc type-checks them with no ambient-types config. Cleanup is called
// manually in afterEach for the same reason (RTL auto-cleanup needs globals).
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "happy-dom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
