// Builds the docs: site/docs/pages/*.md → <out>/<slug>.html, in the order of nav.json.
// Each page starts with `# Title` and a one-line summary. Links to other pages use `name.md`;
// images use `../img/name.png` (site/build.sh copies the README screenshots there).
// Usage: node site/docs/build.mjs <out-dir>
import { Marked } from "marked";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const out = process.argv[2];
if (!out) {
  console.error("usage: node site/docs/build.mjs <out-dir>");
  process.exit(1);
}
const REPO = "https://github.com/devcathq/DevCat";
const nav = JSON.parse(fs.readFileSync(path.join(here, "nav.json"), "utf8"));
const order = nav.flatMap((g) => g.pages);

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const slugify = (s) => s.toLowerCase().replace(/<[^>]+>/g, "").replace(/&[a-z]+;/g, "").replace(/[^\w\s-]/g, "").trim().replace(/\s+/g, "-");
const htmlName = (slug) => (slug === order[0] ? "index.html" : `${slug}.html`);

function load(slug) {
  const src = fs.readFileSync(path.join(here, "pages", `${slug}.md`), "utf8").replace(/\r\n/g, "\n");
  const m = src.match(/^# (.+)\n+([^\n]+)\n/);
  if (!m) throw new Error(`${slug}.md must start with "# Title" and a one-line summary`);
  return { slug, title: m[1].trim(), summary: m[2].trim(), body: src.slice(m[0].length) };
}
const pages = Object.fromEntries(order.map((s) => [s, load(s)]));

function render(page) {
  const toc = [];
  const used = new Set();
  const marked = new Marked({
    gfm: true,
    renderer: {
      heading({ tokens, depth }) {
        const text = this.parser.parseInline(tokens);
        let id = slugify(text) || "section";
        while (used.has(id)) id += "-";
        used.add(id);
        if (depth === 2 || depth === 3) toc.push({ id, text: text.replace(/<[^>]+>/g, ""), depth });
        return `<h${depth} id="${id}"><a class="anchor" href="#${id}" aria-hidden="true">#</a>${text}</h${depth}>\n`;
      },
      link({ href, title, tokens }) {
        const text = this.parser.parseInline(tokens);
        const local = href.match(/^([\w-]+)\.md(#.*)?$/);
        if (local) {
          if (!pages[local[1]]) throw new Error(`${page.slug}.md links to missing page ${href}`);
          href = htmlName(local[1]) + (local[2] ?? "");
        }
        const ext = /^https?:/.test(href) ? ' rel="noopener"' : "";
        return `<a href="${esc(href)}"${title ? ` title="${esc(title)}"` : ""}${ext}>${text}</a>`;
      },
      image({ href, text }) {
        return `<img src="${esc(href)}" alt="${esc(text)}" loading="lazy">`;
      },
      blockquote({ tokens }) {
        // "> **Note:** …" / "> **Tip:** …" / "> **Warning:** …" become callouts.
        const inner = this.parser.parse(tokens);
        const kind = inner.match(/^<p><strong>(Note|Tip|Warning):<\/strong>/)?.[1]?.toLowerCase() ?? "note";
        return `<aside class="callout ${kind}">${inner}</aside>\n`;
      },
    },
  });
  return { html: marked.parse(page.body), toc };
}

function sidebar(current) {
  return nav.map((g) => `<p class="group">${esc(g.title)}</p><ul>${g.pages.map((s) =>
    `<li><a href="${htmlName(s)}"${s === current ? ' aria-current="page"' : ""}>${esc(pages[s].title)}</a></li>`).join("")}</ul>`).join("\n");
}

function layout(page, { html, toc }) {
  const i = order.indexOf(page.slug);
  const prev = pages[order[i - 1]], next = pages[order[i + 1]];
  const group = nav.find((g) => g.pages.includes(page.slug)).title;
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${esc(page.title)} · DevCat docs</title>
  <meta name="description" content="${esc(page.summary)}">
  <meta name="theme-color" content="#0f1114">
  <link rel="icon" href="../img/icon.svg" type="image/svg+xml">
  <link rel="preload" href="../fonts/jetbrains-mono.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="stylesheet" href="../style.css">
  <link rel="stylesheet" href="docs.css">
</head>
<body class="docs">
<header class="nav docs-nav">
  <a class="brand" href="../"><img src="../img/icon.svg" width="28" height="28" alt="">DevCat</a>
  <nav aria-label="Main">
    <a href="index.html" aria-current="true">Docs</a>
    <a href="../#install">Download</a>
    <a href="${REPO}">GitHub</a>
  </nav>
</header>
<div class="docs-grid">
  <details class="doc-side" open>
    <summary>Docs contents</summary>
    <div class="search" hidden><input type="search" id="q" placeholder="Search the docs" aria-label="Search the docs" autocomplete="off"><ol id="hits"></ol></div>
    <nav aria-label="Docs">${sidebar(page.slug)}</nav>
  </details>
  <main class="doc">
    <p class="crumb">${esc(group)}</p>
    <h1>${esc(page.title)}</h1>
    <p class="summary">${esc(page.summary)}</p>
${html}
    <nav class="pager" aria-label="Previous and next page">
      ${prev ? `<a class="prev" href="${htmlName(prev.slug)}"><small>Previous</small>${esc(prev.title)}</a>` : "<span></span>"}
      ${next ? `<a class="next" href="${htmlName(next.slug)}"><small>Next</small>${esc(next.title)}</a>` : "<span></span>"}
    </nav>
    <p class="edit"><a href="${REPO}/edit/master/site/docs/pages/${page.slug}.md">Edit this page on GitHub</a></p>
  </main>
  ${toc.length > 1 ? `<nav class="toc" aria-label="On this page"><p>On this page</p><ul>${toc.map((t) => `<li class="d${t.depth}"><a href="#${t.id}">${esc(t.text)}</a></li>`).join("")}</ul></nav>` : "<span></span>"}
</div>
<script src="docs.js" defer></script>
</body>
</html>
`;
}

fs.mkdirSync(out, { recursive: true });
const index = [];
for (const slug of order) {
  const page = pages[slug];
  const r = render(page);
  fs.writeFileSync(path.join(out, htmlName(slug)), layout(page, r));
  const text = r.html.replace(/<a class="anchor"[^>]*>#<\/a>/g, "").replace(/<[^>]+>/g, " ").replace(/&[a-z#0-9]+;/g, " ").replace(/\s+/g, " ").trim();
  index.push({ url: htmlName(slug), title: page.title, summary: page.summary, headings: r.toc.map((t) => [t.id, t.text]), text });
}
fs.copyFileSync(path.join(here, "docs.css"), path.join(out, "docs.css"));
fs.copyFileSync(path.join(here, "docs.js"), path.join(out, "docs.js"));
fs.writeFileSync(path.join(out, "search.json"), JSON.stringify(index));
console.log(`docs: ${order.length} pages → ${out}`);
