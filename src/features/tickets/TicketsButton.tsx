// Title bar button: opens the Tickets tab (a dot while the agent works on a board's ticket).
import clsx from "clsx";
import { ListTodo } from "lucide-react";
import { useEffect } from "react";
import { openTickets, startTickets, useTickets } from "./store";

export function TicketsButton() {
  const working = useTickets((s) => Object.keys(s.jobs).length > 0);
  const watching = useTickets((s) => s.boards.some((b) => b.watch));
  useEffect(() => {
    startTickets();
    void useTickets.getState().refresh().catch(() => {});
  }, []);
  return (
    <button
      onClick={openTickets}
      title={working ? "Tickets: the agent is working on a ticket" : "Tickets: let the agent fix Jira, Trello or Linear tickets"}
      className={clsx("relative flex h-6 items-center gap-1 rounded px-1.5 hover:bg-hover hover:text-fg", working || watching ? "text-accent" : "text-muted")}
    >
      <ListTodo className="size-3.5" />
      {working && <span className="absolute top-0.5 right-0.5 size-1.5 animate-pulse rounded-full bg-accent" />}
    </button>
  );
}
