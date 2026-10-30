// Integrations status in every window: read once, then follow the backend's `integrations` events.
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { integrationsApi, type IntegrationsStatus } from "./api";

export const useIntegrations = create<{ status: IntegrationsStatus | null; open: boolean }>(() => ({ status: null, open: false }));

export const openIntegrations = () => useIntegrations.setState({ open: true });
export const closeIntegrations = () => useIntegrations.setState({ open: false });
export const setIntegrations = (status: IntegrationsStatus) => useIntegrations.setState({ status });

let started = false;
export function startIntegrations() {
  if (started) return;
  started = true;
  void integrationsApi.status().then(setIntegrations).catch(() => {});
  void listen<IntegrationsStatus>("integrations", (e) => setIntegrations(e.payload)).catch(() => {});
}
