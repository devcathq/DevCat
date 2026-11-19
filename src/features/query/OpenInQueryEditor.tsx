// "Query editor" on a .sql file's tab: opens the file's text (with unsaved edits) in a query tab
// on a database connection picked from a menu, where it runs like any query. The last connection
// picked for that file comes first next time.
import { Database, Play } from "lucide-react";
import { Button } from "../../components/ui";
import { KindIcon } from "../../components/ui/KindIcon";
import { DATABASE_KINDS } from "../../lib/types";
import { useScopedConnections } from "../../store/connections";
import { openTab } from "../../store/tabs";
import { useContextMenu } from "../connections/ContextMenu";
import { basename } from "../files/format";

const LAST_KEY = "devcat.sqlFileConnection";

function lastFor(path: string): string | null {
  try {
    return (JSON.parse(localStorage.getItem(LAST_KEY) ?? "{}") as Record<string, string>)[path] ?? null;
  } catch {
    return null;
  }
}

function remember(path: string, connId: string) {
  try {
    const all = JSON.parse(localStorage.getItem(LAST_KEY) ?? "{}") as Record<string, string>;
    all[path] = connId;
    localStorage.setItem(LAST_KEY, JSON.stringify(all));
  } catch {
    /* storage full or off: only the order of the menu is lost */
  }
}

export const isSqlFile = (path: string) => /\.sql$/i.test(path);

export function OpenInQueryEditor({ path, getText }: { path: string; getText: () => string }) {
  const menu = useContextMenu();
  const conns = useScopedConnections().filter((c) => DATABASE_KINDS.includes(c.kind) && c.kind !== "redis" && c.kind !== "mongodb");
  const last = lastFor(path);
  const ordered = [...conns].sort((a, b) => Number(b.id === last) - Number(a.id === last));

  const open = (connId: string, run: boolean) => {
    remember(path, connId);
    openTab({ type: "query", connId, initialSql: getText(), autoRun: run ? "full" : undefined }, basename(path));
  };

  const show = (e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items = ordered.length
      ? ordered.map((c) => ({
          label: c.name,
          icon: <KindIcon kind={c.kind} className="size-3.5" />,
          submenu: [
            { label: "Open", icon: <Database className="size-3.5" />, onClick: () => open(c.id, false) },
            { label: "Open and run all", icon: <Play className="size-3.5" />, onClick: () => open(c.id, true) },
          ],
        }))
      : [{ label: "No SQL connections yet", disabled: true, onClick: () => {} }];
    menu.show({ clientX: r.left, clientY: r.bottom + 2, preventDefault: () => {} }, items);
  };

  return (
    <>
      <Button size="sm" variant="ghost" onClick={show} title="Open this script in the query editor on a connection, to run it there">
        <Database className="size-3.5" />Query editor
      </Button>
      {menu.element}
    </>
  );
}
