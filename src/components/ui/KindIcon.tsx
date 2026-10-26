import { Boxes, Container, Database, Feather, HardDrive, Leaf, Rabbit, Server, SquareTerminal, Zap } from "lucide-react";
import type { ConnectionKind } from "../../lib/types";

export const KIND_META: Record<ConnectionKind, { label: string; color: string; defaultPort?: number }> = {
  postgres: { label: "PostgreSQL", color: "#6ea8fe", defaultPort: 5432 },
  mysql: { label: "MySQL / MariaDB", color: "#f6a35b", defaultPort: 3306 },
  sqlite: { label: "SQLite", color: "#7dd3fc" },
  mssql: { label: "SQL Server", color: "#f87171", defaultPort: 1433 },
  mongodb: { label: "MongoDB", color: "#4ade80", defaultPort: 27017 },
  redis: { label: "Redis", color: "#ef4444", defaultPort: 6379 },
  kubernetes: { label: "Kubernetes", color: "#60a5fa" },
  rabbitmq: { label: "RabbitMQ", color: "#fb923c", defaultPort: 15672 },
  docker: { label: "Docker", color: "#2496ed" },
  ssh: { label: "SSH session", color: "#a3e635", defaultPort: 22 },
};

export function KindIcon({ kind, className = "size-4" }: { kind: ConnectionKind; className?: string }) {
  const style = { color: KIND_META[kind].color };
  switch (kind) {
    case "postgres": return <Database className={className} style={style} />;
    case "mysql": return <Server className={className} style={style} />;
    case "sqlite": return <Feather className={className} style={style} />;
    case "mssql": return <HardDrive className={className} style={style} />;
    case "mongodb": return <Leaf className={className} style={style} />;
    case "redis": return <Zap className={className} style={style} />;
    case "kubernetes": return <Boxes className={className} style={style} />;
    case "rabbitmq": return <Rabbit className={className} style={style} />;
    case "docker": return <Container className={className} style={style} />;
    case "ssh": return <SquareTerminal className={className} style={style} />;
  }
}
