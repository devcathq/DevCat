// Typed wrappers around the docker_* Tauri commands (src-tauri/src/infra/docker.rs).
import { invoke } from "@tauri-apps/api/core";

export interface DockerInfo { version: string; os: string; cpus: number; memory: number; containers: number; running: number; images: number; name: string }
export interface DockerContainer {
  id: string; name: string; image: string; command: string;
  /** running, exited, paused, created, restarting, dead */
  state: string;
  /** "Up 2 hours", "Exited (0) 3 days ago" */
  status: string;
  ports: string; created: string;
  /** Its Compose project and service. */
  project: string | null; service: string | null;
}
export interface DockerStats { id: string; cpu: string; memory: string; memoryPercent: string; net: string; block: string }
export interface DockerImage { id: string; repository: string; tag: string; size: string; created: string }
export interface DockerVolume { name: string; driver: string; scope: string; mountpoint: string }
export interface DockerNetwork { id: string; name: string; driver: string; scope: string }
export type ContainerAction = "start" | "stop" | "restart" | "pause" | "unpause" | "kill" | "remove";
export type DockerObject = "container" | "image" | "volume" | "network";

export const docker = {
  info: (connId: string) => invoke<DockerInfo>("docker_info", { connId }),
  containers: (connId: string) => invoke<DockerContainer[]>("docker_containers", { connId }),
  stats: (connId: string) => invoke<DockerStats[]>("docker_stats", { connId }),
  images: (connId: string) => invoke<DockerImage[]>("docker_images", { connId }),
  volumes: (connId: string) => invoke<DockerVolume[]>("docker_volumes", { connId }),
  networks: (connId: string) => invoke<DockerNetwork[]>("docker_networks", { connId }),
  action: (connId: string, id: string, action: ContainerAction) => invoke<void>("docker_container_action", { connId, id, action }),
  /** Remove an image (`force`: also when tagged twice or used by a stopped container), volume or network. */
  remove: (connId: string, kind: Exclude<DockerObject, "container">, id: string, force = false) => invoke<void>("docker_remove", { connId, kind, id, force }),
  inspect: (connId: string, kind: DockerObject, id: string) => invoke<unknown>("docker_inspect", { connId, kind, id }),
  /** What a terminal tab runs for a shell in the container. */
  shellCommand: (connId: string, id: string) => invoke<{ program: string; args: string[] }>("docker_shell_command", { connId, id }),
  /** Follow its logs; lines come as `docker-log` events for `streamId`. */
  logsStart: (connId: string, id: string, streamId: string, tail: number, timestamps: boolean) => invoke<void>("docker_logs_start", { connId, id, streamId, tail, timestamps }),
  logsStop: (streamId: string) => invoke<void>("docker_logs_stop", { streamId }),
};
