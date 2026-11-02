// Document-style Markdown rendering for file previews (GFM). Relative links/images resolve against the file's folder.
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { ImageOff } from "lucide-react";
import { memo, useEffect, useMemo, useState, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { api } from "../../../lib/api";
import { OutsideImage } from "../../claude/Markdown";
import { dirname, openFilePreview } from "../format";

/** Resolve `rel` against directory `base` (handles ./ and ../). */
export function resolvePath(base: string, rel: string): string {
  if (rel.startsWith("/")) return rel;
  const parts = base.split("/");
  for (const seg of rel.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") parts.length > 1 && parts.pop();
    else parts.push(seg);
  }
  return parts.join("/");
}

const EXTERNAL = /^(https?:|mailto:|data:)/i;

function LocalImage({ path, alt }: { path: string; alt?: string }) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let live = true;
    api.fsPreview(path)
      .then((p) => {
        if (!live) return;
        if (p.kind === "image" && p.dataBase64) setSrc(`data:${p.mime ?? "image/png"};base64,${p.dataBase64}`);
        else setFailed(true);
      })
      .catch(() => live && setFailed(true));
    return () => { live = false; };
  }, [path]);
  if (failed) return <span className="inline-flex items-center gap-1 text-xs text-faint" title={path}><ImageOff className="size-3.5" /> {alt || path}</span>;
  if (!src) return <span className="inline-block h-6 w-24 animate-pulse rounded bg-hover align-middle" />;
  return <img src={src} alt={alt} className="inline max-w-full" />;
}

function textOf(n: ReactNode): string {
  if (typeof n === "string" || typeof n === "number") return String(n);
  if (Array.isArray(n)) return n.map(textOf).join("");
  if (n && typeof n === "object" && "props" in n) return textOf((n as { props: { children?: ReactNode } }).props.children);
  return "";
}

function makeComponents(filePath: string, root: string | null): Components {
  const dir = dirname(filePath);
  return {
    a: ({ href, children }) => (
      <a
        href={href}
        title={href}
        className="text-info hover:underline"
        onClick={(e) => {
          e.preventDefault();
          if (!href || href.startsWith("#")) {
            if (href) document.getElementById(decodeURIComponent(href.slice(1)))?.scrollIntoView({ behavior: "smooth" });
            return;
          }
          if (EXTERNAL.test(href)) void openUrl(href).catch(() => {});
          else openFilePreview(resolvePath(dir, decodeURIComponent(href.replace(/^file:\/\//, "").replace(/[#?].*$/, ""))), root);
        }}
      >
        {children}
      </a>
    ),
    img: ({ src, alt }) => {
      const s = typeof src === "string" ? src : "";
      // web, `//host` and other schemes don't load by themselves (see OutsideImage); a drive letter is a path
      if (!s || EXTERNAL.test(s) || /^[\\/]{2}/.test(s) || /^[a-z][a-z0-9+.-]+:/i.test(s)) return <OutsideImage src={s} alt={alt} />;
      const path = resolvePath(dir, decodeURIComponent(s.replace(/[#?].*$/, "")));
      // `/\host\share` is a network path on Windows too
      if (/^[\\/]{2}/.test(path)) return <OutsideImage src={s} alt={alt} />;
      return <LocalImage path={path} alt={alt} />;
    },
    pre: ({ children }) => (
      <pre className="selectable my-3 overflow-x-auto rounded-md border border-line bg-app p-3 font-mono text-xs leading-relaxed text-fg/90">{children}</pre>
    ),
    code: ({ children, className }) =>
      className ? <code className={className}>{children}</code>
        : <code className="rounded bg-hover px-1 py-px font-mono text-[0.88em] text-accent/90">{children}</code>,
    h1: ({ children }) => <h1 id={slug(children)} className="mt-6 mb-3 border-b border-line pb-1.5 text-2xl font-semibold first:mt-0">{children}</h1>,
    h2: ({ children }) => <h2 id={slug(children)} className="mt-6 mb-2.5 border-b border-line pb-1 text-xl font-semibold first:mt-0">{children}</h2>,
    h3: ({ children }) => <h3 id={slug(children)} className="mt-5 mb-2 text-base font-semibold">{children}</h3>,
    h4: ({ children }) => <h4 id={slug(children)} className="mt-4 mb-1.5 text-sm font-semibold">{children}</h4>,
    h5: ({ children }) => <h5 id={slug(children)} className="mt-3 mb-1 text-[13px] font-semibold">{children}</h5>,
    h6: ({ children }) => <h6 id={slug(children)} className="mt-3 mb-1 text-[13px] font-semibold text-muted">{children}</h6>,
    p: ({ children }) => <p className="my-2.5 leading-relaxed">{children}</p>,
    ul: ({ children, className }) => <ul className={clsx("my-2.5 pl-6", className?.includes("contains-task-list") ? "list-none pl-1" : "list-disc")}>{children}</ul>,
    ol: ({ children }) => <ol className="my-2.5 list-decimal pl-6">{children}</ol>,
    li: ({ children }) => <li className="my-1 leading-relaxed">{children}</li>,
    blockquote: ({ children }) => <blockquote className="my-3 border-l-3 border-line pl-4 text-muted">{children}</blockquote>,
    hr: () => <hr className="my-6 border-line" />,
    table: ({ children }) => <div className="my-3 overflow-x-auto"><table className="border-collapse text-[13px]">{children}</table></div>,
    th: ({ children, style }) => <th style={style} className="border border-line bg-raised px-3 py-1.5 text-left font-semibold">{children}</th>,
    td: ({ children, style }) => <td style={style} className="border border-line px-3 py-1.5 align-top">{children}</td>,
    input: ({ checked }) => <input type="checkbox" checked={!!checked} readOnly className="mr-1.5 align-middle accent-[var(--color-accent)]" />,
  };
}

function slug(children: ReactNode): string {
  return textOf(children).toLowerCase().trim().replace(/[^\p{L}\p{N}\s-]/gu, "").replace(/\s+/g, "-");
}

const PLUGINS = [remarkGfm];

export const MarkdownDoc = memo(function MarkdownDoc({ text, path, root }: { text: string; path: string; root: string | null }) {
  const components = useMemo(() => makeComponents(path, root), [path, root]);
  return (
    <div className="h-full overflow-auto">
      <article className="selectable mx-auto max-w-3xl px-8 py-6 text-[14px] break-words text-fg">
        <ReactMarkdown remarkPlugins={PLUGINS} components={components}>{text}</ReactMarkdown>
      </article>
    </div>
  );
});
