// Last-resort error boundary around the whole app. Without it, an error thrown while rendering
// unmounts the UI and leaves an empty window. Instead show what broke, and offer to open a GitHub
// issue already filled in with the details (the user reviews and submits it; nothing is sent).
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AlertTriangle, Bug, Copy, RotateCw } from "lucide-react";
import { Component, useEffect, useState, type ErrorInfo, type ReactNode } from "react";
import { Button } from "./ui";

const NEW_ISSUE_URL = "https://github.com/devcathq/DevCat/issues/new";
// GitHub rejects URLs past ~8 KB, and URL-encoding a stack trace about doubles it.
const MAX_BODY = 3500;

type Crash = { error: Error; componentStack?: string };

export class CrashBoundary extends Component<{ children: ReactNode }, { crash: Crash | null }> {
  state = { crash: null as Crash | null };
  static getDerivedStateFromError(e: unknown) {
    return { crash: { error: e instanceof Error ? e : new Error(String(e)) } };
  }
  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("DevCat crashed", error, info.componentStack);
    this.setState({ crash: { error, componentStack: info.componentStack ?? undefined } });
  }
  render() {
    return this.state.crash ? <CrashScreen crash={this.state.crash} /> : this.props.children;
  }
}

function CrashScreen({ crash }: { crash: Crash }) {
  const [version, setVersion] = useState("unknown");
  const [copied, setCopied] = useState(false);
  useEffect(() => { getVersion().then(setVersion).catch(() => {}); }, []);

  const details = crashDetails(crash, version);
  const report = () => {
    const title = `Crash: ${firstLine(crash.error.message || crash.error.name)}`.slice(0, 120);
    const body = [
      "**What were you doing when DevCat crashed?**",
      "<!-- e.g. opened a table, switched tabs, resized a terminal -->",
      "",
      "",
      "**Details**",
      "```",
      details.length > MAX_BODY ? `${details.slice(0, MAX_BODY)}\n… (truncated)` : details,
      "```",
    ].join("\n");
    const url = `${NEW_ISSUE_URL}?${new URLSearchParams({ title, body, labels: "bug,crash" })}`;
    void openUrl(url).catch(() => {});
  };
  const copy = () => {
    void navigator.clipboard.writeText(details).then(() => setCopied(true), () => {});
  };

  return (
    <div className="flex h-screen items-center justify-center bg-app p-6 text-fg">
      <div className="flex w-full max-w-2xl flex-col gap-3">
        <div className="flex items-center gap-2 text-danger">
          <AlertTriangle className="size-5" />
          <h1 className="text-base font-semibold">DevCat ran into a problem</h1>
        </div>
        <p className="text-[13px] text-muted">
          Something broke while drawing the window. Reloading usually gets you going again; your connections and
          settings are kept. Reporting it on GitHub helps get it fixed: the issue opens in your browser with the
          details below filled in, and you can review it before submitting.
        </p>
        <pre className="selectable max-h-[45vh] overflow-auto rounded-md border border-danger/30 bg-danger/10 p-3 font-mono text-[11.5px] whitespace-pre-wrap text-danger">
          {details}
        </pre>
        <div className="flex items-center gap-2">
          <Button variant="primary" onClick={report}><Bug className="size-3.5" />Report on GitHub</Button>
          <Button onClick={copy}><Copy className="size-3.5" />{copied ? "Copied" : "Copy details"}</Button>
          <div className="flex-1" />
          <Button onClick={() => location.reload()}><RotateCw className="size-3.5" />Reload</Button>
        </div>
      </div>
    </div>
  );
}

function crashDetails({ error, componentStack }: Crash, version: string) {
  return [
    `DevCat ${version} · ${navigator.platform} · ${navigator.userAgent}`,
    "",
    // V8 stacks already start with "Name: message"; WebKit's (macOS, Linux) are only the frames.
    error.stack?.startsWith(`${error.name}`) ? error.stack : `${error.name}: ${error.message}\n${error.stack ?? "(no stack)"}`,
    ...(componentStack ? ["", "Component stack:", componentStack.trim()] : []),
  ].join("\n");
}

function firstLine(s: string) {
  return s.split("\n", 1)[0];
}
