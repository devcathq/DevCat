// What a tab is, at a glance: an icon (the connection's kind for connection tabs) and a short
// label for its tooltip, so agent chats, terminals, queries and files don't get mixed up.
import {
  ArrowDownUp, ArrowLeftRight, Code2, FileCode2, GitBranch, GitCompare, History, Home, NotebookPen, PencilRuler, Shield, SquareTerminal, Table2, Workflow,
} from "lucide-react";
import { KindIcon } from "../components/ui/KindIcon";
import { AiTerminalIcon } from "../components/ui/AppIcons";
import { FileIcon } from "../features/files/FileIcon";
import { useConnections } from "../store/connections";
import { displayPath, remoteHost, isRemote } from "../lib/remotePath";
import { tabConnId, type Tab } from "./types";

const CLS = "size-3.5 shrink-0";

export function TabIcon({ tab }: { tab: Tab }) {
  const connId = tabConnId(tab);
  const kind = useConnections((s) => (connId ? s.connections.find((c) => c.id === connId)?.kind : undefined));
  switch (tab.type) {
    case "claude": return <AiTerminalIcon className={`${CLS} text-accent`} />;
    case "terminal": return <SquareTerminal className={`${CLS} text-ok`} />;
    case "sftp": return <ArrowDownUp className={`${CLS} text-ok`} />;
    case "query": return <Code2 className={`${CLS} text-info`} />;
    case "table-design": return <PencilRuler className={`${CLS} text-muted`} />;
    case "ddl": return <FileCode2 className={`${CLS} text-muted`} />;
    case "er-diagram": return <Workflow className={`${CLS} text-muted`} />;
    case "file": return <FileIcon name={tab.path.split("/").pop() ?? tab.path} className={CLS} />;
    case "git": return <GitBranch className={`${CLS} text-warn`} />;
    case "note": return <NotebookPen className={`${CLS} text-muted`} />;
    case "network": return <Shield className={`${CLS} text-ok`} />;
    case "history": return <History className={`${CLS} text-muted`} />;
    case "schema-compare": return <GitCompare className={`${CLS} text-muted`} />;
    case "data-transfer": return <ArrowLeftRight className={`${CLS} text-muted`} />;
    case "welcome": return <Home className={`${CLS} text-muted`} />;
    default:
      if (kind) return <KindIcon kind={kind} className={CLS} />;
      return <Table2 className={`${CLS} text-muted`} />;
  }
}

/** "SQL query · shop-db", "Agent chat · ~/app", "Terminal · devbox: app"… for tooltips. */
export function tabKindLabel(tab: Tab, connName?: string): string {
  const on = connName ? ` · ${connName}` : "";
  switch (tab.type) {
    case "claude": return `Agent chat · ${isRemote(tab.cwd) ? `${remoteHost(tab.cwd)}: ` : ""}${tab.cwd.split("/").pop()}`;
    case "terminal": return `Terminal · ${displayPath(tab.cwd)}`;
    case "sftp": return `SFTP · ${displayPath(tab.cwd)}`;
    case "query": return `SQL query${on}`;
    case "table-data": return `Table data${on}`;
    case "table-design": return `Table design${on}`;
    case "ddl": return `Definition${on}`;
    case "redis": return `Redis${on}`;
    case "mongo-collection": return `MongoDB collection${on}`;
    case "er-diagram": return `ER diagram${on}`;
    case "k8s": return `Kubernetes${on}`;
    case "rabbitmq": return `RabbitMQ${on}`;
    case "file": return `File · ${tab.path}`;
    case "git": return `Git · ${tab.repo}`;
    case "note": return "Note";
    case "network": return "VPN / tunnel";
    case "history": return "History";
    case "schema-compare": return "Schema compare";
    case "data-transfer": return `Data transfer${on}`;
    default: return "";
  }
}
