import { lazy, Suspense, type ComponentType } from "react";
import { Spinner } from "../components/ui";
import type { Tab } from "./types";
import Welcome from "./Welcome";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const registry: Record<Tab["type"], ComponentType<{ tab: any }>> = {
  welcome: Welcome,
  query: lazy(() => import("../features/query/QueryTab")),
  "table-data": lazy(() => import("../features/table/TableDataTab")),
  "table-design": lazy(() => import("../features/table/TableDesignerTab")),
  ddl: lazy(() => import("../features/table/DdlTab")),
  redis: lazy(() => import("../features/redis/RedisTab")),
  "mongo-collection": lazy(() => import("../features/mongo/MongoCollectionTab")),
  "er-diagram": lazy(() => import("../features/er/ErDiagramTab")),
  "schema-compare": lazy(() => import("../features/compare/SchemaCompareTab")),
  "data-transfer": lazy(() => import("../features/transfer/DataTransferTab")),
  history: lazy(() => import("../features/history/HistoryTab")),
  k8s: lazy(() => import("../features/k8s/K8sTab")),
  rabbitmq: lazy(() => import("../features/rabbitmq/RabbitTab")),
  network: lazy(() => import("../features/network/NetworkTab")),
  claude: lazy(() => import("../features/claude/ClaudeTab")),
  file: lazy(() => import("../features/files/FilePreviewTab")),
  git: lazy(() => import("../features/git/GitTab")),
  note: lazy(() => import("../features/notes/NoteTab")),
  terminal: lazy(() => import("../features/terminal/TerminalTab")),
  sftp: lazy(() => import("../features/ssh/SftpTab")),
};

export function TabContent({ tab }: { tab: Tab }) {
  const C = registry[tab.type];
  return (
    <Suspense fallback={<div className="flex h-full items-center justify-center"><Spinner /></div>}>
      <C tab={tab} />
    </Suspense>
  );
}
