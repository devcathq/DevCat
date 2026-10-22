// Sidebar mark on an SSH session whose port forwards are starting, running or failed.
import clsx from "clsx";
import { ArrowLeftRight } from "lucide-react";
import { useEffect } from "react";
import { initForwards, useForwards } from "./forwards";

export function ForwardBadge({ connId }: { connId: string }) {
  useEffect(initForwards, []);
  const s = useForwards((st) => st.byConn[connId]);
  if (!s || s.state === "stopped") return null;
  const title =
    s.state === "error" ? `Port forwards failed: ${s.error ?? ""}`
    : `Port forwards ${s.state === "starting" ? "starting" : "running"}:\n${s.forwards.join("\n")}`;
  return (
    <ArrowLeftRight
      className={clsx("size-3 shrink-0", s.state === "running" ? "text-ok" : s.state === "error" ? "text-danger" : "animate-pulse text-faint")}
      aria-label={title}
    >
      <title>{title}</title>
    </ArrowLeftRight>
  );
}
