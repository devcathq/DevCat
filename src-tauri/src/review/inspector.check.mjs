// `pnpm check:review`: the page-review inspector's locator functions against a hand-made fake DOM.
// Runs under plain Node: reads inspector.js (one expression) and calls it with { test: true }.
import { readFileSync } from "node:fs";

function eq(actual, expected, what) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}\n  expected ${e}\n  actual   ${a}`);
}

const src = readFileSync(new URL("./inspector.js", import.meta.url), "utf8");
const lib = (0, eval)(src)({ test: true });

// Fake DOM -------------------------------------------------------------------------------------
function el(tag, props = {}, children = []) {
  const e = {
    tagName: tag.toUpperCase(),
    localName: tag,
    id: props.clobber ? {} : props.id || "",
    classList: props.cls ? props.cls.split(" ") : [],
    attrs: Object.assign({}, props.id ? { id: props.id } : {}, props.attrs || {}),
    innerText: props.text || "",
    parentElement: null,
    children,
    getAttribute(n) { return n in this.attrs ? this.attrs[n] : null; },
    ...(props.extra || {}),
  };
  for (const c of children) c.parentElement = e;
  e.querySelectorAll = (sel) => query(e, sel);
  return e;
}
const unesc = (s) => s.replace(/\\([0-9a-fA-F]{1,6} ?|.)/g, (_, c) =>
  /^[0-9a-fA-F]/.test(c) ? String.fromCodePoint(parseInt(c, 16)) : c);
// Compound: #id | tag | .a.b | :nth-of-type(n), any mix, in the grammar selectorFor emits.
function compound(c) {
  const out = { id: null, tag: null, cls: [], nth: null };
  const re = /#((?:\\.|[^.#:\s])+)|\.((?:\\.|[^.#:\s])+)|:nth-of-type\((\d+)\)|((?:\\.|[^.#:\s])+)/g;
  let m;
  while ((m = re.exec(c))) {
    if (m[1]) out.id = unesc(m[1]);
    else if (m[2]) out.cls.push(unesc(m[2]));
    else if (m[3]) out.nth = +m[3];
    else out.tag = unesc(m[4]);
  }
  return out;
}
function matches(e, c) {
  if (c.id && e.getAttribute("id") !== c.id) return false;
  if (c.tag && e.localName !== c.tag) return false;
  if (!c.cls.every((k) => e.classList.includes(k))) return false;
  if (c.nth) {
    const sibs = e.parentElement ? e.parentElement.children.filter((x) => x.localName === e.localName) : [e];
    if (sibs.indexOf(e) + 1 !== c.nth) return false;
  }
  return true;
}
function query(root, sel) {
  const parts = sel.split(" ").map(compound);
  const all = [];
  (function walk(n) { for (const k of n.children) { all.push(k); walk(k); } })(root);
  return all.filter((e) => {
    if (!matches(e, parts[parts.length - 1])) return false;
    let a = e.parentElement, i = parts.length - 2;
    while (i >= 0 && a && a !== root) { if (matches(a, parts[i])) i--; a = a.parentElement; }
    return i < 0;
  });
}

const h1 = el("h1", { text: "Simple pricing" });
const cta = el("a", { cls: "cta btn-primary-xl-extra" });
const price1 = el("span", { cls: "price", text: "$9" });
const price2 = el("span", { cls: "price", text: "$12" });
const card1 = el("div", { cls: "card" }, [price1]);
const card2 = el("div", { cls: "card" }, [price2]);
const byId = el("section", { id: "checkout" });
const styled = el("button", { cls: "css-1x2abc" });
const longP = el("p", { text: "Shared workspaces and roles for up to 25 people\nsecond line" });
const img = el("img", { attrs: { alt: "Acme logo" } });
const tw = el("div", { cls: "mt-4 md:flex" });
const svelteEl = el("div", { extra: { __svelte_meta: { loc: { file: "src/routes/+page.svelte", line: 12, column: 4 } } } });
const plainEl = el("div");
const dupA = el("div", { id: "dup" }, [el("b", { text: "A" })]);
const dupB = el("div", { id: "dup" }, [el("b", { text: "B" })]);
const clob = el("form", { id: "signup" }, [el("input", { clobber: true, attrs: { name: "id" } })]);
const grad = el("linearGradient", {});
const svg = el("svg", {}, [el("defs", {}, [grad])]);
const page = el("body", {}, [h1, cta, card1, card2, byId, styled, longP, img, tw, svelteEl, plainEl, dupA, dupB, clob, svg]);

// Tests ----------------------------------------------------------------------------------------
const { cssEscape, isJunkClass, isJunkId, selectorFor, visibleText, sourceOf } = lib;

eq(isJunkClass("css-1x2abc"), true, "css-in-js"); eq(isJunkClass("sc-bdVaJa"), true, "styled-components"); eq(isJunkClass("svelte-12ab3c"), true, "svelte scope");
eq(isJunkClass("devcat-review-marked"), true, "our own mark");
eq(isJunkClass("price"), false, "plain"); eq(isJunkClass("card"), false, "plain"); eq(isJunkClass("mt-4"), false, "tailwind"); eq(isJunkClass("md:flex"), false, "tailwind variant");
eq(isJunkId(":r3:"), true, "useId"); eq(isJunkId("«r3»"), true, "useId 19"); eq(isJunkId("radix-1a2b3c"), true, "radix");
eq(isJunkId("headlessui-menu-button-5"), true, "headlessui"); eq(isJunkId("item-84729104"), true, "long digits");
eq(isJunkId("checkout"), false, "plain id"); eq(isJunkId("step-2"), false, "short digit");

eq(selectorFor(h1, page), "h1", "unique tag");
eq(selectorFor(cta, page), ".cta.btn-primary-xl-extra", "unique classes (two max)");
eq(selectorFor(price2, page), ".card:nth-of-type(2) .price", "walks up only while ambiguous");
eq(selectorFor(byId, page), "#checkout", "id");
eq(selectorFor(styled, page), "button", "css-1x2abc skipped");
eq(selectorFor(tw, page), ".mt-4.md\\:flex", "escaped class");
eq(query(page, selectorFor(tw, page))[0] === tw, true, "escaped selector finds the element");
eq(selectorFor(el("div", { cls: "x".repeat(400) }, []), page).length <= 300, true, "capped at 300");
eq(selectorFor(dupA.children[0], page), "#dup:nth-of-type(6) b", "duplicate id is not unique: keep walking, nth-of-type");
eq(selectorFor(dupB, page), "#dup:nth-of-type(7)", "duplicate id gets nth-of-type");
eq(selectorFor(clob.children[0], page), "input", "id read by attribute, not clobbered property");
eq(selectorFor(clob, page), "#signup", "form with clobbering child");
eq(selectorFor(grad, page), "linearGradient", "svg localName keeps case");
// a long chain drops leading parts whole
let deep = el("em", { cls: "leaf" }); const chain = [deep];
for (let i = 0; i < 40; i++) { deep = el("div", { cls: "level-number-" + "a".repeat(10) + (i % 2 ? "" : "b") }, [deep, el("div", { cls: "level-number-" + "a".repeat(10) + (i % 2 ? "" : "b") })]); }
const deepRoot = el("body", {}, [deep]);
const longSel = selectorFor(chain[0], deepRoot);
eq(longSel.length <= 300 && longSel.startsWith(".") && !longSel.startsWith(". "), true, "long selector drops whole leading parts");
eq(cssEscape("-4a"), "-\\34 a", "fallback: -digit"); eq(cssEscape("4a"), "\\34 a", "fallback: leading digit");
eq(cssEscape("-"), "\\-", "fallback: lone dash"); eq(cssEscape("md:flex"), "md\\:flex", "fallback: colon");

eq(visibleText(longP), "Shared workspaces and roles for up to 2…", "first line cut at 40");
eq(visibleText(el("p", { text: "a".repeat(38) + "😀 tail" })), "a".repeat(38) + "😀…", "no split surrogate at the cut");
eq(JSON.stringify(visibleText(el("p", { text: "a".repeat(39) + "😀😀" }))).includes("\\ud83d\\u"), false, "no lone surrogate");
eq(visibleText(img), "Acme logo", "alt fallback");
eq(visibleText(el("input", { attrs: { placeholder: "Email" } })), "Email", "placeholder fallback");
eq(visibleText(el("div", { text: "  \n " })), "", "empty");

eq(sourceOf(svelteEl), "src/routes/+page.svelte:12", "svelte");
eq(sourceOf(plainEl), null, "nothing");
eq(sourceOf(el("div", { extra: { __svelte_meta: { loc: { file: "/Users/me/app/src/a.svelte", line: 3 } } } })), "src/a.svelte:3", "absolute made relative");
eq(sourceOf(el("div", { extra: { __vueParentComponent: { type: { __file: "/w/app/src/Foo.vue" } } } })), "src/Foo.vue", "vue, no line");
eq(sourceOf(el("div", { extra: { "__reactFiber$abc": { _debugSource: { fileName: "/w/src/App.tsx", lineNumber: 9 } } } })), "src/App.tsx:9", "react 18");
eq(sourceOf(el("div", { extra: { "__reactFiber$abc": {} } })), null, "react 19, nothing");
eq(sourceOf(el("div", { attrs: { "data-source": "src/X.tsx:5" } })), "src/X.tsx:5", "data-source");
eq(sourceOf(el("div", { attrs: { "data-inspector-relative-path": "src/Y.vue", "data-inspector-line": "7" } })), "src/Y.vue:7", "inspector attrs");
eq(sourceOf(el("div", { attrs: { "data-locatorjs": "src/Z.tsx:2" } })), "src/Z.tsx:2", "locatorjs");

// Hostile page globals ---------------------------------------------------------------------------
function node(tag) {
  const n = {
    tag, style: {}, children: [], listeners: {}, attrs: {}, classList: { add() {}, remove() {}, contains: () => false },
    appendChild(c) { this.children.push(c); c.parentNode = this; return c; },
    append(...cs) { for (const c of cs) this.appendChild(c); },
    removeChild(c) { this.children = this.children.filter((x) => x !== c); },
    contains(c) { return this.children.includes(c); },
    attachShadow() { return node("#shadow"); },
    addEventListener(t, f) { (this.listeners[t] ||= []).push(f); },
    removeEventListener() {}, setAttribute(k, v) { this.attrs[k] = v; }, remove() {},
    querySelector: () => null, querySelectorAll: () => [], getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }),
    focus() {}, set textContent(v) {}, set innerHTML(v) {},
  };
  return n;
}
function fakeWorld(origin, pathname, pre) {
  const sent = [];
  const root = node("html");
  const doc = Object.assign(node("#document"), { documentElement: root, readyState: "complete", activeElement: null,
    createElement: node, createTextNode: () => node("#text") });
  const win = Object.assign(node("window"), {
    location: { origin, pathname, hash: "" }, innerWidth: 800, innerHeight: 600, document: doc,
    getComputedStyle: () => ({}), requestAnimationFrame: (f) => 0, setTimeout: () => 0, clearTimeout() {},
    __TAURI_INTERNALS__: { invoke: (...a) => { sent.push(["captured", ...a]); return Promise.resolve({ notes: [] }); } },
    MutationObserver: class { observe() {} disconnect() {} },
  });
  win.window = win;
  if (pre) Object.defineProperty(win, "__devcatReview", { value: "page", configurable: false, writable: false });
  return { win, doc, root, sent };
}
function run(world, cfg) {
  const keep = { window: globalThis.window, document: globalThis.document, MutationObserver: globalThis.MutationObserver };
  globalThis.window = world.win; globalThis.document = world.doc; globalThis.MutationObserver = world.win.MutationObserver;
  try { (0, eval)(src)(cfg); } finally { Object.assign(globalThis, keep); }
}
const cfg = (o) => ({ origin: "http://localhost:9", review: "r1", secret: "s3cret", notes: [], pagePrefix: "", ...o });

{ // wrong origin: nothing at all
  const w = fakeWorld("https://evil.example", "/");
  run(w, cfg());
  eq([Object.getOwnPropertyNames(w.win).includes("__devcatReview"), w.sent.length, w.root.children.length], [false, 0, 0], "foreign origin: inert");
  const f = fakeWorld("http://localhost:9", "/other/x.html");
  run(f, cfg({ pagePrefix: "/f/r1/" }));
  eq([f.sent.length, f.root.children.length], [0, 0], "file review outside prefix: inert");
}
{ // no Tauri invoke: nothing
  const w = fakeWorld("http://localhost:9", "/");
  delete w.win.__TAURI_INTERNALS__;
  run(w, cfg());
  eq([w.win.__devcatReview, w.root.children.length], [undefined, 0], "no invoke: inert");
}
{ // matching origin: global installed, frozen, secret hidden; hello goes through the captured invoke
  const w = fakeWorld("http://localhost:9", "/pricing");
  const captured = w.win.__TAURI_INTERNALS__.invoke;
  run(w, cfg());
  w.win.__TAURI_INTERNALS__.invoke = () => { throw new Error("page replaced invoke"); };
  const g = w.win.__devcatReview;
  eq([typeof g, Object.isFrozen(g), Object.keys(w.win).includes("__devcatReview")], ["object", true, false], "global frozen, not enumerable");
  eq(Object.keys(g).sort(), ["highlight", "scrollTo", "setInspect", "setNotes"], "global methods");
  eq(JSON.stringify(g).includes("s3cret"), false, "no secret on the global");
  eq(Object.getOwnPropertyDescriptor(w.win, "__devcatReview").configurable, false, "not configurable");
  eq(w.root.children.length, 1, "host appended");
  const [who, cmd, args] = w.sent[0];
  eq([who, cmd, args.review, args.secret, args.msg], ["captured", "plugin:devcat-review|msg", "r1", "s3cret",
    { kind: "hello", page: "/pricing", width: 800, height: 600, missing: [] }], "hello through captured invoke");
  void captured;
}
{ // a page that pre-defined the global: inspector still runs
  const w = fakeWorld("http://localhost:9", "/f/r1/site/index.html", true);
  run(w, cfg({ pagePrefix: "/f/r1/" }));
  eq([w.win.__devcatReview, w.root.children.length, w.sent[0][2].msg.page], ["page", 1, "site/index.html"], "pre-defined global: still runs, page name without prefix");
}

console.log("review inspector: ok");
