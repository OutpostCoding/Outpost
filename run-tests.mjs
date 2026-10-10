// Cross-platform test runner for `npm test`.
//
// Two suites, frontend first (fast, fails fast): vitest over src/**/*.test.ts,
// then the Rust cargo suite. Extra args are forwarded to the CARGO side only
// (e.g. `npm test -- grounding::`) — the vitest run is always whole.
//
// Windows: the rfd dialog crates need the common-controls v6 manifest embedded,
// which tauri only does for the app binary — bare `cargo test` dies with
// 0xc0000139. So route through src-tauri/test.cmd, which handles the manifest.
// macOS/Linux: no manifest issue, run cargo directly.

import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = dirname(fileURLToPath(import.meta.url));
const passthrough = process.argv.slice(2);

const vitest = spawnSync("npx", ["vitest", "run"], {
  cwd: repoRoot,
  stdio: "inherit",
  shell: true,
});
if (vitest.status !== 0) process.exit(vitest.status ?? 1);

const result =
  process.platform === "win32"
    ? spawnSync("test.cmd", passthrough, {
        cwd: repoRoot,
        stdio: "inherit",
        shell: true,
      })
    : spawnSync("cargo", ["test", "--lib", "-q", ...passthrough], {
        cwd: join(repoRoot, "src-tauri"),
        stdio: "inherit",
      });

process.exit(result.status ?? 1);
