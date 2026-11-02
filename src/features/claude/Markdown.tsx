// Markdown renderer for assistant messages and review findings (GFM: tables, task lists, …).
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { Check, Copy, Image as ImageIcon } from "lucide-react";
import { memo, useState, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { copyText, looksLikePath, openFile } from "./util";

export function CopyButton({ text, className, label }: { text: string; className?: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      title="Copy"
      className={clsx("inline-flex items-center gap-1 rounded px-1 text-[11px] text-muted hover:bg-hover hover:text-fg", className)}
      onClick={(e) => {
        e.stopPropagation();
        void copyText(text);
        setDone(true);
        setTimeout(() => setDone(false), 1200);
      }}
    >
      {done ? <Check className="size-3 text-ok" /> : <Copy className="size-3" />}
      {label}
    </button>
  );
}

export function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  return (
    <div className="group relative my-2 overflow-hidden rounded-md border border-line bg-app">
      <div className="flex h-6 items-center justify-between border-b border-line/60 px-2 text-[10px] uppercase tracking-wide text-faint">
        <span>{lang || "code"}</span>
        <CopyButton text={code} className="opacity-60 group-hover:opacity-100" />
      </div>
      <pre className="selectable overflow-x-auto p-2.5 font-mono text-xs leading-relaxed text-fg/90"><code>{code}</code></pre>
    </div>
  );
}

/**
 * An image in Markdown that comes from outside DevCat (an agent's answer, a repository's file, a
 * room's, a note): only a `data:` / `blob:` one shows by itself. Loading any other would send its
 * address to that server without a click, with whatever an agent put in it (told to by a ticket,
 * a web page or a database row); and a `//host/…` one makes Windows log in to that host (SMB).
 * Those show as a button that opens the address in the browser.
 */
export function OutsideImage({ src, alt }: { src?: string; alt?: string }) {
  const s = src ?? "";
  if (/^(data:image\/|blob:)/i.test(s)) return <img src={s} alt={alt ?? ""} className="inline max-w-full" />;
  const web = /^https?:\/\//i.test(s);
  let host = "";
  try {
    if (web) host = new URL(s).host;
  } catch {
    /* not a URL */
  }
  return (
    <button
      type="button"
      title={web ? `Not loaded by itself: open ${s} in the browser` : s}
      disabled={!web}
      className="inline-flex max-w-full items-center gap-1 rounded border border-line px-1.5 py-px align-middle text-xs text-muted enabled:hover:bg-hover enabled:hover:text-fg"
      onClick={() => web && void openUrl(s).catch(() => {})}
    >
      <ImageIcon className="size-3.5 shrink-0" />
      <span className="truncate">{alt || "Image"}{host ? ` · ${host}` : ""}</span>
    </button>
  );
}

function textOf(n: ReactNode): string {
  if (typeof n === "string" || typeof n === "number") return String(n);
  if (Array.isArray(n)) return n.map(textOf).join("");
  if (n && typeof n === "object" && "props" in n) return textOf((n as { props: { children?: ReactNode } }).props.children);
  return "";
}

function makeComponents(cwd: string): Components {
  return {
    img: ({ src, alt }) => <OutsideImage src={typeof src === "string" ? src : undefined} alt={alt} />,
    pre: ({ children }) => {
      const child = Array.isArray(children) ? children[0] : children;
      const cls = child && typeof child === "object" && "props" in child ? String((child as { props: { className?: string } }).props.className ?? "") : "";
      const lang = /language-([\w+-]+)/.exec(cls)?.[1];
      return <CodeBlock code={textOf(children).replace(/\n$/, "")} lang={lang} />;
    },
    code: ({ children, className }) => {
      const text = textOf(children);
      if (className) return <code className={className}>{children}</code>;
      if (looksLikePath(text)) {
        return (
          <code
            className="cursor-pointer rounded bg-hover px-1 py-px font-mono text-[0.88em] text-info hover:underline"
            title="Open file"
            onClick={() => openFile(cwd, text.replace(/:\d+$/, ""))}
          >
            {children}
          </code>
        );
      }
      return <code className="rounded bg-hover px-1 py-px font-mono text-[0.88em] text-accent/90">{children}</code>;
    },
    a: ({ href, children }) => (
      <a
        href={href}
        className="text-info hover:underline"
        onClick={(e) => {
          e.preventDefault();
          if (!href) return;
          if (/^https?:|^mailto:/.test(href)) void openUrl(href).catch(() => {});
          else openFile(cwd, href.replace(/^file:\/\//, "").replace(/#.*$/, ""));
        }}
      >
        {children}
      </a>
    ),
    p: ({ children }) => <p className="my-1.5 leading-relaxed">{children}</p>,
    ul: ({ children, className }) => <ul className={clsx("my-1.5 pl-5", className?.includes("contains-task-list") ? "list-none pl-1" : "list-disc")}>{children}</ul>,
    ol: ({ children }) => <ol className="my-1.5 list-decimal pl-5">{children}</ol>,
    li: ({ children }) => <li className="my-0.5 leading-relaxed">{children}</li>,
    h1: ({ children }) => <h1 className="mb-1.5 mt-3 text-base font-semibold">{children}</h1>,
    h2: ({ children }) => <h2 className="mb-1.5 mt-3 text-[14px] font-semibold">{children}</h2>,
    h3: ({ children }) => <h3 className="mb-1 mt-2.5 text-[13px] font-semibold">{children}</h3>,
    h4: ({ children }) => <h4 className="mb-1 mt-2 text-[13px] font-semibold text-muted">{children}</h4>,
    blockquote: ({ children }) => <blockquote className="my-2 border-l-2 border-line pl-3 text-muted">{children}</blockquote>,
    hr: () => <hr className="my-3 border-line" />,
    table: ({ children }) => (
      <div className="my-2 overflow-x-auto">
        <table className="border-collapse text-xs">{children}</table>
      </div>
    ),
    th: ({ children, style }) => <th style={style} className="border border-line bg-raised px-2 py-1 text-left font-semibold">{children}</th>,
    td: ({ children, style }) => <td style={style} className="border border-line px-2 py-1 align-top">{children}</td>,
    input: ({ checked }) => <input type="checkbox" checked={!!checked} readOnly className="mr-1.5 align-middle accent-[var(--color-accent)]" />,
  };
}

const PLUGINS = [remarkGfm];
const cache = new Map<string, Components>();
function componentsFor(cwd: string) {
  let c = cache.get(cwd);
  if (!c) cache.set(cwd, (c = makeComponents(cwd)));
  return c;
}

export const Markdown = memo(function Markdown({ text, cwd, className }: { text: string; cwd: string; className?: string }) {
  return (
    <div className={clsx("selectable min-w-0 break-words text-[13px] text-fg", className)}>
      <ReactMarkdown remarkPlugins={PLUGINS} components={componentsFor(cwd)}>{text}</ReactMarkdown>
    </div>
  );
});
