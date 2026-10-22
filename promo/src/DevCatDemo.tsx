// README demo: a title card, then one screen per feature (captured by capture.mjs from demo mode)
// with a caption. Screens hold still between transitions so the GIF compresses well.
import { AbsoluteFill, Easing, Img, Sequence, interpolate, spring, staticFile, useCurrentFrame, useVideoConfig } from "remotion";

export const FPS = 30;
const INTRO = 54;
const SCENE = 84;
const OUTRO = 60;
const T = 12; // transition frames

const SCENES = [
  { shot: "ai", title: "AI workspace", text: "Claude Code, Codex, Gemini… in a real terminal, next to your files" },
  { shot: "diff", title: "Review the changes", text: "Diff, stage and commit what the agent changed" },
  { shot: "table", title: "Databases", text: "Postgres, MySQL, SQL Server, SQLite, MongoDB, Redis · filter any column" },
  { shot: "query", title: "SQL editor", text: "Autocomplete, run with ⌘↵, export results" },
  { shot: "git", title: "Git", text: "History graph, branches, stashes, blame" },
  { shot: "k8s", title: "Kubernetes", text: "Cluster overview, workloads, logs, exec, port-forward" },
  { shot: "network", title: "VPN & tunnels", text: "OpenVPN, WireGuard, SSH and k8s port-forwards with live traffic" },
  { shot: "notes", title: "Notes", text: "Markdown notes per project" },
] as const;

export const totalFrames = () => INTRO + SCENES.length * SCENE + OUTRO;

const BG = "radial-gradient(1200px 700px at 20% 0%, #3b2a12 0%, #15161a 55%, #0e0f12 100%)";
const FONT = "-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', sans-serif";

export const DevCatDemo = () => (
  <AbsoluteFill style={{ background: BG, fontFamily: FONT }}>
    <Sequence durationInFrames={INTRO + T}><Intro /></Sequence>
    {SCENES.map((s, i) => (
      <Sequence key={s.shot} from={INTRO + i * SCENE} durationInFrames={SCENE + T}>
        <Scene {...s} index={i} />
      </Sequence>
    ))}
    <Sequence from={INTRO + SCENES.length * SCENE}><Outro /></Sequence>
  </AbsoluteFill>
);

function Intro() {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const pop = spring({ frame: f, fps, config: { damping: 14 } });
  const fadeOut = interpolate(f, [INTRO - 4, INTRO + T], [1, 0], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const sub = interpolate(f, [10, 24], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  return (
    <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", opacity: fadeOut }}>
      <Img src={staticFile("icon.png")} style={{ width: 150, height: 150, transform: `scale(${pop})` }} />
      <div style={{ color: "#f5f5f4", fontSize: 72, fontWeight: 700, marginTop: 18, transform: `scale(${0.9 + 0.1 * pop})` }}>DevCat</div>
      <div style={{ color: "#a8a29e", fontSize: 28, marginTop: 10, opacity: sub, transform: `translateY(${(1 - sub) * 12}px)` }}>
        AI agents, Git, databases, Kubernetes and VPNs — one app
      </div>
    </AbsoluteFill>
  );
}

function Scene({ shot, title, text, index }: { shot: string; title: string; text: string; index: number }) {
  const f = useCurrentFrame();
  const ease = Easing.bezier(0.2, 0.8, 0.2, 1);
  const enter = interpolate(f, [0, T], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp", easing: ease });
  const exit = interpolate(f, [SCENE, SCENE + T], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp", easing: ease });
  const caption = interpolate(f, [T - 2, T + 8], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp", easing: ease });
  const W = 1056, H = Math.round(W * (900 / 1440));
  return (
    <AbsoluteFill style={{ opacity: 1 - exit }}>
      <div style={{
        position: "absolute", left: (1280 - W) / 2, top: 20, width: W, height: H + 28,
        borderRadius: 12, overflow: "hidden", background: "#16171b",
        boxShadow: "0 30px 80px rgba(0,0,0,.55), 0 0 0 1px rgba(255,255,255,.08)",
        opacity: enter, transform: `translateY(${(1 - enter) * 24}px) scale(${0.98 + 0.02 * enter})`,
      }}>
        <div style={{ height: 28, display: "flex", alignItems: "center", gap: 8, paddingLeft: 12, background: "#1d1e23", borderBottom: "1px solid rgba(255,255,255,.06)" }}>
          {["#ff5f57", "#febc2e", "#28c840"].map((c) => <div key={c} style={{ width: 12, height: 12, borderRadius: 6, background: c }} />)}
          <div style={{ flex: 1, textAlign: "center", color: "#8b8b90", fontSize: 13, marginRight: 60 }}>DevCat</div>
        </div>
        <Img src={staticFile(`shots/${shot}.png`)} style={{ width: W, height: H, display: "block" }} />
      </div>
      <div style={{
        position: "absolute", left: 0, right: 0, bottom: 22, display: "flex", justifyContent: "center",
        opacity: caption, transform: `translateY(${(1 - caption) * 10}px)`,
      }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: 14, padding: "10px 22px", borderRadius: 999, background: "rgba(20,21,25,.92)", boxShadow: "0 0 0 1px rgba(255,255,255,.1)" }}>
          <span style={{ color: "#f59e0b", fontSize: 15, fontWeight: 700, fontVariantNumeric: "tabular-nums" }}>{String(index + 1).padStart(2, "0")}</span>
          <span style={{ color: "#fafaf9", fontSize: 22, fontWeight: 650 }}>{title}</span>
          <span style={{ color: "#a8a29e", fontSize: 18 }}>{text}</span>
        </div>
      </div>
    </AbsoluteFill>
  );
}

function Outro() {
  const f = useCurrentFrame();
  const a = interpolate(f, [0, 14], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  return (
    <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", opacity: a }}>
      <Img src={staticFile("icon.png")} style={{ width: 96, height: 96 }} />
      <div style={{ color: "#f5f5f4", fontSize: 44, fontWeight: 700, marginTop: 14 }}>DevCat</div>
      <div style={{ color: "#a8a29e", fontSize: 22, marginTop: 8 }}>for macOS, Windows and Linux</div>
      <div style={{ color: "#f59e0b", fontSize: 20, marginTop: 14, fontFamily: "ui-monospace, Menlo, monospace" }}>github.com/devcathq/DevCat</div>
    </AbsoluteFill>
  );
}
