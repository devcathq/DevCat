(function (CONFIG) {
  "use strict";
  // Page-review inspector. One expression: Rust appends `(<config json>)` and injects it.
  // With CONFIG.test the pure `lib` is returned and nothing else happens.

  var MARK = "devcat-review-marked";

  function cssEscape(s) {
    if (typeof CSS !== "undefined" && CSS && typeof CSS.escape === "function") return CSS.escape(s);
    s = String(s);
    var out = "";
    for (var i = 0; i < s.length; i++) {
      var ch = s.charAt(i), c = s.charCodeAt(i);
      if (c === 0) out += "\uFFFD";
      else if (c < 0x20 || c === 0x7f) out += "\\" + c.toString(16) + " ";
      else if ((i === 0 && c >= 48 && c <= 57) || (i === 1 && c >= 48 && c <= 57 && s.charAt(0) === "-"))
        out += "\\3" + ch + " ";
      else if (i === 0 && ch === "-" && s.length === 1) out += "\\-";
      else if (c >= 0x80 || ch === "-" || ch === "_" || /[0-9A-Za-z]/.test(ch)) out += ch;
      else out += "\\" + ch;
    }
    return out;
  }

  function isJunkClass(name) {
    return (
      name === MARK ||
      /^(css|sc|jsx|svelte|emotion)-/.test(name) ||
      /^[a-z]{0,3}[A-Za-z0-9_-]*\d[A-Za-z0-9]{4,}$/.test(name)
    );
  }

  function isJunkId(id) {
    return (
      /^:.*:$/.test(id) || /^«.*»$/.test(id) ||
      /^(radix|headlessui)-/.test(id) ||
      /\d{5,}/.test(id) ||
      /(?=[a-f]*\d)[0-9a-f]{8,}/i.test(id)
    );
  }

  function classesOf(e) {
    var out = [];
    for (var i = 0; i < e.classList.length && out.length < 2; i++) {
      if (!isJunkClass(e.classList[i])) out.push(e.classList[i]);
    }
    return out;
  }

  // One step of the selector: #id, else up to two meaningful classes, else the tag; :nth-of-type
  // only when a sibling would match it too.
  function part(e) {
    var id = e.getAttribute("id"); // not e.id: <input name="id"> shadows it
    var cls = classesOf(e);
    var useId = id && !isJunkId(id);
    var s = useId ? "#" + cssEscape(id) : cls.length ? "." + cls.map(cssEscape).join(".") : e.localName;
    var sibs = e.parentElement ? Array.prototype.slice.call(e.parentElement.children) : [];
    var twins = sibs.filter(function (x) {
      if (x === e) return false;
      if (useId) return x.getAttribute("id") === id;
      if (!cls.length) return x.localName === e.localName;
      for (var i = 0; i < cls.length; i++) if (!hasClass(x, cls[i])) return false;
      return true;
    });
    if (twins.length) {
      var same = sibs.filter(function (x) { return x.localName === e.localName; });
      s += ":nth-of-type(" + (same.indexOf(e) + 1) + ")";
    }
    return s;
  }
  function hasClass(x, c) {
    for (var i = 0; i < x.classList.length; i++) if (x.classList[i] === c) return true;
    return false;
  }

  // Shortest selector unique within root: walk up only while it still matches more than one element.
  function selectorFor(el, root) {
    var parts = [];
    for (var e = el; e && e !== root; e = e.parentElement) {
      parts.unshift(part(e));
      if (root.querySelectorAll(parts.join(" ")).length <= 1) break;
    }
    // Cap at 300 by dropping leading parts, never by cutting inside one (an escape could break).
    while (parts.length > 1 && parts.join(" ").length > 300) parts.shift();
    var s = parts.join(" ");
    return s.length > 300 ? el.localName : s;
  }

  function visibleText(el) {
    var t = String(el.innerText || "").trim().split(/\r?\n/)[0].trim();
    if (!t) {
      var names = ["alt", "aria-label", "placeholder", "title"];
      for (var i = 0; i < names.length && !t; i++) t = String(el.getAttribute(names[i]) || "").trim();
    }
    var cps = Array.from(t); // by code point: never split a surrogate pair
    return cps.length > 40 ? cps.slice(0, 39).join("") + "…" : t;
  }

  // Absolute paths become relative to the project root, guessed as the parent of the last `/src/`.
  function rel(p) {
    p = String(p).replace(/\\/g, "/");
    var i = p.lastIndexOf("/src/");
    if (i >= 0) return p.slice(i + 1);
    return p;
  }
  function withLine(file, line) {
    return line ? rel(file) + ":" + line : rel(file);
  }

  function sourceOf(el) {
    try {
      var meta = el.__svelte_meta;
      if (meta && meta.loc && meta.loc.file) return withLine(meta.loc.file, meta.loc.line);

      for (var k in el) {
        if (k.indexOf("__reactFiber$") === 0) {
          var f = el[k];
          for (var n = 0; f && n < 5; n++, f = f._debugOwner) {
            var d = f._debugSource;
            if (d && d.fileName) return withLine(d.fileName, d.lineNumber);
          }
          break;
        }
      }

      var vue = el.__vueParentComponent;
      if (vue && vue.type && vue.type.__file) return rel(vue.type.__file);

      var a = el.getAttribute("data-source");
      if (a) return rel(a);
      var path = el.getAttribute("data-inspector-relative-path");
      if (path) return withLine(path, el.getAttribute("data-inspector-line"));
      a = el.getAttribute("data-locatorjs");
      if (a) return rel(a);
    } catch (_) {}
    return null;
  }

  var lib = {
    isJunkClass: isJunkClass,
    isJunkId: isJunkId,
    selectorFor: selectorFor,
    visibleText: visibleText,
    cssEscape: cssEscape,
    sourceOf: sourceOf,
  };
  if (CONFIG && CONFIG.test === true) return lib;

  // ---------------------------------------------------------------------------------------------
  // The overlay. The page is untrusted: everything here lives in one closed shadow root, nothing
  // is read from the page except the DOM it shows, and the secret never leaves this closure.
  var W = window, D = document;
  var loc = W.location;
  if (!loc || loc.origin !== CONFIG.origin) return undefined;
  var prefix = typeof CONFIG.pagePrefix === "string" ? CONFIG.pagePrefix : "";
  if (prefix && String(loc.pathname).indexOf(prefix) !== 0) return undefined;
  var internals = W.__TAURI_INTERNALS__;
  if (!internals || typeof internals.invoke !== "function") return undefined;
  var rawInvoke = internals.invoke.bind(internals); // before the page's scripts can swap it
  // Natives we rely on, taken now so the page can't swap them later.
  var createEl = D.createElement.bind(D);
  var queryOne = D.querySelector.bind(D);
  var getCS = W.getComputedStyle.bind(W);
  var raf = W.requestAnimationFrame.bind(W);
  var setT = W.setTimeout.bind(W), clearT = W.clearTimeout.bind(W);
  var MO = W.MutationObserver, SheetCtor = W.CSSStyleSheet;
  var attachFn = W.Element && W.Element.prototype && W.Element.prototype.attachShadow;
  var reappended = 0, sheetOk = false;
  var review = CONFIG.review, secret = CONFIG.secret;
  var notes = cleanNotes(CONFIG.notes);
  CONFIG = null;

  var inspect = true, hovered = null, host = null, shadow = null;
  var hoverBox, hoverLabel, pinLayer, hlBox, box, area, errLine, saveBtn;
  var boxTarget = null, entries = [], missingNow = [], shownPage = null;
  var queued = false, helloTimer = 0, domTimer = 0, highlighted = null;
  var reported = null; // the `missing` list DevCat last heard, as text

  function pageName() {
    var p = String(W.location.pathname);
    if (prefix && p.indexOf(prefix) === 0) p = p.slice(prefix.length);
    return p.slice(0, 500);
  }

  function cleanNotes(list) {
    var out = [];
    if (!Array.isArray(list)) return out;
    for (var i = 0; i < list.length && out.length < 5000; i++) {
      var x = list[i];
      if (x && typeof x.n === "number" && typeof x.page === "string" && typeof x.selector === "string") {
        out.push({ n: x.n, page: x.page, selector: x.selector, comment: String(x.comment == null ? "" : x.comment) });
      }
    }
    return out;
  }

  function send(msg) {
    try {
      return Promise.resolve(rawInvoke("plugin:devcat-review|msg", { review: review, secret: secret, msg: msg })).then(function (r) {
        if (r && Array.isArray(r.notes)) { notes = cleanNotes(r.notes); renderPins(); recheck(); }
        return r;
      });
    } catch (e) {
      return Promise.reject(e);
    }
  }

  function find(sel) {
    try { return queryOne(sel); } catch (_) { return null; }
  }

  var CSS =
    ":host{all:initial}" +
    "*{box-sizing:border-box;font:12px/1.4 system-ui,-apple-system,'Segoe UI',sans-serif}" +
    ".hv{position:fixed;left:0;top:0;pointer-events:none;border:2px solid #2563eb;background:rgba(37,99,235,.12);border-radius:2px;display:none}" +
    ".hl{position:fixed;left:0;top:0;pointer-events:none;border:2px solid #f59e0b;background:rgba(245,158,11,.14);border-radius:2px;display:none}" +
    ".lb{position:fixed;left:0;top:0;pointer-events:none;background:#111827;color:#fff;padding:2px 6px;border-radius:3px;white-space:nowrap;display:none;max-width:90vw;overflow:hidden;text-overflow:ellipsis}" +
    ".pin{position:fixed;left:0;top:0;width:20px;height:20px;border-radius:50%;background:#2563eb;color:#fff;font-weight:700;font-size:11px;line-height:20px;text-align:center;pointer-events:auto;box-shadow:0 1px 4px rgba(0,0,0,.4)}" +
    ".pin.on{background:#f59e0b}" +
    ".box{position:fixed;left:0;top:0;width:300px;background:#fff;color:#111827;border:1px solid #d1d5db;border-radius:8px;padding:8px;pointer-events:auto;box-shadow:0 6px 24px rgba(0,0,0,.3);display:none}" +
    ".box textarea{display:block;width:100%;height:72px;resize:none;border:1px solid #d1d5db;border-radius:4px;padding:4px 6px;color:#111827;background:#fff}" +
    ".row{display:flex;align-items:center;gap:6px;margin-top:6px}" +
    ".row span{flex:1;color:#6b7280;font-size:11px}" +
    ".row .err{color:#b91c1c}" +
    ".row button{border:1px solid #d1d5db;background:#f9fafb;color:#111827;border-radius:4px;padding:3px 10px;cursor:pointer}" +
    ".row button.go{background:#2563eb;border-color:#2563eb;color:#fff}";

  // Without constructable stylesheets (or when a strict CSP blocks <style>) the critical properties go
  // through CSSOM (el.style), which a CSP allows.
  var FONT = "font:12px/1.4 system-ui,-apple-system,sans-serif;box-sizing:border-box;";
  var INLINE = {
    hv: "position:fixed;left:0;top:0;pointer-events:none;border:2px solid #2563eb;background:rgba(37,99,235,.12);display:none;" + FONT,
    hl: "position:fixed;left:0;top:0;pointer-events:none;border:2px solid #f59e0b;background:rgba(245,158,11,.14);display:none;" + FONT,
    lb: "position:fixed;left:0;top:0;pointer-events:none;background:#111827;color:#fff;padding:2px 6px;white-space:nowrap;display:none;" + FONT,
    pin: "position:fixed;left:0;top:0;width:20px;height:20px;border-radius:50%;background:#2563eb;color:#fff;font-weight:700;font-size:11px;line-height:20px;text-align:center;pointer-events:auto;" + FONT,
    box: "position:fixed;left:0;top:0;width:300px;background:#fff;color:#111827;border:1px solid #d1d5db;padding:8px;pointer-events:auto;display:none;" + FONT,
    row: "display:flex;align-items:center;margin-top:6px;" + FONT,
  };
  function h(tag, cls, parent, text) {
    var n = createEl(tag);
    if (cls) n.className = cls;
    if (!sheetOk && cls && INLINE[cls]) n.style.cssText = INLINE[cls];
    if (text != null) n.textContent = text;
    if (parent) parent.appendChild(n);
    return n;
  }

  function rectOf(e) {
    try { return e.getBoundingClientRect(); } catch (_) { return { left: 0, top: 0, width: 0, height: 0 }; }
  }
  function frame(boxEl, r) {
    boxEl.style.transform = "translate(" + r.left + "px," + r.top + "px)";
    boxEl.style.width = Math.max(0, r.width) + "px";
    boxEl.style.height = Math.max(0, r.height) + "px";
    boxEl.style.display = "block";
  }

  function describe(e) {
    var r = rectOf(e), cs = getCS(e);
    var cls = classesOf(e);
    return e.localName + (cls.length ? "." + cls.join(".") : "") + " " + Math.round(r.width) + " × " + Math.round(r.height) +
      " · " + (cs.fontSize || "") + " " + (cs.color || "");
  }

  function drawHover() {
    if (!inspect || !hovered || !hovered.isConnected) { hoverBox.style.display = "none"; hoverLabel.style.display = "none"; return; }
    var r = rectOf(hovered);
    frame(hoverBox, r);
    hoverLabel.textContent = describe(hovered);
    hoverLabel.style.display = "block";
    var top = r.top >= 24 ? r.top - 22 : Math.min(r.top + r.height + 4, W.innerHeight - 24);
    hoverLabel.style.transform = "translate(" + Math.max(4, Math.min(r.left, W.innerWidth - 120)) + "px," + Math.max(2, top) + "px)";
  }

  function setHover(e) {
    if (e && (e === host || e.nodeType !== 1)) return;
    hovered = e;
    drawHover();
  }

  function styleString(e) {
    var r = rectOf(e), cs = getCS(e);
    return Math.round(r.width) + "×" + Math.round(r.height) + ", " + cs.fontSize + "/" + cs.fontWeight + ", color " + cs.color +
      ", bg " + cs.backgroundColor + ", pad " + cs.padding;
  }

  // Comment box ---------------------------------------------------------------------------------
  function placeBox() {
    if (!boxTarget || box.style.display === "none") return;
    var r = rectOf(boxTarget), bw = 300, bh = box.offsetHeight || 150;
    var x = Math.max(8, Math.min(r.left, W.innerWidth - bw - 8));
    var y = r.top + r.height + 8;
    if (y + bh > W.innerHeight) y = Math.max(8, r.top - bh - 8);
    box.style.transform = "translate(" + x + "px," + y + "px)";
  }
  function openBox(e) {
    boxTarget = e;
    errLine.textContent = "⌘/Ctrl+Enter saves · Esc cancels";
    errLine.className = "";
    saveBtn.disabled = false;
    box.style.display = "block";
    placeBox();
    area.focus();
  }
  function closeBox() {
    boxTarget = null;
    area.value = "";
    box.style.display = "none";
  }
  function saveBox() {
    var text = String(area.value || "").trim();
    var e = boxTarget;
    if (!text || !e) return;
    saveBtn.disabled = true;
    var msg = {
      kind: "add",
      page: pageName(),
      width: Math.round(W.innerWidth),
      selector: e === D.documentElement ? "html" : selectorFor(e, D.documentElement),
      text: visibleText(e),
      source: sourceOf(e),
      style: styleString(e),
      comment: text,
    };
    send(msg).then(closeBox, function () {
      saveBtn.disabled = false;
      errLine.textContent = "Couldn't save. Try again.";
      errLine.className = "err";
    });
  }

  // Pins ----------------------------------------------------------------------------------------
  function renderPins() {
    if (!pinLayer) return;
    while (pinLayer.firstChild) pinLayer.removeChild(pinLayer.firstChild);
    entries = []; missingNow = []; 
    shownPage = pageName();
    for (var i = 0; i < notes.length; i++) {
      var note = notes[i];
      if (note.page !== shownPage) continue;
      var e = find(note.selector);
      if (!e) { missingNow.push(note.n); continue; }
      var pin = h("div", "pin", pinLayer, String(note.n));
      pin.setAttribute("title", note.comment.slice(0, 200));
      entries.push({ n: note.n, sel: note.selector, el: e, pin: pin });
    }
    place();
  }

  function entryFor(n) {
    for (var i = 0; i < entries.length; i++) if (entries[i].n === n) return entries[i];
    return null;
  }

  function place() {
    if (pageName() !== shownPage) { renderPins(); return; }
    for (var i = 0; i < entries.length; i++) {
      var en = entries[i];
      if (!en.el.isConnected) en.el = find(en.sel) || en.el;
      var r = rectOf(en.el);
      en.pin.style.display = en.el.isConnected && (r.width || r.height) ? "block" : "none";
      en.pin.style.transform = "translate(" + Math.max(2, r.left - 10) + "px," + Math.max(2, r.top - 10) + "px)";
      en.pin.className = en.n === highlighted ? "pin on" : "pin";
      if (!sheetOk) en.pin.style.background = en.n === highlighted ? "#f59e0b" : "#2563eb";
    }
    var he = highlighted == null ? null : entryFor(highlighted);
    if (he && he.el.isConnected) frame(hlBox, rectOf(he.el)); else hlBox.style.display = "none";
    drawHover();
    placeBox();
  }

  function schedule() {
    if (queued) return;
    queued = true;
    raf(function () { queued = false; place(); });
  }

  function hello() {
    renderPins();
    reported = missingNow.join(",");
    send({ kind: "hello", page: pageName(), width: Math.round(W.innerWidth), height: Math.round(W.innerHeight), missing: missingNow.slice() })
      .catch(function () {});
  }

  // The notes changed under the pins (a reply, or DevCat's list): tell DevCat when that changed
  // which ones are missing. After a reload the first hello only knew the notes the page opened with.
  function recheck() {
    if (reported !== null && missingNow.join(",") !== reported) hello();
  }

  // The page changed its DOM or its URL (an SPA rendering late, pushState): pins and the missing
  // list follow, at most once per 250 ms. A timer, not a loop: nothing runs while the page is still.
  function domChanged() {
    if (domTimer) return;
    domTimer = setT(function () { domTimer = 0; renderPins(); recheck(); }, 250);
  }
  // Mutation records that are only our host moving (or anything under it) aren't the page's.
  function pageMutated(recs) {
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (r.target === host) continue;
      var nodes = Array.prototype.slice.call(r.addedNodes || []).concat(Array.prototype.slice.call(r.removedNodes || []));
      if (!nodes.length) return true;
      for (var j = 0; j < nodes.length; j++) if (nodes[j] !== host) return true;
    }
    return false;
  }
  function wrapHistory() {
    var hist = W.history;
    if (!hist) return;
    ["pushState", "replaceState"].forEach(function (name) {
      var orig = hist[name];
      if (typeof orig !== "function") return;
      try {
        hist[name] = function () {
          var out = orig.apply(this, arguments);
          domChanged();
          return out;
        };
      } catch (_) {}
    });
  }

  // Inspect mode --------------------------------------------------------------------------------
  // Non-passive, so it only exists while inspecting: otherwise touch scrolling would wait on us.
  var touching = false;
  function setTouch(on) {
    if (on === touching || !W) return;
    touching = on;
    if (on) W.addEventListener("touchstart", swallow, { capture: true, passive: false });
    else W.removeEventListener("touchstart", swallow, { capture: true });
  }
  function setInspect(on) {
    inspect = !!on;
    setTouch(inspect);
    if (!inspect) { hovered = null; closeBox(); }
    drawHover();
  }

  function typing(t) {
    if (!t || t.nodeType !== 1) return false;
    var n = t.localName;
    return n === "input" || n === "textarea" || n === "select" || t.isContentEditable === true;
  }

  function onMove(e) {
    if (!inspect || e.target === host) return;
    setHover(e.target);
  }
  function onOut(e) {
    if (inspect && e.relatedTarget == null && e.target !== host) { hovered = null; drawHover(); }
  }
  function swallow(e) {
    if (!inspect || e.target === host) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.stopImmediatePropagation) e.stopImmediatePropagation();
  }
  function onClick(e) {
    if (!inspect || e.target === host) return;
    swallow(e);
    if (boxTarget && String(area.value || "").trim()) return; // keep the draft on its element
    var t = hovered && hovered.isConnected ? hovered : e.target;
    if (t && t.nodeType === 1) { setHover(t); openBox(t); }
  }
  function onBoxKey(e) {
    if (e.target !== host) return;
    if (e.stopImmediatePropagation) e.stopImmediatePropagation();
    if (e.type !== "keydown") return;
    if (e.key === "Escape") { e.preventDefault(); closeBox(); }
    else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); saveBox(); }
  }
  function onKey(e) {
    if (e.target === host || e.metaKey || e.ctrlKey || e.altKey) return;
    if (typing(e.target)) return;
    if (e.key === "i" || e.key === "I") { e.preventDefault(); setInspect(!inspect); }
    else if (e.key === "Escape" && inspect) { e.preventDefault(); setInspect(false); }
    else if (e.key === "ArrowUp" && inspect && hovered) {
      var p = hovered.parentElement;
      if (p) { e.preventDefault(); setHover(p); }
    }
  }

  // Install -------------------------------------------------------------------------------------
  function install() {
    var root = D.documentElement;
    host = createEl("devcat-review-host");
    host.style.cssText = "all: initial; position: fixed; inset: 0; pointer-events: none; z-index: 2147483647";
    try {
      shadow = attachFn ? attachFn.call(host, { mode: "closed" }) : host.attachShadow({ mode: "closed" });
    } catch (_) { return; } // can't make a private root: stay inert
    if (!shadow) return;
    try {
      var sheet = new SheetCtor();
      sheet.replaceSync(CSS);
      shadow.adoptedStyleSheets = [sheet];
      sheetOk = true;
    } catch (_) { sheetOk = false; }
    hoverBox = h("div", "hv", shadow);
    hlBox = h("div", "hl", shadow);
    hoverLabel = h("div", "lb", shadow);
    pinLayer = h("div", null, shadow);
    box = h("div", "box", shadow);
    area = h("textarea", null, box);
    area.setAttribute("maxlength", "2000");
    area.setAttribute("placeholder", "Comment");
    var row = h("div", "row", box);
    errLine = h("span", null, row, "");
    var cancel = h("button", null, row, "Cancel");
    saveBtn = h("button", "go", row, "Save");
    if (!sheetOk) {
      area.style.cssText = "display:block;width:100%;height:72px;resize:none;border:1px solid #d1d5db;padding:4px 6px;color:#111827;background:#fff;" + FONT;
      errLine.style.cssText = "flex:1;color:#6b7280;font-size:11px;";
      cancel.style.cssText = saveBtn.style.cssText = "border:1px solid #d1d5db;background:#f9fafb;color:#111827;padding:3px 10px;margin-left:6px;" + FONT;
      saveBtn.style.background = "#2563eb"; saveBtn.style.color = "#fff";
    }
    cancel.addEventListener("click", closeBox);
    saveBtn.addEventListener("click", saveBox);
    // Keys typed in our box are handled here, in a window capture listener registered at init so it runs
    // before page listeners added later; stopImmediatePropagation keeps those from seeing them. (Typing
    // still works: the default action is untouched.) The page can still observe its own events in
    // general, and read its own DOM: accepted.
    W.addEventListener("keydown", onBoxKey, true);
    W.addEventListener("keyup", onBoxKey, true);
    W.addEventListener("keypress", onBoxKey, true);
    root.appendChild(host);

    new MO(function () {
      if (host.parentNode !== root && reappended < 20) { reappended++; root.appendChild(host); }
    }).observe(root, { childList: true });
    new MO(function (recs) { if (!domTimer && pageMutated(recs || [])) domChanged(); }).observe(root, { childList: true, subtree: true });
    wrapHistory();

    var cap = { capture: true };
    W.addEventListener("mousemove", onMove, { capture: true, passive: true });
    W.addEventListener("mouseout", onOut, { capture: true, passive: true });
    ["click", "mousedown", "mouseup", "pointerdown", "pointerup", "auxclick", "contextmenu", "dblclick"].forEach(function (t) {
      W.addEventListener(t, t === "click" ? onClick : swallow, cap);
    });
    if (inspect) setTouch(true);
    W.addEventListener("keydown", onKey, cap);
    W.addEventListener("scroll", schedule, { capture: true, passive: true });
    W.addEventListener("resize", function () {
      schedule();
      if (helloTimer) clearT(helloTimer);
      helloTimer = setT(function () { helloTimer = 0; hello(); }, 500);
    }, { passive: true });
    W.addEventListener("popstate", schedule);
    W.addEventListener("hashchange", schedule);

    var api = Object.freeze({
      setInspect: function (on) { setInspect(on); },
      setNotes: function (list) { notes = cleanNotes(list); renderPins(); recheck(); },
      highlight: function (n) { highlighted = typeof n === "number" ? n : null; place(); },
      scrollTo: function (n) {
        var en = typeof n === "number" ? entryFor(n) : null;
        if (!en) return;
        try { en.el.scrollIntoView({ block: "center", inline: "nearest" }); } catch (_) {}
        highlighted = n;
        place();
      },
    });
    try {
      Object.defineProperty(W, "__devcatReview", { value: api, configurable: false, writable: false, enumerable: false });
    } catch (_) {} // the page got there first: run without the global

    if (D.readyState === "complete") hello();
    else W.addEventListener("load", hello);
  }

  if (D.documentElement) install();
  else D.addEventListener("DOMContentLoaded", install);
  return undefined;
})
