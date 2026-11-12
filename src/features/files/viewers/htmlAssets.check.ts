// `pnpm check:html-assets`: which references the HTML preview inlines, and how.
// Runs under plain Node (type stripping): no node:* imports, no @types/node.
import { inlineAssets, isRelative, joinRel, type Load } from "./htmlAssets.ts";

const assert = {
  equal(a: unknown, b: unknown, what = "") { if (a !== b) throw new Error(`${what}: expected ${JSON.stringify(b)}, got ${JSON.stringify(a)}`); },
  match(s: string, re: RegExp) { if (!re.test(s)) throw new Error(`expected ${re} in:\n${s}`); },
  ok(v: unknown, what: string) { if (!v) throw new Error(what); },
};
const enc = (mime: string, s: string) => `data:${mime};base64,${btoa(String.fromCharCode(...new TextEncoder().encode(s)))}`;
const dec = (u: string) => new TextDecoder().decode(Uint8Array.from(atob(u.slice(u.indexOf(",") + 1)), (c) => c.charCodeAt(0)));
const files: Record<string, string> = {
  "style.css": '@import "theme/base.css"; body{background:url("img/bg.png")}',
  "theme/base.css": "h1{background:url(../img/bg.png)}",
  "img/bg.png": "PNG",
  "app.js": 'import { data } from "./data.js"; import("./lib/search.js");',
  "data.js": "export const data = 1;",
  "lib/search.js": 'import { data } from "../data.js";',
  "classic.js": "var x = 1;",
  "logo.svg": "<svg/>",
};
const asked: string[] = [];
const load: Load = async (ref) => {
  asked.push(ref);
  const t = files[ref];
  if (t === undefined) return null;
  const mime = ref.endsWith(".css") ? "text/css" : ref.endsWith(".js") ? "text/javascript" : "image/png";
  return enc(mime, t);
};

assert.equal(isRelative("style.css"), true);
assert.equal(isRelative("./a/b.css"), true);
for (const r of ["https://x/y.css", "//cdn/x.js", "/abs.css", "#top", "data:image/png;base64,AA", ""]) assert.equal(isRelative(r), false, r);
assert.equal(joinRel("theme/base.css", "../img/bg.png"), "img/bg.png");
assert.equal(joinRel("app.js", "./lib/search.js"), "lib/search.js");
assert.equal(joinRel("app.js", "../up.js"), "../up.js");

const page = `<!doctype html><html><head>
<link rel="stylesheet" href="style.css?v=2">
<link rel="icon" href=logo.svg>
<link rel="stylesheet" href="https://cdn.example/x.css">
<link rel="stylesheet" href="missing.css">
<style>.a{background:url('img/bg.png')}</style>
</head><body><img src="./img/bg.png" alt="">
<script src="classic.js"></script>
<script type="module" src="app.js"></script>
<script type="module">import { data } from "./data.js";</script>
</body></html>`;

// scripts off: css, icons and images only; scripts stay as written
const off = await inlineAssets(page, false, load);
assert.match(off, /<link rel="stylesheet" href="data:text\/css;charset=utf-8;base64,/);
assert.match(off, /<link rel="icon" href="data:image\/png;base64,/);
assert.match(off, /href="https:\/\/cdn\.example\/x\.css"/);
assert.match(off, /href="missing\.css"/);
assert.match(off, /<img src="data:image\/png;base64,UE5H" alt="">/);
assert.match(off, /<style>\.a\{background:url\('data:image\/png;base64,UE5H'\)\}<\/style>/);
assert.match(off, /<script src="classic\.js">/);
assert.match(off, /<script type="module" src="app\.js">/);
assert.match(off, /from "\.\/data\.js"/);
assert.ok(!asked.some((r) => r.endsWith(".js")), "no scripts read while they're off");
const css = dec(off.match(/<link rel="stylesheet" href="([^"]+)"/)![1]);
assert.match(css, /^@import "data:text\/css;charset=utf-8;base64,/);
assert.match(css, /url\("data:image\/png;base64,UE5H"\)/);
assert.match(dec(css.match(/@import "([^"]+)"/)![1]), /url\(data:image\/png;base64,UE5H\)/);

// scripts on: classic and module scripts, with the modules' imports
const on = await inlineAssets(page, true, load);
assert.match(on, /<script src="data:text\/javascript;base64,/);
const app = dec(on.match(/<script type="module" src="([^"]+)"/)![1]);
assert.match(app, /from "data:text\/javascript;charset=utf-8;base64,/);
const search = dec(app.match(/import\("([^"]+)"\)/)![1]);
assert.equal(dec(search.match(/from "([^"]+)"/)![1]), "export const data = 1;");
assert.match(on, /<script type="module">import \{ data \} from "data:text\/javascript;charset=utf-8;base64,/);

// an import cycle ends instead of recursing forever
files["a.js"] = 'import "./b.js";';
files["b.js"] = 'import "./a.js";';
const cyc = await inlineAssets('<script type="module" src="a.js"></script>', true, load);
assert.match(dec(dec(cyc.match(/src="([^"]+)"/)![1]).match(/import "([^"]+)"/)![1]), /import "\.\/a\.js"/);

console.log("htmlAssets: ok");
