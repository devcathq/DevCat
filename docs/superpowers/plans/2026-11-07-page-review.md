# Page Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Review tab that shows a web page or local `.html` file, lets the reviewer click elements and comment
on them like a PDF, and saves the review as one compact Markdown note.

**Architecture:** The page shows in a native child webview of the DevCat window, placed over the Review tab's
page area. An injected inspector script (closed shadow root) draws the hover box, pins and comment box, and talks
to Rust over one secret-checked channel. Rust keeps each review's notes (so they survive reloads), formats the
output, and saves it through the normal notes path. The page webview gets no Tauri capability.

**Tech Stack:** Tauri 2.12 (`unstable` feature for `Window::add_child`), Rust, React + Zustand + Tailwind, plain
ES2020 JavaScript for the injected inspector.

**Spec:** `docs/superpowers/specs/2026-11-07-page-review-design.md`

## Global Constraints

- No bundled or driven Chromium: no CDP, no headless browser. The page renders in the system webview.
- The reviewed page must never reach DevCat's IPC or any app command; the page webview gets no capability.
- Inspector messages need the per-review secret; the secret lives only in the injected script's closure.
- Intake caps: comment ≤ 2,000 chars, selector ≤ 300, text ≤ 40 (with `…`); control characters stripped;
  backticks in values escaped.
- Nothing is saved to Notes until the user presses Finish and has seen the text.
- Output: one instruction line, then `N. [prefix] \`selector\` "text" (source) → comment`, about 25 tokens per
  note; styles only with **Include styles**: `   (style: W×H, size/weight, color …, bg …, pad …)`.
- Title line: `UI review · <host><path> · <date>` (files: `UI review · <relative path> · <date>`).
- Labels never assume an agent: the button is **Copy review**.
- Desktop only: nothing is added to `src-tauri/src/remote/dispatch.rs`, `mobile/` or the browser build.
- Endless animations (if any) animate only opacity/transform and never sit on an `<svg>` (AGENTS.md).

## Review Focus

1. A dev server's full reload (Vite HMR fallback) mid-review: notes and pins must come back, not vanish.
   → Task 7 step "reload keeps notes".
2. A page that defines its own `window.__devcatReview`, overrides `fetch`, or has `* { all: unset }` CSS: the
   inspector must still work and look right. → Task 6 check test "hostile page globals".
3. A comment or page text containing backticks, newlines or `→`: the output line must stay one line and parseable.
   → Task 2 test `format_escapes_hostile_values`.
4. A DevCat dialog, menu or command palette opened while a Review tab is visible: it must not be hidden behind
   the page. → Task 8 step "overlay hides page".
5. The tab moved to another window, or the app restarted with the Review tab restored: no crash, the tab reopens
   its page. Unsaved notes don't travel (DevCat's existing "unsaved changes are not moved" warning covers it,
   because the tab is marked dirty). → Task 8 step "moved and restored tabs".

---

## File Structure

- `src-tauri/src/review/mod.rs`: `ReviewStore` (in-memory reviews by id), types, Tauri commands.
- `src-tauri/src/review/intake.rs`: validates and sanitizes inspector messages.
- `src-tauri/src/review/format.rs`: builds the note text (title, instruction, lines).
- `src-tauri/src/review/channel.rs`: the inspector → Rust transport and the scoped file server (Task 1 picks
  the transport).
- `src-tauri/src/review/webview.rs`: child webview create / place / show / navigate / eval / destroy.
- `src-tauri/src/review/inspector.js`: the injected script (plain JS, no imports, no build step).
- `src-tauri/src/review/inspector.check.mjs`: Node test for the inspector's pure functions.
- `src/features/review/ReviewTab.tsx`: toolbar, page area, notes sidebar, Finish dialog.
- `src/features/review/store.ts`: Zustand store mirroring Rust review state per tab; `openReview()`.
- `src/features/review/overlay.ts`: detects DevCat overlays that cover the page area.
- `src/features/review/overlay.check.ts`: Node test for the rectangle logic.
- Modified: `src/tabs/types.ts`, `src/tabs/registry.tsx`, `src/tabs/TabIcon.tsx`, `src/lib/api.ts`,
  `src/features/notes/NotesPanel.tsx`, `src/features/files/FilePreviewTab.tsx`, `src-tauri/src/lib.rs`,
  `src-tauri/src/windows.rs`, `src-tauri/Cargo.toml`, `package.json`, `AGENTS.md`, `CHANGELOG.md`.

---

### Task 1: Spike: child webview, no IPC, and the inspector channel

Throwaway code on a scratch branch (`spike/review-channel`); only the findings are kept.

**Files:**
- Modify (scratch only): `src-tauri/Cargo.toml` (tauri features add `"unstable"`), `src-tauri/src/lib.rs`
- Modify (kept): `docs/superpowers/specs/2026-11-07-page-review-design.md` (add a "Channel (decided)" section)

**Interfaces:**
- Produces: the chosen transport, written in the spec, and the exact endpoint shape Task 5 implements:
  `POST <endpoint>/m` with JSON body `{ "review": "<id>", "secret": "<hex>", "msg": {…} }` and
  `GET <endpoint>/f/<reviewId>/<path>` for local files.

- [ ] **Step 1:** Add a debug-only command `spike_review(url)` that calls `window.add_child` with
  `WebviewBuilder::new("review-spike", WebviewUrl::External(url)).initialization_script(JS)` at a fixed rect.
- [ ] **Step 2:** In `JS`, try `window.__TAURI_INTERNALS__.invoke("notes_list")` and report the result to the
  console. Load `http://localhost:5173` (any Vite app), `https://example.com`, and a page served by the candidate
  transport. Expected for all three: the invoke is rejected. If the custom-scheme page *is* allowed, that
  transport is out for file serving.
- [ ] **Step 3:** Try transport A, an asynchronous URI scheme `devcat-review` (`register_asynchronous_uri_scheme_protocol`;
  URL `devcat-review://localhost/…` on macOS/Linux, `http://devcat-review.localhost/…` on Windows), with
  `fetch(…, {method: "POST"})` from each of the three pages. Then transport B, a loopback HTTP server on
  `127.0.0.1:<random>` (answer CORS and `Access-Control-Allow-Private-Network: true`). Record which reach Rust
  from each page on macOS; ask the user to run the same spike build on Windows (they have to: no Windows here).
- [ ] **Step 4:** Check the review webview's cookies persist across app restarts with a data folder under
  `<data>/review-profile/` (`WebviewBuilder::data_directory` on Windows/Linux; on macOS `data_store_identifier`
  with a fixed 16-byte id). Record what works.
- [ ] **Step 5:** Pick A if it works from all three pages on both platforms, else B. Write the "Channel
  (decided)" section into the spec with the results table, delete the scratch branch, commit the spec.

```bash
git add docs/superpowers/specs/2026-11-07-page-review-design.md
git commit -m "Page review: record the inspector channel decision"
```

---

### Task 2: Output format (Rust)

**Files:**
- Create: `src-tauri/src/review/format.rs`, `src-tauri/src/review/mod.rs` (types only for now)
- Modify: `src-tauri/src/lib.rs` (`mod review;`)

**Interfaces:**
- Produces (in `review/mod.rs`):
  ```rust
  pub struct ReviewNote { pub n: u32, pub page: String /* path or file rel path */, pub width: u32,
      pub selector: String, pub text: String, pub source: Option<String>, pub style: Option<String>,
      pub comment: String }
  pub struct ReviewDoc { pub target: Target, pub first_page: String, pub width: u32, pub height: u32,
      pub notes: Vec<ReviewNote> }
  pub enum Target { Url { origin: String }, File { root: PathBuf, rel: String } }
  ```
- Produces (in `review/format.rs`): `pub fn render(doc: &ReviewDoc, styles: bool, date: &str) -> String`

- [ ] **Step 1: Write the failing tests** in `format.rs`:

```rust
#[test] fn format_matches_spec_example() {
    // doc: origin http://localhost:5173, first_page "/pricing", 1280×800, three notes as in the spec
    assert_eq!(render(&doc, false, "7 Nov 2026"), "UI review · localhost:5173/pricing · 7 Nov 2026\n\
UI review of http://localhost:5173/pricing (1280×800). Format: `selector` \"visible text\" (source) → comment. Search the code for the text or classes when no source is given.\n\n\
1. `h1` \"Simple pricing for growing teams\" (src/pages/Pricing.tsx:18) → too large on mobile, cap at 32px\n\
2. `.cta` \"Start free trial\" → use our Button component, not a span\n\
3. [/checkout] `.card:nth-of-type(2) .price` \"$12\" → add \"/seat/mo\"\n");
}
#[test] fn styles_line_only_when_asked() { /* styles=true adds "   (style: …)" after a note with style Some */ }
#[test] fn width_prefix_when_different() { /* note.width 390 vs doc 1280 → "[390w] "; with other page "[/checkout 390w] " */ }
#[test] fn file_target_uses_relative_path() { /* Target::File rel "site/index.html" → title "UI review · site/index.html · …", header "UI review of site/index.html (…)" */ }
#[test] fn format_escapes_hostile_values() {
    // comment "a\nb `x` → c": still one line, backticks escaped as \`, newlines become spaces
    assert_eq!(render(&doc, false, "d").lines().filter(|l| l.starts_with("1. ")).count(), 1);
    assert!(render(&doc, false, "d").contains("→ a b \\`x\\` → c"));
}
#[test] fn empty_text_is_omitted() { /* text "" → "1. `img.logo` → …" */ }
```

- [ ] **Step 2:** Run `cd src-tauri && cargo test review::format`. Expected: FAIL (not defined).
- [ ] **Step 3:** Implement `render`. Prefix rule: `[page width]` parts present only when `page != first_page`
  or `width != doc.width`, joined by a space. Escape in every value: `` ` `` → `` \` ``, `\r`/`\n` → space.
- [ ] **Step 4:** Run `cargo test review::format`. Expected: PASS.
- [ ] **Step 5:** Commit: `git commit -m "Page review: output format"`.

---

### Task 3: Inspector locator functions (JS, pure)

**Files:**
- Create: `src-tauri/src/review/inspector.js` (only the `lib` part now), `src-tauri/src/review/inspector.check.mjs`
- Modify: `package.json` (script `"check:review": "node src-tauri/src/review/inspector.check.mjs && node --experimental-strip-types src/features/review/overlay.check.ts"`; the overlay part is added in Task 8, until then only the first command)

**Interfaces:**
- `inspector.js` is one expression: `(function (CONFIG) { … })`. Rust appends `(<config json>)`. With
  `CONFIG.test === true` it returns `lib` and does nothing else.
- Produces `lib`: `isJunkClass(name: string): boolean`, `isJunkId(id: string): boolean`,
  `selectorFor(el: Element, root: Element): string`, `visibleText(el: Element): string` (≤ 40 chars + `…`, first
  line; falls back to `alt`, `aria-label`, `placeholder`, `title`), `sourceOf(el: Element): string | null`.

- [ ] **Step 1: Write the failing check** using a tiny fake DOM (objects with `id`, `tagName`, `classList`,
  `parentElement`, `children`, `getAttribute`, `innerText`, and a `root.querySelectorAll(sel)` that matches by
  the selector grammar `selectorFor` emits). Assertions:

```js
eq(isJunkClass("css-1x2abc"), true); eq(isJunkClass("sc-bdVaJa"), true); eq(isJunkClass("svelte-12ab3c"), true);
eq(isJunkClass("price"), false); eq(isJunkClass("card"), false); eq(isJunkClass("mt-4"), false);
eq(isJunkId(":r3:"), true); eq(isJunkId("radix-1a2b3c"), true); eq(isJunkId("checkout"), false);
eq(selectorFor(h1, page), "h1");                                  // unique tag
eq(selectorFor(cta, page), ".cta");                               // unique class
eq(selectorFor(secondPrice, page), ".card:nth-of-type(2) .price"); // walks up only while ambiguous
eq(selectorFor(byId, page), "#checkout");
eq(selectorFor(styled, page), "button");                          // css-1x2abc skipped
eq(visibleText(longP), "Shared workspaces and roles for up to 2…");
eq(visibleText(img), "Acme logo");                                // alt fallback
eq(sourceOf(svelteEl), "src/routes/+page.svelte:12");             // __svelte_meta.loc
eq(sourceOf(plainEl), null);
console.log("review inspector: ok");
```

- [ ] **Step 2:** Run `pnpm check:review`. Expected: FAIL.
- [ ] **Step 3:** Implement. Selector algorithm as in the prototype
  (`/private/tmp/claude-502/-Users-rusdiy-Claude-DevCat/7faa4eed-a4bc-4bf6-a52e-940d0e82d356/scratchpad/review-mockup.html`, `part` / `selector`):
  per element `#id` (if not junk) else up to two non-junk classes else tag, `:nth-of-type(n)` only when siblings
  match, walk up while `root.querySelectorAll(path).length > 1`, stop at an id. Truncate the result to 300 chars.
  `sourceOf` order: Svelte `__svelte_meta.loc` (`file:line`), React fiber (`__reactFiber$*` key → `_debugSource`
  `fileName:lineNumber`, else nothing for React 19), Vue `__vueParentComponent.type.__file`, attributes
  `data-source`, `data-inspector-relative-path` + `data-inspector-line`, `data-locatorjs`. Paths made relative
  by stripping everything up to and including `/src/`'s parent when absolute.
- [ ] **Step 4:** Run `pnpm check:review`. Expected: `review inspector: ok`.
- [ ] **Step 5:** Commit: `git commit -m "Page review: inspector locators"`.

---

### Task 4: Review store and intake (Rust)

**Files:**
- Modify: `src-tauri/src/review/mod.rs`
- Create: `src-tauri/src/review/intake.rs`

**Interfaces:**
- Consumes: `ReviewDoc`, `ReviewNote`, `Target` (Task 2).
- Produces:
  ```rust
  pub enum InspectorMsg { Hello { page: String, width: u32, height: u32, missing: Vec<u32> /* notes on this page whose element wasn't found */ },
      Add { page: String, width: u32, selector: String, text: String, source: Option<String>, style: Option<String>, comment: String },
      Edit { n: u32, comment: String }, Remove { n: u32 } }      // serde tag = "kind", camelCase
  pub fn parse(raw: &serde_json::Value) -> AppResult<InspectorMsg>   // intake.rs: caps + cleaning
  pub struct Review { pub id: String, pub secret: String, pub doc: ReviewDoc, pub note_id: Option<String>,
      pub project_id: Option<String>, pub window: String, pub missing: Vec<u32>, pub saved: bool /* no change since Finish */ }
  pub struct ReviewStore { /* parking_lot::Mutex<HashMap<String, Review>> */ }
  impl ReviewStore { pub fn start(&self, target: Target, first_page: String, project_id: Option<String>, window: String) -> Review;
      pub fn apply(&self, id: &str, secret: &str, msg: InspectorMsg) -> AppResult<ReviewDoc>;  // secret compared in constant time
      pub fn get(&self, id: &str) -> Option<Review>; pub fn edit/remove(&self, id, n, …) -> AppResult<ReviewDoc>;
      pub fn set_saved(&self, id: &str, note_id: String); pub fn close(&self, id: &str);
      pub fn close_window(&self, label: &str) -> Vec<String> /* closed ids */ }
  pub fn review_store() -> &'static ReviewStore   // a OnceLock global, like other per-process stores
  ```
- Note numbers: `n` is the 1-based position; removing a note renumbers the rest (the sidebar and pins follow).

- [ ] **Step 1: Write the failing tests:**
  `intake_caps_lengths` (2,500-char comment → 2,000; 50-char text → 40 + `…`; 400-char selector → 300),
  `intake_strips_control_chars` (`"a\u{0}b\u{7}"` → `"ab"`, `\n` kept in comments only),
  `intake_rejects_unknown_kind`, `apply_needs_secret` (wrong secret → `Err`, doc unchanged),
  `remove_renumbers`, `hello_updates_size_only_on_first_page_or_finish` (Hello sets `doc.width/height` to the
  latest size), `close_window_drops_its_reviews`.
- [ ] **Step 2:** Run `cargo test review::`. Expected: FAIL.
- [ ] **Step 3:** Implement. Secret: 32 random bytes hex (`rand` / `getrandom`, whichever the crate already
  uses). Review ids: `rv-<12 hex>`.
- [ ] **Step 4:** Run `cargo test review::`. Expected: PASS.
- [ ] **Step 5:** Commit: `git commit -m "Page review: review store and inspector intake"`.

---

### Task 5: Channel and scoped file server (Rust)

**Files:**
- Create: `src-tauri/src/review/channel.rs`
- Modify: `src-tauri/src/lib.rs` (register the transport chosen in Task 1)

**Interfaces:**
- Consumes: `review_store()`, `intake::parse` (Task 4); endpoint shape (Task 1).
- Produces: `pub fn endpoint() -> String` (base URL the inspector posts to), `pub fn file_url(review: &str, rel: &str) -> String`,
  `pub fn resolve_file(root: &Path, rel: &str) -> AppResult<PathBuf>`, and a Tauri event
  `review://changed` with payload `{ id, doc, missing, saved }` emitted after every applied message.

- [ ] **Step 1: Write the failing tests** for `resolve_file`: `../secret` → Err; `/etc/passwd` → Err; a
  symlink inside root pointing outside → Err; `assets/app.css` → Ok(root/assets/app.css); `""` → root/index.html
  is not assumed (Err); URL-encoded `%2e%2e/` → Err.
- [ ] **Step 2:** Run `cargo test review::channel`. Expected: FAIL.
- [ ] **Step 3:** Implement `resolve_file` (canonicalize both, `starts_with`), the `/m` handler (JSON ≤ 16 KiB,
  `store.apply`, emit `review://changed` to the review's window only, respond `204`, CORS for the review's origin
  only), and `/f/<id>/<path>` (content type by extension, `Cache-Control: no-store`).
- [ ] **Step 4:** Run `cargo test review::`. Expected: PASS.
- [ ] **Step 5:** Commit: `git commit -m "Page review: inspector channel and file server"`.

---

### Task 6: Inspector UI (injected script)

**Files:**
- Modify: `src-tauri/src/review/inspector.js`, `src-tauri/src/review/inspector.check.mjs`

**Interfaces:**
- Consumes: `lib` (Task 3); `CONFIG = { origin, endpoint, review, secret, notes: ReviewNote[] }` set by Rust.
- Produces, for Rust → inspector calls via `webview.eval`: a frozen global `window.__devcatReview` (defined with
  `Object.defineProperty`, `configurable: false, writable: false`) with methods `setInspect(on: bool)`,
  `setNotes(notes)`, `highlight(n | null)`, `scrollTo(n)`. Nothing on it exposes the secret.

- [ ] **Step 1: Write the failing check** "hostile page globals": evaluate the script with `CONFIG.test = false`
  in a fake `window` whose `location.origin` differs from `CONFIG.origin` → the script defines nothing and makes
  no request; with matching origin and a page that pre-defined `window.__devcatReview` and replaced `fetch` →
  ours still installs (ours is defined first in real life; here assert it uses the `fetch` captured at start).
- [ ] **Step 2:** Run `pnpm check:review`. Expected: FAIL.
- [ ] **Step 3:** Implement the UI, behaving like the prototype: host element appended to
  `document.documentElement` with a closed shadow root and `all: initial`; hover box + label (`tag.classes W × H ·
  font-size color`); `↑` parent; `Esc` / `I` toggles inspect; click (captured, `preventDefault` +
  `stopPropagation` only while inspecting) opens the comment box; `⌘/Ctrl+Enter` saves, `Esc` cancels; numbered
  pins positioned from `getBoundingClientRect` on scroll/resize (passive listeners, rAF-throttled, no endless
  loop); on load sends `Hello`, then places pins for `CONFIG.notes` whose `page` equals the current path, by
  `querySelector(selector)`, marking misses. The inspector never changes page layout (host is
  `position: fixed; inset: 0; pointer-events: none`, only its interactive children take events).
- [ ] **Step 4:** Run `pnpm check:review`. Expected: PASS.
- [ ] **Step 5:** Commit: `git commit -m "Page review: inspector overlay"`.

---

### Task 7: Child webview and review commands (Rust)

**Files:**
- Create: `src-tauri/src/review/webview.rs`
- Modify: `src-tauri/src/review/mod.rs` (commands), `src-tauri/src/lib.rs` (register commands),
  `src-tauri/src/windows.rs` (`on_destroyed` → `review::on_window_destroyed(label)`), `src-tauri/Cargo.toml`
  (`tauri` features `["macos-private-api", "unstable"]`), `src/lib/api.ts`

**Interfaces:**
- Consumes: Tasks 2, 4, 5, 6.
- Produces Tauri commands (camelCase args from TS):
  ```rust
  review_start(window, target: StartTarget /* {url} | {path} */, project_id: Option<String>) -> ReviewInfo { id, title, doc }
  review_place(window, id, x: f64, y: f64, w: f64, h: f64, visible: bool) -> ()   // logical px; creates the webview on first call
  review_nav(id, action: "back" | "reload" | { url: String }) -> ()
  review_inspect(id, on: bool) -> ()
  review_highlight(id, n: Option<u32>, scroll: bool) -> ()
  review_edit(id, n, comment) -> ReviewDoc; review_remove(id, n) -> ReviewDoc
  review_render(id, styles: bool) -> String
  review_finish(id, styles: bool) -> Note        // formats, saves via notes::save_checked (updates note_id when set and it still exists)
  review_close(id) -> ()
  review_clear_data() -> ()
  ```
  Event `review://page` `{ id, url, inspectable }` from the webview's `on_page_load` (finished): `inspectable`
  is false on other origins, so the tab can say "Inspect is off on <host>".
  TS (`src/lib/api.ts`): `reviewStart`, `reviewPlace`, `reviewNav`, `reviewInspect`, `reviewHighlight`,
  `reviewEdit`, `reviewRemove`, `reviewRender`, `reviewFinish`, `reviewClose`, `reviewClearData` with the same
  argument names.
- Child webview label: `review-<id>` (never `main` / `win-*`). File targets load `channel::file_url`.

- [ ] **Step 1: Write the failing test** `start_url_target_parses_origin` (`"localhost:5173/pricing?x=1"` →
  origin `http://localhost:5173`, first_page `/pricing`; `"https://a.b/"` → `/`; `"javascript:…"` and
  `"file:///…"` → Err) and `start_file_target_root` (file inside a project code folder → that folder as root,
  else the file's parent; rel path relative to root).
- [ ] **Step 2:** Run `cargo test review::`. Expected: FAIL.
- [ ] **Step 3:** Implement the commands and `webview.rs`. The inspector script is
  `include_str!("inspector.js")` + `(` + config JSON + `)`. Use the data folder decided in Task 1. After a
  `review://changed` the Rust side also calls `__devcatReview.setNotes` on the webview. `on_window_destroyed`
  closes the window's reviews and their webviews. Confirm `capabilities/default.json` does not grant anything
  to `review-*` (add `"webviews": ["main", "win-*"]` scoping if Task 1 showed child webviews inherit it).
- [ ] **Step 4:** Run `cargo test review::` and `cargo clippy --all-targets -- -D warnings`. Expected: PASS, no warnings.
- [ ] **Step 5 (manual, "reload keeps notes"):** `pnpm tauri dev`, start a review of a Vite app from the dev
  console (`await window.__TAURI_INTERNALS__.invoke("review_start", …)` + `review_place`), add two notes, save a
  file in the Vite app so it fully reloads. Expected: both pins return; `review_render` shows both lines.
- [ ] **Step 6:** Commit: `git commit -m "Page review: child webview and commands"`.

---

### Task 8: Review tab (React)

**Files:**
- Create: `src/features/review/ReviewTab.tsx`, `src/features/review/store.ts`, `src/features/review/overlay.ts`,
  `src/features/review/overlay.check.ts`
- Modify: `src/tabs/types.ts` (`| { type: "review"; url?: string; path?: string }`),
  `src/tabs/registry.tsx` (`review: lazy(() => import("../features/review/ReviewTab"))`),
  `src/tabs/TabIcon.tsx` (icon `MousePointerClick`, label "Review"), `package.json` (`check:review`, second part)

**Interfaces:**
- Consumes: the `api.review*` calls (Task 7), `review://changed` event.
- Produces: `openReview(target: { url: string } | { path: string }): void` in `store.ts` (opens the tab; title
  `Review · <host>` or `Review · <file name>`), `coveringRect(page: DOMRect, overlays: DOMRect[]): boolean` and
  `useOverlayCovers(ref: RefObject<HTMLElement>): boolean` in `overlay.ts`.

- [ ] **Step 1: Write the failing check** `overlay.check.ts`: `coveringRect` true when any overlay rect
  intersects the page rect, false when it only touches an edge or is fully outside, false for a 0-size rect.
- [ ] **Step 2:** Run `pnpm check:review`. Expected: FAIL.
- [ ] **Step 3:** Implement `overlay.ts`: a `MutationObserver` on `document.body` (childList, subtree) plus
  `resize`, collecting rects of elements that are `position: fixed` and not inside the review tab itself
  (dialogs, `ContextMenu`, `QuickOpen`, tour, popovers, sonner toasts). Debounce to one check per animation frame.
- [ ] **Step 4:** Run `pnpm check:review`. Expected: PASS.
- [ ] **Step 5:** Implement `ReviewTab.tsx` (layout as in the prototype): `Toolbar` with back, reload, URL
  input (Enter navigates), size label (`W × H` of the page area), Inspect toggle, overflow menu ("Clear review
  browsing data"), **Finish review**; the page area is an empty `div` measured with `ResizeObserver` that calls
  `api.reviewPlace(…, visible)` where `visible = tab is active && !useOverlayCovers(ref)`; while hidden it shows a
  placeholder ("Page hidden while a DevCat menu or dialog is open"). Sidebar (`w-80`): notes with number,
  selector, comment; hover → `reviewHighlight(n, false)`, click → `reviewHighlight(n, true)`, double-click edits
  inline, `×` removes; "not found on this page" marker when the inspector reported a miss. Finish dialog
  (`Dialog`): `<pre>` of `reviewRender`, **Include styles** checkbox, `≈N tokens` (`ceil(chars / 4)`), Save
  (→ `reviewFinish`, then reload notes via `useNotes.getState().load()`), and **Copy review**. No toasts from the
  tab's own flows (they'd cover the page): show "Saved to Notes" inline in the dialog.
  The tab is `dirty` (`useTabs.getState().update(tab.id, { dirty })`) while it has notes and `saved` is false,
  so DevCat's existing close / move confirmations ask before anything is lost. Unmounting the tab calls
  `reviewClose`. The toolbar shows the URL from `review://page`, and "Inspect is off on <host>" when not
  inspectable.
- [ ] **Step 6 (manual, "overlay hides page"):** with a Review tab open, open the command palette, a context
  menu in the sidebar and Settings. Expected: each is fully visible; the page returns when it closes.
- [ ] **Step 7 (manual, "moved and restored tabs"):** with an unsaved note, move the Review tab to a new
  window. Expected: the "unsaved changes" confirmation; after Move, the page loads in the new window with no
  notes and no errors. Restart the app with a Review tab restored. Expected: the page loads, no errors.
- [ ] **Step 8:** Run `pnpm build` (tsc + vite). Expected: no type errors.
- [ ] **Step 9:** Commit: `git commit -m "Page review: Review tab"`.

---

### Task 9: Entry points and Copy review

**Files:**
- Modify: `src/features/notes/NotesPanel.tsx`, `src/features/files/FilePreviewTab.tsx`

**Interfaces:**
- Consumes: `openReview` (Task 8).

- [ ] **Step 1:** Notes panel header: a "Review a page…" button (icon `MousePointerClick`) next to `+`, opening
  a small `Dialog` with a URL input (default `http://localhost:5173`, remembered per project in `localStorage`
  under `devcat.review.lastUrl.<projectId>`) → `openReview({ url })`.
- [ ] **Step 2:** Notes list: notes whose body starts with `UI review · ` show a visible **Copy review** button
  (icon `Copy`) calling `copyText(n.body, "review")`. "Copy text" in the menu stays.
- [ ] **Step 3:** File preview toolbar: for `.html` / `.htm` files on this computer (not `ssh://`), a **Review**
  button → `openReview({ path })`.
- [ ] **Step 4 (manual):** review `http://localhost:5173` end to end: three notes, Finish, the note appears in
  Notes with **Copy review**; paste result matches the spec's format. Then review a local `.html` in a repo that
  loads `../assets/x.css`: styles apply; the note header shows the repo-relative path.
- [ ] **Step 5:** Run `pnpm build`. Expected: no errors.
- [ ] **Step 6:** Commit: `git commit -m "Page review: entry points and Copy review"`.

---

### Task 10: Docs and guard rails

**Files:**
- Modify: `AGENTS.md`, `CHANGELOG.md`

- [ ] **Step 1:** `AGENTS.md`: a "Page review" section: the reviewed page is untrusted (no capability, no IPC,
  secret-checked channel, intake caps, nothing saved without Finish); the inspector stays inert on other origins;
  desktop only (keep `review_*` out of `remote/dispatch.rs`, `mobile/`, the browser build).
- [ ] **Step 2:** `CHANGELOG.md`: an "Unreleased" entry: "Review tab: click elements on a page to comment on
  them, and save the review as a short note to copy to a developer or an agent."
- [ ] **Step 3:** Run `grep -n "review_" src-tauri/src/remote/dispatch.rs`. Expected: no output.
- [ ] **Step 4:** Run `cd src-tauri && cargo test && cargo clippy --all-targets -- -D warnings`, `pnpm check:review`,
  `pnpm build`. Expected: all pass.
- [ ] **Step 5:** Commit: `git commit -m "Page review: docs"`.
