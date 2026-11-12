// The HTML preview is a sandboxed `srcdoc` frame: it has no address of its own, so a page's
// relative `style.css`, `app.js` or `logo.png` can't load and the page shows unstyled (or empty, when
// JavaScript builds it). `inlineAssets` swaps those references for `data:` URLs of the files, read by
// `fs_html_assets` (inside the page's folder / the tab's folder, no hidden or secret-looking files).
// Stylesheets get the same for their `url(…)` and `@import`, module scripts for their `import`s.
// Pure (no DevCat calls), so `htmlAssets.check.ts` runs it under node.

export type Load = (ref: string) => Promise<string | null>;

/** A reference to a file next to the page: not a URL with a scheme, not `//host`, `/abs` or `#x`. */
export function isRelative(ref: string): boolean {
  const r = ref.trim();
  return !!r && !/^[a-z][a-z0-9+.-]*:/i.test(r) && !/^[\\/#?]/.test(r) && !r.startsWith("{{");
}

/** `ref` without its query and fragment, percent-decoded, `.` / `..` applied (one key per file). */
function filePart(ref: string): string {
  let r = ref.trim().replace(/[?#].*$/, "");
  try { r = decodeURIComponent(r); } catch { /* keep it as written */ }
  return joinRel("", r);
}

/** `spec` (relative) from the folder of `from` (a page-relative ref), `.` / `..` applied; leading `..` kept. */
export function joinRel(from: string, spec: string): string {
  const out = from.split("/").slice(0, -1);
  for (const seg of spec.split("/")) {
    if (!seg || seg === ".") continue;
    if (seg === ".." && out.length && out[out.length - 1] !== "..") out.pop();
    else out.push(seg);
  }
  return out.join("/");
}

function decodeData(url: string): string {
  const b64 = url.slice(url.indexOf(",") + 1);
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

function encodeData(mime: string, text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return `data:${mime};base64,${btoa(bin)}`;
}

/** Replace each match's relative reference (named group `ref`, everything before it in `pre`) with what `map` gives, keeping the rest. */
async function replaceAsync(text: string, re: RegExp, map: (ref: string) => Promise<string | null>): Promise<string> {
  const parts: (string | Promise<string>)[] = [];
  let last = 0;
  for (const m of text.matchAll(re)) {
    const ref = m.groups!.ref;
    const at = m.index! + m.groups!.pre.length, end = at + ref.length;
    parts.push(text.slice(last, at));
    parts.push(isRelative(ref) ? map(ref).then((u) => u ?? ref, () => ref) : ref);
    last = end;
  }
  parts.push(text.slice(last));
  return (await Promise.all(parts)).join("");
}

const CSS_URL = /(?<pre>url\(\s*(['"]?))(?<ref>[^'")\s]+)\2\s*\)/gi;
const CSS_IMPORT = /(?<pre>@import\s+(['"]))(?<ref>[^'"]+)\2/gi;
// `import x from "./a.js"`, `export { y } from "./a.js"`, `import "./a.js"`, `import("./a.js")`
const JS_IMPORT = /(?<pre>(?:\bfrom\s*|\bimport\s*\(?\s*)(['"]))(?<ref>\.{1,2}\/[^'"\n]+)\2/g;

class Inliner {
  private files = new Map<string, Promise<string | null>>();
  private css = new Map<string, Promise<string | null>>();
  private mods = new Map<string, Promise<string | null>>();
  private load: Load;
  constructor(load: Load) { this.load = load; }

  file(ref: string): Promise<string | null> {
    const key = filePart(ref);
    let p = this.files.get(key);
    if (!p) this.files.set(key, (p = this.load(key)));
    return p;
  }

  /** A stylesheet (page-relative `ref`) with its own references inlined. */
  stylesheet(ref: string, depth = 0): Promise<string | null> {
    const key = filePart(ref);
    let p = this.css.get(key);
    if (!p) {
      p = this.file(key).then(async (url) => {
        if (!url || depth > 4) return url;
        const text = await this.cssText(decodeData(url), key, depth);
        return encodeData("text/css;charset=utf-8", text);
      });
      this.css.set(key, p);
    }
    return p;
  }

  /** CSS text whose references are relative to `from` (a page-relative path; "" for the page itself). */
  async cssText(text: string, from: string, depth = 0): Promise<string> {
    const withImports = await replaceAsync(text, CSS_IMPORT, (r) => this.stylesheet(joinRel(from, r), depth + 1));
    return replaceAsync(withImports, CSS_URL, (r) => this.file(joinRel(from, r)));
  }

  /** A module script with its relative `import`s inlined (an import cycle stays as written). */
  module(ref: string, seen: string[] = []): Promise<string | null> {
    const key = filePart(ref);
    if (seen.includes(key)) return Promise.resolve(null);
    let p = this.mods.get(key);
    if (!p) {
      p = this.file(key).then(async (url) => {
        if (!url) return null;
        const text = await this.jsText(decodeData(url), key, [...seen, key]);
        return encodeData("text/javascript;charset=utf-8", text);
      });
      this.mods.set(key, p);
    }
    return p;
  }

  jsText(text: string, from: string, seen: string[] = []): Promise<string> {
    return replaceAsync(text, JS_IMPORT, (r) => this.module(joinRel(from, r), seen));
  }
}

const TAG = /<(link|script|img|source|video|audio|input)\b[^>]*>/gi;
const STYLE_BLOCK = /(<style\b[^>]*>)([\s\S]*?)(<\/style>)/gi;
const MODULE_BLOCK = /(<script\b[^>]*\btype\s*=\s*["']?module["']?[^>]*>)([\s\S]*?)(<\/script>)/gi;

function attr(tag: string, name: string): string | null {
  const m = tag.match(new RegExp(`\\s${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s>]+))`, "i"));
  return m ? (m[1] ?? m[2] ?? m[3]) : null;
}

function setAttr(tag: string, name: string, value: string): string {
  return tag.replace(new RegExp(`(\\s${name}\\s*=\\s*)(?:"[^"]*"|'[^']*'|[^\\s>]+)`, "i"), (_, pre: string) => `${pre}"${value}"`);
}

/**
 * `html` with the relative stylesheets, images, icons and (when `scripts`) scripts it loads
 * swapped for `data:` URLs from `load` (a page-relative path → data URL, or null to leave it).
 */
export async function inlineAssets(html: string, scripts: boolean, load: Load): Promise<string> {
  const il = new Inliner(load);
  /** the new tag for one found in the page, or what to fetch for it */
  const what = (tag: string, name: string): [attr: string, url: Promise<string | null>] | null => {
    if (name === "link") {
      const href = attr(tag, "href");
      const rel = (attr(tag, "rel") ?? "").toLowerCase().split(/\s+/);
      if (!href || !isRelative(href)) return null;
      if (rel.includes("stylesheet")) return ["href", il.stylesheet(href)];
      if (rel.includes("icon")) return ["href", il.file(href)];
      return null;
    }
    const src = attr(tag, "src");
    if (!src || !isRelative(src)) return null;
    if (name !== "script") return ["src", il.file(src)];
    if (!scripts) return null;
    return ["src", /^module$/i.test(attr(tag, "type") ?? "") ? il.module(src) : il.file(src)];
  };
  const tags: Promise<string>[] = [];
  const marked = html.replace(TAG, (tag, name: string) => {
    const w = what(tag, name.toLowerCase());
    if (!w) return tag;
    tags.push(w[1].then((u) => (u ? setAttr(tag, w[0], u) : tag), () => tag));
    return `\u0000devcat-asset-${tags.length - 1}\u0000`;
  });
  const done = await Promise.all(tags);
  let out = marked.replace(/\u0000devcat-asset-(\d+)\u0000/g, (k, n: string) => done[Number(n)] ?? k);
  out = await replaceBlocks(out, STYLE_BLOCK, (css) => il.cssText(css, ""));
  if (scripts) out = await replaceBlocks(out, MODULE_BLOCK, (js) => il.jsText(js, ""));
  return out;
}

async function replaceBlocks(text: string, re: RegExp, f: (body: string) => Promise<string>): Promise<string> {
  const parts: (string | Promise<string>)[] = [];
  let last = 0;
  for (const m of text.matchAll(re)) {
    parts.push(text.slice(last, m.index!), m[1], f(m[2]).catch(() => m[2]), m[3]);
    last = m.index! + m[0].length;
  }
  parts.push(text.slice(last));
  return (await Promise.all(parts)).join("");
}
