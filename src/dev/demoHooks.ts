// `vite --mode demo` only: handles for promo/capture.mjs to put the UI in a known state.
import { openClaudeWorkspace } from "../features/claude/open";
import { useWorkspace } from "../features/claude/sidebar/workspace";
import { openGitRepo } from "../features/git/open";
import { openNote, useNotes } from "../features/notes/store";
import { useMobile } from "../features/mobile/store";
import { openIntegrations } from "../features/integrations/store";
import { openMemory } from "../features/memory/MemoryDialog";
import { openSetup, startTour } from "../features/onboarding/store";
import { useConnections } from "../store/connections";
import { openTab, useTabs } from "../store/tabs";
import { useUi } from "../store/ui";

declare global {
  interface Window { __demo?: unknown }
}

window.__demo = { useTabs, useUi, useConnections, useNotes, useWorkspace, useMobile, openTab, openClaudeWorkspace, openGitRepo, openNote, openIntegrations, openMemory, openSetup, startTour };
