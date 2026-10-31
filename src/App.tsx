import AppShell from "./components/layout/AppShell";
import GhostWindow from "./features/agent/GhostWindow";
import { isGhostWindow } from "./lib/windows";

// A ghost chat's window shows only that chat (features/agent/ghost.ts).
const ghost = isGhostWindow();

export default function App() {
  return ghost ? <GhostWindow /> : <AppShell />;
}
