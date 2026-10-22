// Small helpers shared by the Claude workspace views.
import { openTab } from "../../store/tabs";
import { basename } from "./open";

export { basename };

const isAbs = (p: string) => p.startsWith("/") || /^[A-Za-z]:[\\/]/.test(p) || p.startsWith("\\\\");

export function resolvePath(cwd: string, p: string): string {
  if (p.startsWith("~")) return p;
  const sep = cwd.includes("\\") && !cwd.includes("/") ? "\\" : "/";
  const abs = isAbs(p) ? p : `${cwd.replace(/[\\/]+$/, "")}${sep}${p.replace(/^\.[\\/]/, "")}`;
  // Windows: backslashes only, so C:/x/a.ts (as CLIs print it) and C:\x\a.ts are the same tab
  return /^[A-Za-z]:[\\/]/.test(abs) ? abs.replace(/\//g, "\\") : abs;
}

export function relPath(cwd: string, p: string): string {
  const root = cwd.replace(/[\\/]+$/, "");
  return p.startsWith(root + "/") || p.startsWith(root + "\\") ? p.slice(root.length + 1) : p;
}

/** Open the file preview tab. */
export function openFile(cwd: string, path: string) {
  const abs = resolvePath(cwd, path);
  openTab({ type: "file", path: abs, root: cwd }, basename(abs), `file:${abs}`);
}

/** Heuristic: does an inline code span look like a file path? */
export function looksLikePath(s: string): boolean {
  if (s.length > 200 || /\s/.test(s) || /^[a-z]+:\/\//i.test(s)) return false;
  return /^(\.{0,2}\/|~\/|[A-Za-z]:\\)?[\w@.\-]+([\\/][\w@.\-]+)*\.[A-Za-z0-9]{1,8}(:\d+)?$/.test(s) && (/[\\/]/.test(s) || /\.[a-z]{1,5}$/.test(s));
}

export const fmtCost = (usd: number) => (usd < 0.01 && usd > 0 ? "<$0.01" : `$${usd.toFixed(usd < 10 ? 2 : 1)}`);
export function fmtDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`;
}

export const MODEL_ALIASES = [
  { value: "", label: "Default" },
  { value: "fable", label: "Fable" },
  { value: "opus", label: "Opus" },
  { value: "sonnet", label: "Sonnet" },
  { value: "haiku", label: "Haiku" },
];

/** Model options: aliases + whatever `claudeInfo().models` reports (array of strings or {value|id, displayName|name}). */
export function modelOptions(models: unknown): Array<{ value: string; label: string }> {
  const out = [...MODEL_ALIASES];
  if (Array.isArray(models)) {
    for (const m of models) {
      const o = m && typeof m === "object" ? (m as Record<string, unknown>) : null;
      const value = typeof m === "string" ? m : String(o?.value ?? o?.id ?? o?.model ?? "");
      if (!value || value === "default" || out.some((x) => x.value === value)) continue;
      const label = typeof m === "string" ? m : String(o?.displayName ?? o?.display_name ?? o?.name ?? value);
      out.push({ value, label });
    }
  }
  return out;
}

export const PERMISSION_MODES = [
  { value: "manual", label: "Ask" },
  { value: "acceptEdits", label: "Auto-accept edits" },
  { value: "plan", label: "Plan" },
  { value: "auto", label: "Auto" },
  { value: "dontAsk", label: "Don't ask" },
  { value: "bypassPermissions", label: "Bypass permissions" },
] as const;

/** Monaco language id from a file name. */
export function languageOf(path: string): string {
  const name = basename(path).toLowerCase();
  if (name === "dockerfile") return "dockerfile";
  if (name === "makefile") return "makefile";
  const ext = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1) : "";
  const map: Record<string, string> = {
    ts: "typescript", tsx: "typescript", mts: "typescript", cts: "typescript", js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
    json: "json", jsonc: "json", md: "markdown", mdx: "markdown", css: "css", scss: "scss", less: "less", html: "html", htm: "html", xml: "xml", svg: "xml",
    rs: "rust", go: "go", py: "python", rb: "ruby", java: "java", kt: "kotlin", kts: "kotlin", swift: "swift", c: "c", h: "c", cpp: "cpp", cc: "cpp", hpp: "cpp",
    cs: "csharp", php: "php", sh: "shell", bash: "shell", zsh: "shell", yml: "yaml", yaml: "yaml", toml: "ini", ini: "ini", sql: "sql", lua: "lua",
    dart: "dart", scala: "scala", r: "r", graphql: "graphql", gql: "graphql", ps1: "powershell", vue: "html", svelte: "html",
  };
  return map[ext] ?? "plaintext";
}

export async function copyText(text: string) {
  try { await navigator.clipboard.writeText(text); } catch { /* clipboard unavailable */ }
}
