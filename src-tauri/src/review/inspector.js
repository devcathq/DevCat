(function (CONFIG) {
  "use strict";
  // Page-review inspector. One expression: Rust appends `(<config json>)` and injects it.
  // With CONFIG.test the pure `lib` is returned and nothing else happens.

  var MARK = "devcat-review-marked";

  function cssEscape(s) {
    if (typeof CSS !== "undefined" && CSS && typeof CSS.escape === "function") return CSS.escape(s);
    return String(s).replace(/[^\w-]|^-?\d/g, function (ch, i) {
      if (/\d/.test(ch)) return "\\3" + ch + " ";
      return /[\x00-\x7f]/.test(ch) ? "\\" + ch : ch;
    });
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
    if (e.id && !isJunkId(e.id)) return "#" + cssEscape(e.id);
    var cls = classesOf(e);
    var s = cls.length ? "." + cls.map(cssEscape).join(".") : e.tagName.toLowerCase();
    var sibs = e.parentElement ? Array.prototype.slice.call(e.parentElement.children) : [];
    var twins = sibs.filter(function (x) {
      if (x === e) return false;
      if (!cls.length) return x.tagName === e.tagName;
      for (var i = 0; i < cls.length; i++) if (!hasClass(x, cls[i])) return false;
      return true;
    });
    if (twins.length) {
      var same = sibs.filter(function (x) { return x.tagName === e.tagName; });
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
      var p = part(e);
      parts.unshift(p);
      if (p.charAt(0) === "#" && p.indexOf(" ") < 0 && e.id && !isJunkId(e.id)) break;
      if (root.querySelectorAll(parts.join(" ")).length <= 1) break;
    }
    var s = parts.join(" ");
    return s.length > 300 ? s.slice(0, 300) : s;
  }

  function visibleText(el) {
    var t = String(el.innerText || "").trim().split(/\r?\n/)[0].trim();
    if (!t) {
      var names = ["alt", "aria-label", "placeholder", "title"];
      for (var i = 0; i < names.length && !t; i++) t = String(el.getAttribute(names[i]) || "").trim();
    }
    return t.length > 40 ? t.slice(0, 39) + "…" : t;
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
    sourceOf: sourceOf,
  };
  if (CONFIG && CONFIG.test === true) return lib;

  // The overlay UI is added below.
  return undefined;
})
