// `pnpm dev:copy-data [--force]`: copy the installed DevCat's data and saved passwords into DevCat Dev's
// own (`devcat dev-copy-data`, src-tauri/src/devdata.rs), with DevCat Dev closed (it would save over them).
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { runningDevcats } from "./devcat-running.mjs";

const { dev } = runningDevcats();
if (dev.length) {
  console.error("DevCat Dev is running: quit it first (it would save its own data over the copy).");
  process.exit(1);
}
const manifest = fileURLToPath(new URL("../src-tauri/Cargo.toml", import.meta.url));
// the same build as `pnpm tauri dev`, so it isn't rebuilt with other features
const r = spawnSync("cargo", ["run", "--quiet", "--manifest-path", manifest, "--no-default-features", "--features", "gui", "--", "dev-copy-data", ...process.argv.slice(2)], { stdio: "inherit", shell: process.platform === "win32" });
process.exit(r.status ?? 1);
