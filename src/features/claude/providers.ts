// AI connections: which coding-agent CLI the AI workspace terminal runs (Claude Code by default).
// Profiles live in the backend (API keys in the OS keychain, never sent back here); the selected
// one is persisted per machine in localStorage and used for new chats.
import { create } from "zustand";
import { api, errMsg } from "../../lib/api";
import type { AgentSettings, AiDetect, AiKind, AiProvider } from "../../lib/types";

/** Id of the implicit Claude Code profile. */
export const CLAUDE_PROVIDER_ID = "claude";
const KEY_SELECTED = "devcat.ai.provider";

export interface KindInfo {
  label: string;
  /** Default binary. */
  program: string;
  /** Conventional API key variable (null = user picks). */
  keyEnv: string | null;
  /** Let the user choose the key variable (provider supports several vendors). */
  keyEnvEditable: boolean;
  install: string | null;
  /** How the CLI logs in when no API key is set. */
  login: string;
}

export const KINDS: Record<AiKind, KindInfo> = {
  claude: { label: "Claude Code", program: "claude", keyEnv: "ANTHROPIC_API_KEY", keyEnvEditable: false, install: "curl -fsSL https://claude.ai/install.sh | bash", login: "Run claude once in a terminal to log in." },
  codex: { label: "OpenAI Codex", program: "codex", keyEnv: "OPENAI_API_KEY", keyEnvEditable: false, install: "npm i -g @openai/codex", login: "Run codex login once, or set an API key." },
  gemini: { label: "Gemini CLI", program: "gemini", keyEnv: "GEMINI_API_KEY", keyEnvEditable: false, install: "npm i -g @google/gemini-cli", login: "Sign in with Google on first start, or set an API key." },
  copilot: { label: "GitHub Copilot", program: "copilot", keyEnv: "GH_TOKEN", keyEnvEditable: true, install: "npm i -g @github/copilot", login: "Use /login on first start, or set a GitHub token." },
  aider: { label: "Aider", program: "aider", keyEnv: "OPENAI_API_KEY", keyEnvEditable: true, install: "pip install aider-chat", login: "Aider reads the API key of the model's vendor." },
  opencode: { label: "opencode", program: "opencode", keyEnv: "OPENAI_API_KEY", keyEnvEditable: true, install: "npm i -g opencode-ai", login: "Run opencode auth login once, or set an API key." },
  custom: { label: "Custom", program: "", keyEnv: null, keyEnvEditable: true, install: null, login: "Any CLI that runs in a terminal." },
  agent: { label: "DevCat Agent", program: "", keyEnv: null, keyEnvEditable: false, install: null, login: "Built in: talks to a model API directly. Nothing to install." },
};

export const KIND_ORDER: AiKind[] = ["claude", "agent", "codex", "gemini", "copilot", "aider", "opencode", "custom"];

export const builtinClaude = (): AiProvider => ({ id: CLAUDE_PROVIDER_ID, name: "Claude Code", kind: "claude", args: [], env: [], builtin: true });

export const blankProvider = (kind: AiKind): AiProvider => ({
  id: "", name: KINDS[kind].label, kind, command: null, args: [], env: [],
  ...(kind === "agent" ? { agent: { ...AGENT_PRESETS[0].settings } } : {}),
});

/** Model APIs offered when setting up DevCat Agent (all editable afterwards). */
export const AGENT_PRESETS: { label: string; settings: AgentSettings; keyHint: string }[] = [
  // Z.ai's Anthropic-style endpoint caches the conversation (90%+ of input tokens in our tests),
  // its OpenAI-style one much less.
  { label: "Z.ai GLM Coding Plan", settings: { protocol: "anthropic", baseUrl: "https://api.z.ai/api/anthropic", model: "glm-5.3-flash" }, keyHint: "Your Z.ai API key (z.ai → API keys)." },
  { label: "Z.ai GLM (OpenAI-style)", settings: { protocol: "openai-chat", baseUrl: "https://api.z.ai/api/coding/paas/v4", model: "glm-5.3-flash" }, keyHint: "Your Z.ai API key. The Anthropic-style preset caches more." },
  { label: "DeepSeek", settings: { protocol: "openai-chat", baseUrl: "https://api.deepseek.com/v1", model: "deepseek-chat" }, keyHint: "From platform.deepseek.com." },
  { label: "OpenAI", settings: { protocol: "openai-chat", baseUrl: "https://api.openai.com/v1", model: "gpt-5" }, keyHint: "From platform.openai.com." },
  { label: "OpenAI (Responses API)", settings: { protocol: "openai-responses", baseUrl: "https://api.openai.com/v1", model: "gpt-5-codex" }, keyHint: "From platform.openai.com. Also for the models only on the Responses API, such as gpt-5-codex." },
  { label: "Anthropic", settings: { protocol: "anthropic", baseUrl: "https://api.anthropic.com", model: "claude-sonnet-5-5" }, keyHint: "From console.anthropic.com." },
  { label: "OpenRouter", settings: { protocol: "openai-chat", baseUrl: "https://openrouter.ai/api/v1", model: "deepseek/deepseek-chat" }, keyHint: "From openrouter.ai/keys." },
  { label: "Kimi (Moonshot)", settings: { protocol: "openai-chat", baseUrl: "https://api.moonshot.ai/v1", model: "kimi-k2-turbo-preview" }, keyHint: "From platform.moonshot.ai." },
  { label: "Ollama (this computer)", settings: { protocol: "openai-chat", baseUrl: "http://127.0.0.1:11434/v1", model: "qwen3:8b" }, keyHint: "No key needed." },
  { label: "LM Studio (this computer)", settings: { protocol: "openai-chat", baseUrl: "http://127.0.0.1:1234/v1", model: "" }, keyHint: "No key needed." },
];

interface ProvidersState {
  providers: AiProvider[];
  loaded: boolean;
  error: string | null;
  /** Install status of the built-in CLIs (by kind). */
  detect: Partial<Record<AiKind, AiDetect>>;
  detecting: boolean;
  /** Provider for new chats. */
  selectedId: string;
  /** "Manage AI connections" dialog: null = closed; editId = profile to select, newKind = start a new one. */
  manage: { editId: string; newKind?: AiKind } | null;
}

function loadSelected(): string {
  try { return localStorage.getItem(KEY_SELECTED) || CLAUDE_PROVIDER_ID; } catch { return CLAUDE_PROVIDER_ID; }
}

export const useProviders = create<ProvidersState>(() => ({
  providers: [builtinClaude()],
  loaded: false,
  error: null,
  detect: {},
  detecting: false,
  selectedId: loadSelected(),
  manage: null,
}));

let loading: Promise<void> | null = null;
let watching = false;

export function loadProviders(force = false): Promise<void> {
  if (!watching) {
    watching = true;
    // Other windows may have edited the profiles / selection.
    window.addEventListener("focus", () => void loadProviders(true));
    window.addEventListener("storage", (e) => { if (e.key === KEY_SELECTED) useProviders.setState({ selectedId: loadSelected() }); });
  }
  if (useProviders.getState().loaded && !force) return Promise.resolve();
  if (loading) return loading;
  loading = api.aiProvidersList()
    .then((providers) => useProviders.setState({ providers, loaded: true, error: null }))
    .catch((e) => useProviders.setState({ loaded: true, error: errMsg(e) }))
    .finally(() => { loading = null; });
  return loading;
}

let detected = false;
export async function detectProviders(force = false) {
  if (detected && !force) return;
  detected = true;
  useProviders.setState({ detecting: true });
  try {
    const list = await api.aiProviderDetect();
    useProviders.setState({ detect: Object.fromEntries(list.map((d) => [d.kind, d])) });
  } catch { /* backend not ready: status stays unknown */ } finally {
    useProviders.setState({ detecting: false });
  }
}

export function selectProvider(id: string) {
  try { localStorage.setItem(KEY_SELECTED, id); } catch { /* private mode */ }
  useProviders.setState({ selectedId: id });
}

export async function saveProvider(p: AiProvider): Promise<AiProvider> {
  const saved = await api.aiProviderSave(p);
  await loadProviders(true);
  return saved;
}

export async function deleteProvider(id: string) {
  await api.aiProviderDelete(id);
  if (useProviders.getState().selectedId === id) selectProvider(CLAUDE_PROVIDER_ID);
  await loadProviders(true);
}

export const openManageProviders = (editId = "", newKind?: AiKind) => useProviders.setState({ manage: { editId, newKind } });
export const closeManageProviders = () => useProviders.setState({ manage: null });

/** Profile by id; the built-in Claude Code profile is known before the list loads. */
export function findProvider(providers: AiProvider[], id: string | null | undefined): AiProvider | null {
  const want = id || CLAUDE_PROVIDER_ID;
  return providers.find((p) => p.id === want) ?? (want === CLAUDE_PROVIDER_ID ? builtinClaude() : null);
}

/** Selected provider for new chats (falls back to Claude Code if it was deleted). */
export function useSelectedProvider(): AiProvider {
  const providers = useProviders((s) => s.providers);
  const selectedId = useProviders((s) => s.selectedId);
  return findProvider(providers, selectedId) ?? builtinClaude();
}

/** Profile of a tab: undefined while the list is loading, null if it was deleted. */
export function useProvider(id: string | null | undefined): AiProvider | null | undefined {
  const providers = useProviders((s) => s.providers);
  const loaded = useProviders((s) => s.loaded);
  return findProvider(providers, id) ?? (loaded ? null : undefined);
}

export type InstallState = "installed" | "missing" | "unknown";

/** Whether a profile's CLI is installed (only known for profiles that use the default binary). */
export function installState(p: AiProvider, detect: Partial<Record<AiKind, AiDetect>>): InstallState {
  if (p.kind === "agent") return "installed";
  if (p.kind === "custom" || p.command) return "unknown";
  const d = detect[p.kind];
  if (!d) return "unknown";
  return d.path ? "installed" : "missing";
}

/** Prompt sent into the terminal by "Review" for providers without a built-in review. */
export const REVIEW_PROMPT = "Review the uncommitted changes in this repo (git diff) and list bugs, risks and suggested fixes.";

/** Shell-like split of an args line: whitespace separated, '…' / "…" quoting, backslash escapes. */
export function splitArgs(s: string): string[] {
  const out: string[] = [];
  let cur = "";
  let has = false;
  let q: string | null = null;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (q) {
      if (c === q) q = null;
      else if (c === "\\" && q === '"' && i + 1 < s.length) cur += s[++i];
      else cur += c;
    } else if (c === '"' || c === "'") {
      q = c;
      has = true;
    } else if (/\s/.test(c)) {
      if (has) out.push(cur);
      cur = "";
      has = false;
    } else if (c === "\\" && i + 1 < s.length) {
      cur += s[++i];
      has = true;
    } else {
      cur += c;
      has = true;
    }
  }
  if (has) out.push(cur);
  return out;
}

/** Inverse of splitArgs (single-quotes anything unusual). */
export function joinArgs(args: string[]): string {
  return args.map((a) => (/^[\w@%+=:,./-]+$/.test(a) ? a : `'${a.replace(/'/g, "'\\''")}'`)).join(" ");
}
