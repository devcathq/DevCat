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

  // The overlay UI is added below.
  return undefined;
})
