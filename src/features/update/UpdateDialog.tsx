// "A new version is available" popup: release notes, download progress, restart.
import { openUrl } from "@tauri-apps/plugin-opener";
import { Download, ExternalLink, RotateCw } from "lucide-react";
import { Button, Dialog } from "../../components/ui";
import { Markdown } from "../claude/Markdown";
import { useUpdate } from "./store";

const RELEASES_URL = "https://github.com/devcathq/DevCat/releases/latest";

function fmtMb(n: number) {
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function UpdateDialog() {
  const { open, phase, info, downloaded, total, error, install, later, skip } = useUpdate();
  if (!info) return null;
  const busy = phase === "downloading" || phase === "restarting";
  const pct = total ? Math.min(100, Math.round((downloaded / total) * 100)) : null;

  return (
    <Dialog
      open={open}
      onClose={() => { if (!busy) later(); }}
      title={`DevCat ${info.version} is available`}
      width={520}
      footer={
        busy ? (
          <span className="text-[12px] text-muted">{phase === "restarting" ? "Restarting…" : "Downloading… DevCat restarts when it's done."}</span>
        ) : (
          <>
            <Button variant="ghost" className="mr-auto" onClick={skip} title="Don't remind me about this version">Skip this version</Button>
            <Button variant="ghost" onClick={later}>Later</Button>
            {info.canInstall ? (
              <Button variant="primary" onClick={() => void install()}>
                {phase === "error" ? <RotateCw className="size-3.5" /> : <Download className="size-3.5" />}
                {phase === "error" ? "Try again" : "Update and restart"}
              </Button>
            ) : (
              <Button variant="primary" onClick={() => { void openUrl(RELEASES_URL); later(); }}>
                <ExternalLink className="size-3.5" />Download
              </Button>
            )}
          </>
        )
      }
    >
      <div className="space-y-3 text-[13px]">
        <div className="text-muted">
          You have {info.currentVersion}.
          {info.date && <> Released {new Date(info.date).toLocaleDateString()}.</>}
          {!info.canInstall && <> This installation (.deb / .rpm) can't update itself; download the new package.</>}
        </div>
        {info.notes && (
          <div className="max-h-72 overflow-y-auto rounded-md border border-line bg-app/60 px-3 py-2">
            <Markdown text={info.notes} cwd="" className="text-[12.5px]" />
          </div>
        )}
        {phase === "downloading" && (
          <div>
            <div className="h-1.5 overflow-hidden rounded-full bg-hover">
              <div
                className={pct === null ? "h-full w-1/3 animate-pulse rounded-full bg-accent" : "h-full rounded-full bg-accent transition-[width]"}
                style={pct === null ? undefined : { width: `${pct}%` }}
              />
            </div>
            <div className="mt-1 text-[11px] text-faint tabular-nums">
              {fmtMb(downloaded)}{total ? ` of ${fmtMb(total)}` : ""}{pct !== null ? ` · ${pct}%` : ""}
            </div>
          </div>
        )}
        {phase === "error" && error && <div className="rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-[12px] text-danger">{error}</div>}
      </div>
    </Dialog>
  );
}
