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
    id: props.id || "",
    classList: props.cls ? props.cls.split(" ") : [],
    attrs: props.attrs || {},
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
    else out.tag = m[4].toUpperCase();
  }
  return out;
}
function matches(e, c) {
  if (c.id && e.id !== c.id) return false;
  if (c.tag && e.tagName !== c.tag) return false;
  if (!c.cls.every((k) => e.classList.includes(k))) return false;
  if (c.nth) {
    const sibs = e.parentElement ? e.parentElement.children.filter((x) => x.tagName === e.tagName) : [e];
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
const page = el("body", {}, [h1, cta, card1, card2, byId, styled, longP, img, tw, svelteEl, plainEl]);

// Tests ----------------------------------------------------------------------------------------
const { isJunkClass, isJunkId, selectorFor, visibleText, sourceOf } = lib;

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

eq(visibleText(longP), "Shared workspaces and roles for up to 2…", "first line cut at 40");
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

console.log("review inspector: ok");
