// Theme presets: the colors behind DevCat's design tokens (src/index.css), plus the matching
// terminal (xterm) and editor (Monaco) themes. Dark presets share the text colors; each has its
// own surfaces (tinted) and accent.

export interface Palette {
  id: string;
  name: string;
  dark: boolean;
  app: string;
  panel: string;
  raised: string;
  hover: string;
  line: string;
  fg: string;
  muted: string;
  faint: string;
  accent: string;
  accentFg: string;
  ok: string;
  warn: string;
  danger: string;
  info: string;
}

const DARK_TEXT = { fg: "#e6e8ec", muted: "#8b93a3", faint: "#5c6370", ok: "#4ade80", warn: "#fbbf24", danger: "#f87171", info: "#60a5fa", dark: true };
const LIGHT_TEXT = { ok: "#15803d", warn: "#b45309", danger: "#dc2626", info: "#2563eb", accentFg: "#ffffff", dark: false };

const d = (id: string, name: string, [app, panel, raised, hover, line]: string[], accent: string, accentFg = "#1a1205"): Palette =>
  ({ id, name, app, panel, raised, hover, line, accent, accentFg, ...DARK_TEXT });

export const DARK_PRESETS: Palette[] = [
  d("devcat", "DevCat", ["#0f1114", "#14161a", "#1b1e24", "#232730", "#2a2f39"], "#f5a524"),
  d("black", "Black", ["#000000", "#08090a", "#121315", "#1b1c1f", "#25272b"], "#f5a524"),
  d("slate", "Slate", ["#0e131a", "#131a22", "#1a232d", "#222d39", "#2c3744"], "#38bdf8", "#04131c"),
  d("midnight", "Midnight", ["#0b0d17", "#10131f", "#171b2b", "#1f2438", "#2a3049"], "#818cf8", "#0c0d24"),
  d("teal", "Teal", ["#0a1213", "#0f191b", "#152225", "#1c2c2f", "#26393c"], "#2dd4bf", "#03201c"),
  d("forest", "Forest", ["#0b120e", "#101813", "#17211b", "#1e2a23", "#28362d"], "#4ade80", "#04210f"),
  d("rose", "Rose", ["#140d10", "#1a1215", "#23181c", "#2d1f24", "#3a292f"], "#fb7185", "#2a060e"),
  d("purple", "Purple", ["#100d17", "#15121e", "#1d1829", "#261f35", "#312843"], "#c084fc", "#1d0a2e"),
];

const l = (id: string, name: string, [app, panel, raised, hover, line]: string[], [fg, muted, faint]: string[], accent: string): Palette =>
  ({ id, name, app, panel, raised, hover, line, fg, muted, faint, accent, ...LIGHT_TEXT });

export const LIGHT_PRESETS: Palette[] = [
  l("light", "Light", ["#eef0f3", "#fbfbfc", "#eceef2", "#e2e5ea", "#d5d9e0"], ["#1b1f27", "#5b6372", "#8c93a1"], "#c2410c"),
  l("paper", "Paper", ["#efebe2", "#faf8f3", "#ece7dc", "#e2dccd", "#d5ccb9"], ["#2a2620", "#6b6457", "#9a927f"], "#b45309"),
  l("cloud", "Cloud", ["#e9eef5", "#f8fafc", "#e6ecf3", "#dae2ec", "#cbd5e1"], ["#152033", "#53617a", "#8592a8"], "#2563eb"),
];

export function findPreset(id: string, dark: boolean): Palette {
  const list = dark ? DARK_PRESETS : LIGHT_PRESETS;
  return list.find((p) => p.id === id) ?? list[0];
}

/** `#rrggbb` + alpha → `rgba()`. */
export function withAlpha(hex: string, a: number): string {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${Math.max(0, Math.min(1, a)).toFixed(3)})`;
}

/** Terminal colors (xterm) for a palette, on the work area's color; `glass`: a see-through background (the window's tint
 *  shows instead; the terminal needs `allowTransparency`). */
export function terminalTheme(p: Palette, glass = false) {
  const base = {
    // the work area's color (editors use it too), so a terminal isn't a darker box in it
    background: glass ? "rgba(0, 0, 0, 0)" : p.panel,
    foreground: p.fg,
    cursor: p.accent,
    cursorAccent: p.panel,
    selectionBackground: withAlpha(p.accent, p.dark ? 0.25 : 0.22),
    selectionInactiveBackground: withAlpha(p.accent, 0.14),
  };
  return p.dark
    ? {
      ...base,
      black: p.raised, red: "#f87171", green: "#4ade80", yellow: "#fbbf24", blue: "#60a5fa", magenta: "#c084fc", cyan: "#22d3ee", white: "#c9cdd4",
      brightBlack: p.faint, brightRed: "#fca5a5", brightGreen: "#86efac", brightYellow: "#fde68a", brightBlue: "#93c5fd", brightMagenta: "#d8b4fe", brightCyan: "#67e8f9", brightWhite: "#f3f4f6",
    }
    : {
      ...base,
      black: "#1f2328", red: "#cf222e", green: "#116329", yellow: "#9a6700", blue: "#0969da", magenta: "#8250df", cyan: "#1b7c83", white: "#6e7781",
      brightBlack: "#57606a", brightRed: "#a40e26", brightGreen: "#1a7f37", brightYellow: "#7d4e00", brightBlue: "#218bff", brightMagenta: "#a475f9", brightCyan: "#3192aa", brightWhite: "#8c959f",
    };
}

const hex6 = (c: string) => c.replace("#", "").toUpperCase();

/** The Monaco theme "devcat" for a palette (`glass`: see-through editor background). */
export function monacoTheme(p: Palette, glass = false) {
  const bg = glass ? "#00000000" : p.panel;
  return {
    base: (p.dark ? "vs-dark" : "vs") as "vs-dark" | "vs",
    inherit: true,
    rules: p.dark
      ? [
        { token: "keyword", foreground: hex6(p.accent), fontStyle: "bold" },
        { token: "string", foreground: "8BD49C" },
        { token: "number", foreground: "7CC4FA" },
        { token: "comment", foreground: "6B7280", fontStyle: "italic" },
      ]
      : [
        { token: "keyword", foreground: hex6(p.accent), fontStyle: "bold" },
        { token: "string", foreground: "116329" },
        { token: "number", foreground: "0550AE" },
        { token: "comment", foreground: "6E7781", fontStyle: "italic" },
      ],
    colors: {
      "editor.background": bg,
      "editor.lineHighlightBackground": glass ? (p.dark ? "#ffffff0d" : "#0000000a") : p.raised,
      "editorLineNumber.foreground": p.faint,
      "editorGutter.background": bg,
      "minimap.background": bg,
      "editorWidget.background": p.raised,
      "editorWidget.border": p.line,
      "editorSuggestWidget.background": p.raised,
      "editorSuggestWidget.border": p.line,
      "editorSuggestWidget.selectedBackground": p.hover,
    },
  };
}
