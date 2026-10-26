// Which DevCat programs are running, split into this checkout's dev builds (src-tauri/target/…)
// and the rest (an installed DevCat, or a `devcat serve` from it). For scripts/tauri.mjs and
// scripts/dev-data.mjs, which must not touch the data a running DevCat holds in memory.
import { execFileSync } from "node:child_process";
import { readlinkSync } from "node:fs";
import { fileURLToPath } from "node:url";

const TARGET = fileURLToPath(new URL("../src-tauri/target/", import.meta.url)).toLowerCase();

/** Full paths of running programs named devcat. */
function devcatPaths() {
  try {
    if (process.platform === "win32") {
      const out = execFileSync("powershell", ["-NoProfile", "-Command", "Get-Process devcat -ErrorAction SilentlyContinue | ForEach-Object { $_.Path }"], { encoding: "utf8" });
      return out.split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
    }
    const out = execFileSync("ps", ["-axo", "pid=,comm="], { encoding: "utf8" });
    return out.split("\n").flatMap((line) => {
      const m = line.trim().match(/^(\d+)\s+(.+)$/);
      if (!m || !/(^|\/)devcat$/i.test(m[2])) return [];
      // Linux: comm is only the name; the path is /proc/<pid>/exe
      if (!m[2].includes("/")) { try { return [readlinkSync(`/proc/${m[1]}/exe`)]; } catch { return [m[2]]; } }
      return [m[2]];
    });
  } catch {
    return [];
  }
}

export function runningDevcats() {
  const all = devcatPaths().filter((p) => p !== process.execPath);
  const isDev = (p) => p.replaceAll("\\", "/").toLowerCase().startsWith(TARGET.replaceAll("\\", "/"));
  return { installed: all.filter((p) => !isDev(p)), dev: all.filter(isDev) };
}
