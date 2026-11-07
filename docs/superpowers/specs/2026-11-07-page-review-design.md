# Page review: click an element, comment, hand the list to an agent

Status: design, approved by the user 2026-11-07.

## Why

Reviewing frontend work today means taking screenshots, collecting them somewhere temporary and pasting them
into an agent. That's slow, heavy on the agent's input (images are expensive and imprecise), and the switching
between browser, screenshot tool and agent makes reviewers skip things. We want review to feel like commenting
on a PDF: click the thing, type what's wrong, move on. At the end, the whole review goes to the agent as a small
block of text it can act on, with enough to find each element in the code.

## What the user sees

Prototype (throwaway, interactive): `/private/tmp/claude-502/-Users-rusdiy-Claude-DevCat/7faa4eed-a4bc-4bf6-a52e-940d0e82d356/scratchpad/review-mockup.html`. It
shows the intended interaction and the output format; it isn't code to keep.

1. **Start a review** of a URL (a local dev server such as `http://localhost:5173`, or a deployed site) or of a
   local `.html` file. Entry points: a "Review page…" item wherever DevCat offers new tabs/windows (asks for a URL),
   and a "Review" button in the file preview toolbar for `.html` files (`features/files/FilePreviewTab.tsx`).
2. **A Review tab opens** in the current DevCat window, next to the user's other tabs (as in the prototype). The
   tab's toolbar and notes sidebar are ordinary DevCat UI (React); the page itself shows in the area between
   them. Toolbar: URL, back / reload, the page area's size, an **Inspect** toggle and **Finish review**.
3. **Inspect works like Chrome's element picker**: hovering outlines the element and shows a label (tag and
   classes, size, font and color). `↑` selects the parent. `Esc` (or `I`) turns inspect off so the page can be
   used normally (log in, open a menu, navigate); `I` turns it back on.
4. **Commenting works like a PDF**: click an element, a small box opens beside it, type, `⌘/Ctrl+Enter` saves
   (`Esc` cancels). The element gets a numbered pin; the sidebar lists the notes. Hovering a note highlights its
   element, clicking scrolls to it, `×` removes it, and a note's text can be edited.
5. **The review survives the page reloading** (dev servers reload on every save) and navigating to other pages
   of the same site. When the page goes to another origin (an SSO login, a payment page), the inspector is
   inactive there: the injected script checks the origin first and does nothing, never receiving the review
   secret, and the toolbar says "Inspect is off on <host>". Coming back to the review's origin brings the notes
   back. *(Call made in design.)* Pins come back where their element can still be found; notes whose element is gone stay
   in the sidebar, marked as not found on this page.
6. **Finish review** shows the result text (with a token estimate and an **Include styles** checkbox) and saves
   it as one note in the project's Notes. The review stays open: the reviewer can keep adding notes and finish
   again, which updates the same note (the review state remembers the note id; if the user deleted that note
   meanwhile, a new one is made). Include styles is fixed into the text when saved; Notes stores only the text.
   Closing the tab asks whether to discard only when there are notes not yet saved.
7. In Notes, review notes (recognised by the title prefix `UI review ·`; no new `Note` field) get a visible
   **Copy for agent** button that copies the body as is, the same text as the existing "Copy text" menu item,
   which stays. It's a shortcut, not a different format. *(Builder's call, made in design.)*
   Agents with DevCat's MCP can also just read the note (`devcat_notes`, `src-tauri/src/agent_notes.rs`):
   "fix the pricing review note".

## Output format (the important part)

Instruction once, then one line per note. No HTML; styles only when the reviewer ticks **Include styles**.

```
UI review of http://localhost:5173/pricing (1280×800). Fix each item in this repo. Format: `selector` "visible text" (source) → comment. Grep the text/classes to find the source when none is given.

1. `h1` "Simple pricing for growing teams" (src/pages/Pricing.tsx:18) → too large on mobile, cap at 32px
2. `.cta` "Start free trial" → use our Button component, not a span
3. [/checkout] `.card:nth-of-type(2) .price` "$12" → add "/seat/mo"
   (style: 220×28, 20px/700, color rgb(29,35,48), bg transparent, pad 0px)
```

The note's first line is its title: `UI review · <host><path> · <date>`; the instruction line follows. Rules:

- **Selector**: the shortest one that is unique on the page. Per element: `#id`, else up to two meaningful
  classes, else the tag; `:nth-of-type(n)` only when siblings match; walk up to ancestors only while the
  selector matches more than one element. Skip generated class names (CSS-in-JS like `css-1x2abc`, `sc-…`,
  `svelte-…`, hash-looking names) since they can't be found in source. Ids that look generated are skipped too.
- **Visible text**: the element's first line of rendered text, trimmed, cut at 40 characters with `…`; omitted
  when empty (then add `alt`, `aria-label`, `placeholder` or `title` if present, in that order).
- **Source** (`file:line`, relative to the dev server's project root): only when the page exposes it in a
  development build. Best effort, in order: Svelte `__svelte_meta.loc`, React fiber debug info (`_debugSource`
  on React ≤18; React 19's owner stack where it carries a location), Vue `__vueParentComponent.type.__file`
  (file without line), and common `data-*` source attributes set by locator plugins. If nothing is found, leave
  it out. No requirement to change the reviewed project.
- **Page**: the header names the first page's URL. A note made on another path of the same origin gets a
  `[/path]` prefix. Query strings are dropped from the path unless the user's notes span pages that differ
  only by query (then keep them).
- **Window size** in the header is the size of the page area when the review finished; a note made at a different width gets
  `[390w]` (combined with the path prefix as `[/path 390w]`).
- **Styles** (only with the checkbox): one line, `size, font-size/weight, color, bg, padding`.
- Target size: about 25 tokens per note without styles. The prototype's token estimate (`chars / 4`) is fine.

## How it's built (decided)

- **No bundled browser, no Chromium of our own.** The page renders in the system webview Tauri already uses
  (WKWebView on macOS, WebKitGTK on Linux, WebView2 on Windows, which is Edge's engine but ships with the OS).
- **The page loads in its own webview, never in DevCat's app webview or an iframe**, so cross-origin pages work
  and the page has no reach into DevCat. Decided by the user: it's a **tab**, not a separate window. So the page
  webview is a child webview of the DevCat window (Tauri's multi-webview, `Window::add_child`, behind Tauri's
  `unstable` feature), placed exactly over the tab's page area and kept there as the layout changes (sidebar
  resize, window resize, zoom). It's hidden when its tab isn't the active one and destroyed when the tab closes.
- **DevCat overlays vs the native webview.** A native child webview draws above the app's HTML, so DevCat's own
  menus, dialogs, the command palette and toasts would be hidden behind it where they overlap the page area.
  The review tab must handle this: while any DevCat overlay is open in that window, the page webview is hidden
  (optionally replaced by a frozen snapshot if one is cheap to get; a plain placeholder is fine) and shown again
  when the overlay closes. The comment box and pins are drawn inside the page webview by the inspector, so they
  are not affected.
- **An inspector script is injected into every page load** (Tauri's initialization script for that webview). It
  draws the hover box, label, pins and comment box inside a closed shadow root on a top-layer host element, so
  page CSS can't restyle it and its CSS can't leak into the page. It must not change the page's layout.
- **The reviewed page is untrusted** (it may be any website). It must never get DevCat's IPC or any app command.
  The review webview gets no Tauri capability. The inspector reports to DevCat over one narrow channel that only
  carries review messages (add / edit / remove note, hover info is local), authenticated with a per-review
  secret that only the injected script holds (in a closure, set before page scripts run). The channel is the
  builder's call: candidates are a dedicated custom URI scheme handler, a loopback endpoint with token like
  `agent_bridge.rs`, or a single remote-scoped command. Whatever is picked must hold on all three platforms,
  including from `https://` pages (mixed content, Private Network Access in WebView2). The first plan step is a
  spike that proves the channel on macOS and Windows.
- **Everything that arrives from the inspector is treated as data**: lengths capped (comment ≤ 2,000 chars,
  selector ≤ 300, text ≤ 40), control characters stripped, backticks in values escaped so a page can't break
  out of the format, and nothing is saved until the user presses Finish and sees the text. The saved note goes
  through the normal notes path (`notes::save_checked`).
- **The review state lives in Rust**, per review tab, so it survives reloads and navigation. The injected
  script asks for the current notes on each load and re-places pins by selector.
- **Local `.html` files** are served to the review webview through a scoped protocol that can read only one
  folder: the code folder of the DevCat project that contains the file, or the file's own folder when it's in
  no project's folders (so `../assets/…` works inside a repo). Paths outside it, and symlinks leading out, are
  refused. In the output, the header and `[path]` prefixes show the file's path relative to that folder, never
  the protocol's internal URL. SSH-host files are out of scope (below). *(Call made in design.)*
- **Cookies and logins** persist between reviews in a review-only browser data store, separate from DevCat's
  own webview data (under `<data>/review-profile/`), so reviewers don't log in again every time. The review
  tab's toolbar menu has "Clear review browsing data". *(Call made in design.)*
- **Where the note goes**: the project of the window that started the review (General when none).
- **Desktop only.** Like the SSH workbench and rooms, review tabs and their commands stay out of the phone
  dispatcher (`src-tauri/src/remote/dispatch.rs`) and the browser version. Add a short section to `AGENTS.md`
  saying so and stating the "reviewed page is untrusted, no IPC" rule.
- Closing the tab ends the review, and so does closing its window (`windows::on_destroyed` must clean up the
  window's review webviews and state). If DevCat lets tabs move to another window, the review moves with its
  state and its webview is recreated in the new window.
- Long-running animations: none needed. If a "pin placed" or highlight effect is added, follow the AGENTS.md
  animation rules (opacity/transform only, not on svg).

## Goals

- Click-to-comment review of any page, with the result as compact text in Notes.
- Fewer tokens and less context switching than screenshots.
- Enough location info for an agent to find each element in the code without asking.

## Non-goals (v1)

- Drawing, arrows, region selection or screenshots of elements.
- Ticking notes off as fixed / re-review rounds.
- Viewport presets or device emulation (the reviewer resizes the window or the sidebar; the size is recorded).
- Sending straight into a DevCat Agent chat or terminal (Notes + copy, or the agent reads the note over MCP).
- Reviewing pages on SSH hosts or from the phone.
- Editing styles live like DevTools.

## Anti-goals

- Shipping or driving a Chromium/Chrome (no CDP, no headless browser).
- Any path by which the reviewed page can call DevCat commands or read DevCat data.
- An output format that grows per note with HTML or full computed styles by default.
- Changing how the reviewed page looks or lays out when inspect is off.

## Left to the builder

- The inspector channel mechanism (see above), after the spike.
- The child webview's label scheme: it must not match `main` / `win-*` in `capabilities/default.json`, so the
  page webview gets no capability. Check that a capability matched by *window* label doesn't extend to child
  webviews in that window (Tauri capabilities can match `webviews` separately); if it does, scope the default
  capability to the app webviews by label.
- How overlays are detected (a shared overlay counter in the UI kit's dialog/menu/popover components is the
  likely route).
- Exact heuristics for "generated" class names and ids, and the source-location lookups, as long as they meet
  the rules above. Unit-test the selector and formatting logic.
- Visual styling, following DevCat's existing UI components and theme.
