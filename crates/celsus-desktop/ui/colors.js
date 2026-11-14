/* The colours of the interface as the user sets them, over those of the theme. Loaded before the page is drawn,
   so that a page never shows in the theme's own colours first.

   Each theme keeps its own choices. A few colours are chosen; the others the interface needs are made from them
   the way the theme makes its own: lines a little towards the text, faint text between the text and the paper. */
"use strict";

const celsusColors = (() => {
  /** The colours that can be chosen, with the names the style sheet gives them. Mirrors style.css. */
  const DEFAULTS = {
    dark: {
      background: "#000000", panel: "#0a0a09", surface: "#161513", field: "#0f0e0d", line: "#201e1b",
      text: "#f4efe8", soft: "#a39d94", accent: "#ffa45c", good: "#5fd35f", bad: "#ff9d8a", warn: "#fab219", info: "#9cc3f2",
    },
    light: {
      background: "#f0e6d3", panel: "#f5ecdb", surface: "#e6dac4", field: "#f9f2e3", line: "#e2d6c0",
      text: "#3b332a", soft: "#726658", accent: "#b5622e", good: "#327232", bad: "#b0473c", warn: "#9d6d14", info: "#34639c",
    },
  };
  const KEYS = Object.keys(DEFAULTS.dark);

  /** Whole palettes to start from. */
  const PRESETS = {
    dark: {
      charcoal: {},
      night: { background: "#05070d", panel: "#0b1020", surface: "#141b2e", field: "#0e1425", line: "#1d2640", text: "#e8eefc", soft: "#93a0bd", accent: "#6aa8f0" },
      forest: { background: "#040805", panel: "#0a110c", surface: "#131d16", field: "#0e1611", line: "#1d2a21", text: "#e9f3ea", soft: "#94a898", accent: "#7fc98a" },
      plum: { background: "#08050a", panel: "#110b14", surface: "#1d1422", field: "#160f1a", line: "#2a1f30", text: "#f5ecf7", soft: "#a898ad", accent: "#f07fb0" },
    },
    light: {
      parchment: {},
      paper: { background: "#eceae6", panel: "#f7f6f3", surface: "#e3e0da", field: "#ffffff", line: "#dcd8d0", text: "#26231f", soft: "#6c665d", accent: "#b5622e" },
      mist: { background: "#e6ebf0", panel: "#f1f4f8", surface: "#dbe2ea", field: "#fafcfe", line: "#d2dae4", text: "#1f2833", soft: "#5d6b7c", accent: "#2f6fb5" },
      sage: { background: "#e5ebe2", panel: "#f0f4ee", surface: "#d9e2d4", field: "#f9fbf8", line: "#cfd9ca", text: "#24301f", soft: "#5f6e59", accent: "#3f7f4a" },
    },
  };

  const rgb = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  const hex = (c) => "#" + c.map((v) => Math.round(Math.max(0, Math.min(255, v))).toString(16).padStart(2, "0")).join("");
  const mix = (a, b, t) => hex(rgb(a).map((v, i) => v + (rgb(b)[i] - v) * t));
  /** How light a colour looks, from 0 to 1. */
  const lum = (h) => {
    const lin = (v) => { v /= 255; return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
    const [r, g, b] = rgb(h);
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  };
  const contrast = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
  const valid = (v) => typeof v === "string" && /^#[0-9a-f]{6}$/i.test(v);

  function load() {
    let saved = {};
    try { saved = JSON.parse(localStorage.getItem("celsus-colors")) || {}; } catch {}
    // An accent chosen before there was a page for all the colours.
    try {
      const old = localStorage.getItem("celsus-accent");
      if (valid(old)) for (const t of ["dark", "light"]) { saved[t] ||= {}; saved[t].accent ||= old; }
    } catch {}
    return saved;
  }

  function save(all) {
    try {
      localStorage.setItem("celsus-colors", JSON.stringify(all));
      localStorage.removeItem("celsus-accent");
    } catch {}
  }

  /** The colours the theme shows now: its own, with those the user chose over them. */
  function current(theme) {
    const chosen = load()[theme] || {};
    const out = { ...DEFAULTS[theme] };
    for (const k of KEYS) if (valid(chosen[k])) out[k] = chosen[k].toLowerCase();
    return out;
  }

  /** Whether the user changed any colour of the theme. */
  const changed = (theme) => KEYS.some((k) => valid((load()[theme] || {})[k]));

  /** Put the colours in place. Without any chosen, the style sheet's own stand. */
  function apply() {
    const root = document.documentElement;
    const theme = root.dataset.theme === "light" ? "light" : "dark";
    const names = ["--black", "--panel", "--field", "--raise", "--raise-2", "--line", "--line-2", "--line-3", "--ink", "--ink-3",
      "--soft", "--mute", "--on-fill", "--ember", "--ember-soft", "--bad", "--good-ink", "--warn", "--info-ink"];
    if (!changed(theme)) { for (const n of names) root.style.removeProperty(n); return; }
    const c = current(theme);
    // Colours that must read on the background are moved towards the text until they do.
    const readable = (color, need) => {
      let out = color;
      for (let i = 0; i < 20 && contrast(out, c.background) < need; i++) out = mix(out, c.text, 0.1);
      return out;
    };
    const accent = readable(c.accent, 3);
    const set = {
      "--black": c.background, "--panel": c.panel, "--field": c.field,
      "--raise": c.surface, "--raise-2": mix(c.surface, c.text, 0.05),
      "--line": c.line, "--line-2": mix(c.line, c.text, 0.12), "--line-3": mix(c.line, c.text, 0.32),
      "--ink": c.text, "--ink-3": mix(c.text, c.soft, 0.25),
      "--soft": readable(c.soft, 3.5), "--mute": mix(readable(c.soft, 3.5), c.background, 0.3),
      "--ember": accent, "--ember-soft": mix(accent, c.text, 0.4),
      "--on-fill": contrast(accent, "#0b0a09") >= contrast(accent, "#fbf8f3") ? "#0b0a09" : "#fbf8f3",
      "--bad": readable(c.bad, 3), "--good-ink": readable(c.good, 3), "--warn": readable(c.warn, 3), "--info-ink": readable(c.info, 3),
    };
    for (const [n, v] of Object.entries(set)) root.style.setProperty(n, v);
  }

  /** Choose one colour of a theme; `null` gives it back to the theme. */
  function choose(theme, key, value) {
    const all = load();
    all[theme] ||= {};
    if (value && valid(value) && value.toLowerCase() !== DEFAULTS[theme][key]) all[theme][key] = value.toLowerCase();
    else delete all[theme][key];
    save(all);
    apply();
  }

  /** Take a whole palette, or with `null` the theme's own colours again. */
  function usePalette(theme, palette) {
    const all = load();
    all[theme] = { ...(palette || {}) };
    save(all);
    apply();
  }

  return { DEFAULTS, KEYS, PRESETS, current, changed, apply, choose, usePalette };
})();

celsusColors.apply();
