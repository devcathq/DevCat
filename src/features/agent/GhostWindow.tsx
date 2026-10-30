// The window of a ghost chat (features/agent/ghost.ts): only the chat, like Claude Code in a
// terminal: no sidebar, tabs or workspace header. Closing the window deletes the chat.
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Ghost } from "lucide-react";
import { useEffect, useState } from "react";
import { Toaster } from "sonner";
import { Empty, Spinner } from "../../components/ui";
import { takeLaunchTab } from "../../lib/windows";
import { openTab } from "../../store/tabs";
import { useIsDark } from "../appearance/store";
import { findProvider, loadProviders, useProviders } from "../claude/providers";
import AgentChat from "./AgentChat";

// The chat's tab lives in the store like any other (the chat keeps its session id there), unseen.
// Read once for the window (a render may run twice in development).
let launched: { id: string; cwd: string; providerId: string | null } | null | undefined;
function launchTab() {
  if (launched === undefined) {
    const t = takeLaunchTab();
    if (t?.type === "claude") {
      const { title, ...spec } = t;
      launched = { id: openTab(spec, title), cwd: t.cwd, providerId: t.providerId ?? null };
    } else {
      launched = null;
    }
  }
  return launched;
}

export default function GhostWindow() {
  const [tab] = useState(launchTab);
  const providers = useProviders((s) => s.providers);
  const loaded = useProviders((s) => s.loaded);
  useEffect(() => { void loadProviders(); }, []);
  useEffect(() => {
    if (tab) void getCurrentWindow().setTitle("Ghost chat").catch(() => {});
  }, [tab]);
  const toastTheme = useIsDark() ? "dark" : "light";
  const provider = tab ? findProvider(providers, tab.providerId) : null;

  return (
    <div className="flex h-screen flex-col bg-app text-fg">
      {!tab ? (
        <Empty icon={<Ghost className="size-8" />} title="This ghost chat is gone">Close this window and start a new one.</Empty>
      ) : provider?.kind === "agent" ? (
        <AgentChat tabId={tab.id} cwd={tab.cwd} ghost provider={provider} visible onReview={() => {}} />
      ) : loaded ? (
        <Empty icon={<Ghost className="size-8" />} title="DevCat Agent connection not found">Ghost chats run on a DevCat Agent AI connection.</Empty>
      ) : (
        <div className="flex h-full items-center justify-center"><Spinner className="size-5" /></div>
      )}
      <Toaster theme={toastTheme} position="bottom-right" richColors closeButton />
    </div>
  );
}
