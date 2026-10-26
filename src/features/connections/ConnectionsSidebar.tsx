// Connections sidebar: the connection tree, and under it the queries (saved + history), collapsible.
import { ChevronDown, ChevronRight, History } from "lucide-react";
import { useEffect, useState } from "react";
import { Group, Panel, Separator, usePanelRef } from "react-resizable-panels";
import HistoryPanel from "../history/HistoryPanel";
import ConnectionTree from "./ConnectionTree";

const QUERIES_OPEN_KEY = "devcat.connections.queriesOpen";

export default function ConnectionsSidebar() {
  const queriesPanel = usePanelRef();
  const [queriesOpen, setQueriesOpen] = useState(() => localStorage.getItem(QUERIES_OPEN_KEY) === "1");
  const setOpen = (open: boolean) => {
    localStorage.setItem(QUERIES_OPEN_KEY, open ? "1" : "0");
    setQueriesOpen(open);
  };
  const toggle = () => {
    const p = queriesPanel.current;
    if (!p) return;
    if (p.isCollapsed()) {
      p.expand();
      // started collapsed: there's no earlier height to go back to
      if (p.getSize().asPercentage < 20) p.resize("40%");
    } else p.collapse();
  };

  // Restore the persisted collapsed state once the panels exist.
  useEffect(() => {
    if (!queriesOpen) queriesPanel.current?.collapse();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const Chevron = queriesOpen ? ChevronDown : ChevronRight;
  return (
    <Group orientation="vertical" className="h-full">
      <Panel minSize="120px">
        <ConnectionTree />
      </Panel>
      <Separator className="h-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
      <Panel
        panelRef={queriesPanel}
        defaultSize="40%"
        minSize="120px"
        collapsible
        collapsedSize="28px"
        onResize={() => {
          const open = !(queriesPanel.current?.isCollapsed() ?? false);
          if (open !== queriesOpen) setOpen(open);
        }}
      >
        <div className="flex h-full flex-col">
          <button
            type="button"
            onClick={toggle}
            title={queriesOpen ? "Hide queries" : "Show saved queries and query history"}
            className="flex h-7 w-full shrink-0 items-center gap-1.5 bg-app/40 px-2 text-left text-[11px] font-semibold tracking-wider text-muted uppercase hover:text-fg"
          >
            <Chevron className="size-3.5" />
            <History className="size-3.5" />
            <span className="flex-1">Queries</span>
          </button>
          {queriesOpen && <div className="min-h-0 flex-1 border-t border-line"><HistoryPanel /></div>}
        </div>
      </Panel>
    </Group>
  );
}
