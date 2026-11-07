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
   **Copy review** button that copies the body as is, the same text as the existing "Copy text" menu item,
   which stays. It's a shortcut, not a different format. *(Builder's call, made in design.)* The label and the
   text never assume the reader is an agent: a product manager may copy a review to a developer just as well.
   Agents with DevCat's MCP can also just read the note (`devcat_notes`, `src-tauri/src/agent_notes.rs`):
   "fix the pricing review note".

## Output format (the important part)

Instruction once, then one line per note. No HTML; styles only when the reviewer ticks **Include styles**.

```
UI review of http://localhost:5173/pricing (1280×800). Format: `selector` "visible text" (source) → comment. Search the code for the text or classes when no source is given.

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
  secret the injected script holds in a closure, set before page scripts run. A page that targets DevCat can
  still read it (accepted risk, see "Channel (decided)"); the secret stops casual forgery, not a determined page,
  and the review's notes are all that page could reach. The channel is the
  builder's call: candidates are a dedicated custom URI scheme handler, a loopback endpoint with token like
  `agent_bridge.rs`, or a single remote-scoped command. Whatever is picked must hold on all three platforms,
  including from `https://` pages (mixed content, Private Network Access in WebView2). The first plan step is a
  spike that proves the channel on macOS and Windows. Decided after the spike: see "Channel (decided)" below.
- **Everything that arrives from the inspector is treated as data**: lengths capped (comment ≤ 2,000 chars,
  selector ≤ 300, text ≤ 40), control characters stripped, backticks in values escaped so a page can't break
  out of the format, and nothing is saved until the user presses Finish and sees the text. The saved note goes
  through the normal notes path (`notes::save_checked`).
- **The review state lives in Rust**, per review tab, so it survives reloads and navigation. The injected
  script asks for the current notes on each load and re-places pins by selector.
- **Local `.html` files** are served to the review webview by a loopback file server,
  `http://127.0.0.1:<port>/f/<reviewId>/<path>`, that can read only one folder: the code folder of the DevCat
  project that contains the file, or the file's own folder when it's in no project's folders (so `../assets/…`
  works inside a repo). Paths outside it, and symlinks leading out, are refused. Not a custom URI scheme: Tauri
  treats a page served by a scheme the app registered as a local origin, which gets every DevCat command (the
  spike showed `notes_list` succeeding from one), while a loopback page is a remote origin and gets none. In
  the output, the header and `[path]` prefixes show the file's path relative to that folder, never the
  server's internal URL. SSH-host files are out of scope (below). *(Call made in design.)*
- **Cookies and logins** persist between reviews in a review-only browser data store, separate from DevCat's
  own webview data (under `<data>/review-profile/`), so reviewers don't log in again every time. The review
  tab's toolbar menu has "Clear review browsing data". *(Call made in design.)*
- **Where the note goes**: the project of the window that started the review (General when none).
- **Desktop only.** Like the SSH workbench and rooms, review tabs and their commands stay out of the phone
  dispatcher (`src-tauri/src/remote/dispatch.rs`) and the browser version. Add a short section to `AGENTS.md`
  saying so and stating the "reviewed page is untrusted, no IPC" rule.
- Closing the tab ends the review, and so does closing its window (`windows::on_destroyed` must clean up the
  window's review webviews and state). Moving the tab to another window follows DevCat's rule for tabs with
  unsaved changes: the tab is marked dirty while it has unsaved notes, DevCat warns that they aren't moved, and
  the page opens fresh in the new window. *(Call made while planning.)*
- Long-running animations: none needed. If a "pin placed" or highlight effect is added, follow the AGENTS.md
  animation rules (opacity/transform only, not on svg).

## Channel (decided)

Spike on 2026-11-07 (macOS 26, WKWebView, Tauri 2.12.3 with `unstable`, debug build; the spike code was thrown
away). Four child webviews were added to the `main` window with `Window::add_child`, labels `review-spike-<n>`,
each with an initialization script that probed IPC, sent a message over every candidate transport and reported
back through a navigation Rust intercepted (`on_navigation`), so blocked transports were still seen. Pages:
`http://localhost:5173/` (a static page from `python3 -m http.server`, not a Vite app: the same origin rules,
but a dev server's HMR websocket and module loading were not exercised),
`https://example.com/`, the test page served by transport A (`devcat-review://localhost/f/…`) and the test page
served by transport B (`http://127.0.0.1:<port>/f/…`).

Transports tried:

- **A**: an asynchronous custom URI scheme `devcat-review` (`register_asynchronous_uri_scheme_protocol`),
  `fetch(…, {method: "POST"})` to `devcat-review://localhost/m`.
- **B**: a loopback HTTP server on `127.0.0.1:<random>` answering CORS and `Access-Control-Allow-Private-Network: true`,
  `POST http://127.0.0.1:<port>/m`. Also tried from each page: `http://localhost:<port>`, `mode: "no-cors"`,
  `navigator.sendBeacon` and a WebSocket.
- **C**: Tauri's own IPC with one command made reachable from remote pages: an inline plugin `devcat-review` with
  one command `msg` (`tauri_build::Attributes::plugin("devcat-review", InlinedPlugin::new().commands(&["msg"]))`
  generates `devcat-review:allow-msg`) and a capability added at runtime for the review webviews only:
  `CapabilityBuilder::new("review").webview("review-*").remote("http://*:*").remote("https://*:*").permission("devcat-review:allow-msg")`.
  (`"http://*"` without `:*` does not match a URL with a port: `http://localhost:5173` was rejected until the
  pattern had `:*`.)

### Results on macOS

| Page | `invoke("notes_list")` | `invoke("plugin:app\|version")` (capability `default`) | A (custom scheme) | B (loopback HTTP) | C (`plugin:devcat-review\|msg`) |
|---|---|---|---|---|---|
| `http://localhost:5173` | rejected ("notes_list not allowed. Plugin not found") | rejected ("allowed on: windows main, win-*, URL: local") | reaches Rust | reaches Rust (CORS preflight answered) | reaches Rust |
| `https://example.com` | rejected | rejected | **blocked** ("Load failed", nothing arrives) | **blocked** (also `localhost`, `no-cors`, `sendBeacon` false, WebSocket error) | reaches Rust |
| page served by A (`devcat-review://localhost`) | **ALLOWED** (returned `[]`) | **ALLOWED** (returned `"0.22.0"`; `plugin:window\|title` returned `"DevCat"`) | reaches Rust | reaches Rust | reaches Rust |
| page served by B (`http://127.0.0.1:<port>`) | rejected | rejected | reaches Rust | reaches Rust | reaches Rust |

Notes on the results:

- Every page gets `window.__TAURI_INTERNALS__` (Tauri injects its IPC script into child webviews too); what
  protects DevCat is the ACL check on the origin, not the absence of the API.
- **A page served by a custom scheme the app registered is a "local" origin** to Tauri (`Webview::is_local_url`:
  `uri_scheme_protocols.contains_key(scheme)`; on Windows `http(s)://<scheme>.localhost`). Local origins get
  every app command (DevCat has no app ACL manifest) and every capability matched by window label. So **A must
  never serve review content**, and since A is blocked from `https://` pages anyway, A is out entirely.
- From an `https://` page WKWebView sends nothing to `http://127.0.0.1`, `http://localhost`, or a custom scheme
  (mixed content), not even Tauri's own `ipc://localhost` (`fetch` failed). Tauri's invoke still works there
  because it falls back to `window.ipc.postMessage` (WKScriptMessageHandler), which no mixed-content or CSP rule
  touches. That's why, on macOS, C works from every page and B and A don't.
- Even with C's capability added, the reviewed pages still couldn't call `notes_list` or `plugin:app|version`:
  the review capability allows exactly one command.

### Decision

The brief's rule was "A if it works from all three pages on both platforms, else B". Neither does: A gives
pages it serves full IPC and is blocked from `https://`, B is blocked from `https://` on macOS. C is
**verified on macOS only; Windows is pending**, and the Windows run (below) is a gate before this branch is
merged: if C fails there, the channel decision is reopened. So:

- **Messages from the inspector go over C**, one remote-scoped plugin command:
  `invoke("plugin:devcat-review|msg", { review: "<id>", secret: "<hex>", msg: {…} })`, the same three fields the
  plan gave the HTTP body. The command also checks, in Rust, that the calling webview (`Webview` argument, its
  label and its native `url()`, not anything the page says) is that review's webview and on the review's origin
  before it looks at the secret. That check is designed, not verified: the spike only printed the label and URL
  the command received (they were right for all four pages), it didn't reject anything with them. Task 5 tests
  it. Replies (the current notes, on load) are the command's return value.
- **Local `.html` files are served by B**, a loopback HTTP server that only serves files:
  `GET http://127.0.0.1:<port>/f/<reviewId>/<path>`. That page is a remote origin to Tauri, so it gets no IPC
  (row 4). B has no `/m` endpoint and sends **no** CORS or Private-Network headers, so other pages can't read
  through it; `<reviewId>` must be random and unguessable (it is a read grant for a folder to any process or
  browser tab on this computer that knows it); check the `Host` header is `127.0.0.1:<port>` (DNS rebinding);
  the folder scoping and symlink rules of "Local `.html` files" above apply here. It is a loopback server, not a
  custom scheme, because of the row-3 result.
  The port is random per launch, which resets `localStorage` of file pages between launches (cookies are not
  port-scoped and survived). Acceptable for v1.
- **Task 5 implements**: the inline plugin and its command, the runtime capability for `review-*` webviews only,
  and the file server. No `POST /m` endpoint. The `InlinedPlugin` goes in `build.rs` inside the existing
  `CARGO_FEATURE_GUI` branch (`tauri_build::try_build(Attributes::new().plugin(…))` in place of
  `tauri_build::build()`), and must not break the CLI build: check `cargo build --no-default-features` as well
  as the app build.
- **Refuse local origins as review URLs**: a review URL that Tauri would treat as local gets full IPC. In a
  debug build that is DevCat's own `devUrl` (`http://localhost:1420`); in any build `tauri://`, `ipc://` and any
  registered custom scheme (and `http(s)://<scheme>.localhost` on Windows). Task 5/7 must refuse these when the
  review starts and on navigation (`on_navigation` returning false), so a page can't navigate its webview to one.

**The secret can leak to a page that targets DevCat: accepted.** Tauri's invoke reads globals at call time
(`fetch`, `Headers`), so a page that patches `window.fetch` before the inspector sends can read the secret in the
request. With it, that page could only add, edit or remove notes in its own review, which the user sees before
pressing Finish, and which are capped and escaped like every inspector message. It gets no other command and no
other review. We don't work around it by posting raw IPC messages through Tauri's internals (`window.ipc.postMessage`
with the invoke key): that couples DevCat to Tauri's private message format for a small gain.

### Review data store

- **macOS**: `WebviewBuilder::data_store_identifier([u8; 16])` (macOS 14+) with a fixed id (spike:
  `b"devcat-review-p1"` = UUID `64657663-6174-2d72-6576-6965772d7031`). Cookies set on `https://example.com`,
  `http://localhost:5173` and the B page were there after quitting and restarting the app, and so was
  `localStorage` on the first two (the B page's port changed between launches, so its `localStorage` origin did too).
  `data_directory` is ignored by WKWebView: the store lives in
  `~/Library/WebKit/<bundle id>/WebsiteDataStore/<uuid>/` (the unbundled debug binary used
  `~/Library/WebKit/devcat/…`), **not** under `<data>/review-profile/`; it's separate from the app webview's
  default store, which is what matters. Below macOS 14 wry silently falls back to the default store (shared with
  DevCat's own webview); DevCat's minimum is macOS 11, so Task 5 must use `incognito(true)` there (no
  persistence) rather than share the app's store. "Clear review browsing data": `Webview::clear_all_browsing_data`
  on a review webview, or `wry::WebView::remove_data_store(&id)` with no review open.
- **Windows / Linux**: `WebviewBuilder::data_directory(<data>/review-profile)`. Not tested here. On Windows this
  is a separate WebView2 user data folder, so a separate browser process; DevCat sets no additional browser
  args, so the environment options don't conflict.
- In the spike's debug build `<data>` was `~/Library/Application Support/dev.devcat.app.dev/`; the spike created
  an empty `review-profile/` there.

### Capabilities and child webviews

**A capability matched by window label applies to every webview in that window.** `RuntimeAuthority::resolve_access`
matches `cmd.webviews` against the webview label *or* `cmd.windows` against the window label, and the spike
confirmed it: the A-served (local) page in `review-spike-2`, a child of `main`, could call `plugin:app|version`
and `plugin:window|title` through capability `default`. Remote pages were rejected only because `default` is
local-only. **Task 7 must** change `capabilities/default.json` from `"windows": ["main", "win-*"]` to
`"webviews": ["main", "win-*"]` (an app window's webview has the window's label) and check the app still works in
the main window and in a `win-*` window; the review webview labels (`review-<id>`) must match neither.

### Windows: to verify (gate before merge)

Nothing here was run on Windows; what follows is what WebView2 is documented to do, not a result. Run the same
spike build on Windows before the branch merges (the spike code isn't kept; the setup above is enough to rebuild it:
four child webviews on those pages, probing `notes_list`, `plugin:app|version`, A, B and C, plus a restart for the
cookie check). Expected, from WebView2's documented behaviour: custom schemes appear as
`http://devcat-review.localhost` and Tauri treats them as local there too, so A is out the same way;
`http://127.0.0.1` is "potentially trustworthy" in Chromium so B isn't blocked as mixed content, but a fetch from
a public `https://` page to loopback hits Private Network Access / Local Network Access (a preflight, or a
permission prompt in newer Edge), which is why B isn't relied on for messages; C uses
`window.chrome.webview.postMessage`, which neither rule touches, and remote capabilities are a documented Tauri
feature on Windows. Navigation to the B file server is a top-level load DevCat starts, which those rules don't
block. Until it's run on Windows, these rows are **open**: C from `https://`, B as a top-level page, and cookie
persistence with `data_directory`.

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

- ~~The inspector channel mechanism~~: decided, see "Channel (decided)".
- The child webview's label scheme: it must not match `main` / `win-*` in `capabilities/default.json`, so the
  page webview gets no capability. Check that a capability matched by *window* label doesn't extend to child
  webviews in that window (Tauri capabilities can match `webviews` separately); if it does, scope the default
  capability to the app webviews by label.
- How overlays are detected (a shared overlay counter in the UI kit's dialog/menu/popover components is the
  likely route).
- Exact heuristics for "generated" class names and ids, and the source-location lookups, as long as they meet
  the rules above. Unit-test the selector and formatting logic.
- Visual styling, following DevCat's existing UI components and theme.
