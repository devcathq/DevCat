// Small helpers shared by the git views.
import { displayPath } from "../../lib/remotePath";
import type { GitRefKind } from "../../lib/types";
import { relTime } from "../files/format";

/** relTime() for unix seconds. */
export const relUnix = (sec: number, now?: number) => (sec ? relTime(new Date(sec * 1000).toISOString(), now) : "");

/** Full local date-time for tooltips / details. */
export const fmtUnix = (sec: number) => (sec ? new Date(sec * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }) : "");

/** Compact age: "now", "5m", "3h", "2d", "3w", "4mo", "2y". */
export function ageShort(sec: number, now = Date.now()): string {
  const s = Math.max(0, Math.round(now / 1000 - sec));
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 86400 * 14) return `${Math.floor(s / 86400)}d`;
  if (s < 86400 * 60) return `${Math.floor(s / (86400 * 7))}w`;
  if (s < 86400 * 365) return `${Math.floor(s / (86400 * 30))}mo`;
  return `${Math.floor(s / (86400 * 365))}y`;
}

export const REF_STYLE: Record<GitRefKind, string> = {
  head: "border-accent/50 bg-accent/20 text-accent",
  branch: "border-ok/40 bg-ok/10 text-ok",
  remote: "border-info/40 bg-info/10 text-info",
  tag: "border-warn/40 bg-warn/10 text-warn",
  detached: "border-danger/40 bg-danger/10 text-danger",
  stash: "border-line bg-hover text-muted",
};

/** Status letter of a changed file → text color. */
export const STATUS_TONE: Record<string, string> = {
  A: "text-ok", M: "text-warn", D: "text-danger", R: "text-info", C: "text-info", T: "text-warn", U: "text-danger",
};

/** Human name of an in-progress operation. */
export function operationLabel(op: string): string {
  const o = op.toLowerCase();
  if (o.includes("merge")) return "Merge";
  if (o.includes("rebase")) return "Rebase";
  if (o.includes("cherry")) return "Cherry-pick";
  if (o.includes("revert")) return "Revert";
  if (o.includes("bisect")) return "Bisect";
  return op;
}

/** Shorten a path for display: /Users/me/x → ~/x (on an SSH host: `devbox: ~/x`) */
export function tildify(p: string): string {
  return displayPath(p);
}
