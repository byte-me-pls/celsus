/* The reader: pages drawn by pdf.js into our own view. Pages are drawn only when they come near the screen and
   are let go again when far away, so a 900-page book scrolls as smoothly as a short one. */

const RS = uiStrings("reader");
// Strings newer than some of the translations: shown in English until the language has its own.
const RS_NEWER = {
  thisPageAgain: (p) => `This page again (${p})`,
  nextTenAgain: (a, b) => `10 pages from here again (${a}–${b})`,
  foundMany: (n) => `${n}+ found`,
  printedAs: (l) => `(${l})`,
  printedTitle: "The page number printed in the book",
  pageBoxTitle: "A page number, or a page number as printed in the book (such as xii)",
};
for (const [k, v] of Object.entries(RS_NEWER)) RS[k] ??= v;

/* ---------- the layers over a page ---------- */

/** Rules for what is laid over each drawn page: its text, invisible but there to be selected and copied (the
    text layer of pdf.js), and its links. They come with the reader, as a sheet of their own. */
const LAYER_CSS = `
.rd-page .textLayer { position: absolute; inset: 0; z-index: 0; overflow: clip; opacity: 1; line-height: 1; text-align: initial; -webkit-text-size-adjust: none; text-size-adjust: none; forced-color-adjust: none; transform-origin: 0 0; caret-color: CanvasText; }
.rd-page .textLayer :is(span, br) { color: transparent; position: absolute; white-space: pre; cursor: text; transform-origin: 0% 0%; }
.rd-page .textLayer > :not(.markedContent), .rd-page .textLayer .markedContent span:not(.markedContent) { z-index: 1; }
.rd-page .textLayer span.markedContent { top: 0; height: 0; }
.rd-page .textLayer span[role="img"] { -webkit-user-select: none; user-select: none; cursor: default; }
.rd-page .textLayer ::selection { background: rgba(0, 90, 255, .28); }
.rd-page .textLayer br::selection { background: transparent; }
.rd-page .textLayer .endOfContent { display: block; position: absolute; inset: 100% 0 0; z-index: 0; cursor: default; -webkit-user-select: none; user-select: none; }
.rd-page .textLayer.selecting .endOfContent { top: 0; }
.rd-page .textLayer[data-main-rotation="90"] { transform: rotate(90deg) translateY(-100%); }
.rd-page .textLayer[data-main-rotation="180"] { transform: rotate(180deg) translate(-100%, -100%); }
.rd-page .textLayer[data-main-rotation="270"] { transform: rotate(270deg) translateX(-100%); }
.rd-links { position: absolute; inset: 0; z-index: 1; pointer-events: none; }
.rd-link { position: absolute; pointer-events: auto; cursor: pointer; border-radius: 2px; }
.rd-link:hover { background: color-mix(in srgb, #2b6cff 14%, transparent); }
.rd-untranslated { z-index: 2; }
.rd-label { font-variant-numeric: tabular-nums; white-space: nowrap; }
`;
(function addLayerStyles() {
  // A constructed sheet is used where it can be, as a page that allows no inline styles still takes it.
  try {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(LAYER_CSS);
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  } catch {
    const style = document.createElement("style");
    style.textContent = LAYER_CSS;
    document.head.appendChild(style);
  }
})();

/* ---------- pdf.js ---------- */

let pdfjsLib = null;
async function pdfjs() {
  if (!pdfjsLib) {
    pdfjsLib = await import("./pdfjs/pdf.min.js");
    pdfjsLib.GlobalWorkerOptions.workerSrc = "pdfjs/pdf.worker.min.js";
  }
  return pdfjsLib;
}

async function parsePdf(bytes) {
  const lib = await pdfjs();
  return lib.getDocument({
    data: new Uint8Array(bytes),
    cMapUrl: "pdfjs/cmaps/",
    cMapPacked: true,
    standardFontDataUrl: "pdfjs/standard_fonts/",
    isEvalSupported: false,
  }).promise;
}

/* ---------- covers ---------- */

/** A book's cover: its first page as a small JPEG, drawn the way the reader draws it. */
async function drawCover(bytes, width = 480) {
  const doc = await parsePdf(bytes);
  try {
    const page = await doc.getPage(1);
    const size = page.getViewport({ scale: 1 });
    // A very tall page (a scroll, a receipt) still gives a picture of a sensible size.
    const vp = page.getViewport({ scale: Math.min(width / size.width, (width * 2) / size.height) });
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.floor(vp.width));
    canvas.height = Math.max(1, Math.floor(vp.height));
    await page.render({ canvasContext: canvas.getContext("2d", { alpha: false }), viewport: vp }).promise;
    const blob = await new Promise((done) => canvas.toBlob(done, "image/jpeg", 0.86));
    if (!blob) throw new Error("no picture");
    return blob;
  } finally {
    doc.destroy();
  }
}

/** Books whose first page cannot be drawn are remembered, so each is tried once and not at every visit. */
function coverFailures() {
  try { return JSON.parse(localStorage.getItem("celsus-cover-failed")) || []; } catch { return []; }
}
const coverAsked = new Set();
let coverLine = Promise.resolve();

/** Make a cover for a book that has none. Books wait in line, so one PDF is open at a time. `path` says where
    the book's file is and `save` keeps the picture; `key` names the book and its file. */
function makeCover(key, path, save) {
  if (coverAsked.has(key) || coverFailures().includes(key)) return;
  coverAsked.add(key);
  coverLine = coverLine.then(async () => {
    let bytes;
    try {
      bytes = await invoke("read_file", { path: await path() });
    } catch {
      coverAsked.delete(key); // the file could not be fetched just now: ask again later
      return;
    }
    let blob;
    try {
      blob = await drawCover(bytes);
    } catch {
      try { localStorage.setItem("celsus-cover-failed", JSON.stringify([...coverFailures(), key].slice(-500))); } catch {}
      return;
    }
    await save(blob).catch(() => {});
  });
}

/** Open documents by file path; a stamp forces a fresh load after the file was rewritten. */
const openDocs = new Map(); // path -> { stamp, doc }
let filesStamp = 0;
async function openPdf(path) {
  const cached = openDocs.get(path);
  if (cached && cached.stamp === filesStamp) return cached.doc;
  if (cached) cached.doc.then((d) => d.destroy()).catch(() => {});
  const doc = (async () => parsePdf(await invoke("read_file", { path })))();
  openDocs.set(path, { stamp: filesStamp, doc });
  doc.catch(() => openDocs.delete(path));
  return doc;
}

/** Called when translated files may have changed on disk. */
let reloadTimer;
function readerFilesChanged() {
  filesStamp++;
  if (state.route.view !== "reader" || !reader) return;
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(() => reader?.reloadTranslation(), 400);
}

/** Where the reader stood when it was built again for the same book (a language came or went), so that it
    opens again on the same spot of the same page: { id, place, at }. */
let keptPlace = null;

/* ---------- reading positions ---------- */

function readingPrefs(id) {
  try { return JSON.parse(localStorage.getItem(`celsus-read-${id}`)) || {}; } catch { return {}; }
}
function saveReadingPrefs(id, prefs) {
  try { localStorage.setItem(`celsus-read-${id}`, JSON.stringify({ ...readingPrefs(id), ...prefs })); } catch {}
}
function viewPrefs() {
  try { return JSON.parse(localStorage.getItem("celsus-reader")) || {}; } catch { return {}; }
}
function saveViewPrefs(p) {
  try { localStorage.setItem("celsus-reader", JSON.stringify({ ...viewPrefs(), ...p })); } catch {}
}

/** Where to open a book: the language last read, else its most translated language, else the original. */
function readingLangFor(book) {
  const last = readingPrefs(book.id).lang;
  if (last === "orig" || book.translated[last]) return last;
  const best = Object.entries(book.translated).sort((a, b) => b[1] - a[1])[0];
  return best ? best[0] : "orig";
}

function openReader(book, page) {
  go("reader", { id: book.id, lang: readingLangFor(book), page: page || readingPrefs(book.id).page || 1 });
}

/* ---------- the view ---------- */

const ICON = {
  back: '<svg viewBox="0 0 24 24"><path d="M15 18l-6-6 6-6"/></svg>',
  split: '<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M12 4v16"/></svg>',
  minus: '<svg viewBox="0 0 24 24"><path d="M5 12h14"/></svg>',
  plus: '<svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>',
  more: '<svg viewBox="0 0 24 24"><circle cx="5" cy="12" r="1.2"/><circle cx="12" cy="12" r="1.2"/><circle cx="19" cy="12" r="1.2"/></svg>',
  translate: '<svg viewBox="0 0 24 24"><path d="M4 5h9M8.5 3v2M6 5c.6 3.6 2.8 6.5 6 8"/><path d="M11 5c-.6 3.4-2.9 6.4-7 8.5"/><path d="M13 21l4-10 4 10M14.5 17.5h5"/></svg>',
  left: '<svg viewBox="0 0 24 24"><path d="M15 18l-6-6 6-6"/></svg>',
  right: '<svg viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg>',
  contents: '<svg viewBox="0 0 24 24"><path d="M9 6h11M9 12h11M9 18h11"/><circle cx="4.5" cy="6" r="1"/><circle cx="4.5" cy="12" r="1"/><circle cx="4.5" cy="18" r="1"/></svg>',
  find: '<svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/></svg>',
  close: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>',
  light: '<svg viewBox="0 0 24 24"><path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/></svg>',
};

/* ---------- the reading light ---------- */

/** How pages are lit: as printed, warmer for the evening, or light on dark. Warmth and brightness from 0 to 1. */
function readingLight() {
  const l = viewPrefs().light || {};
  const num = (v, d, lo) => (typeof v === "number" && v >= lo && v <= 1 ? v : d);
  return {
    mode: ["paper", "night", "dark"].includes(l.mode) ? l.mode : "paper",
    warmth: num(l.warmth, 0.35, 0),
    brightness: num(l.brightness, 1, 0.4),
  };
}

/** Lay the light over the pages. The colour is multiplied into each page, the way a warm lamp takes the blue out
    of white paper; dark pages are the page turned over, with its colours kept. */
function applyReadingLight(box, light) {
  const warm = light.mode === "paper" ? 0 : light.warmth;
  // Towards candlelight as the warmth goes up, dimmer as the brightness goes down.
  const tint = [255, 150, 60].map((c) => Math.round((255 + (c - 255) * warm) * light.brightness));
  box.classList.toggle("lit", warm > 0 || light.brightness < 1);
  box.classList.toggle("dark-pages", light.mode === "dark");
  box.style.setProperty("--rd-tint", `rgb(${tint.join(",")})`);
}

async function renderReader(r) {
  reader?.destroy();
  reader = null;
  const book = await call("book_get", { id: r.id });
  setCrumbs(T.navLibrary, book.title, T.crumbReader);
  const files = await call("book_files", { id: r.id, open: true });
  const langs = Object.keys(files.outputs);
  const lang = r.lang === "orig" || langs.includes(r.lang) ? r.lang : langs[0] || "orig";
  const prefs = viewPrefs();
  const view = {
    lang,
    mode: prefs.mode === "paged" ? "paged" : "scroll",
    split: lang !== "orig" && !!prefs.split,
    zoom: prefs.zoom || 1,
  };
  saveReadingPrefs(book.id, { lang });
  main.className = "reader-host";
  main.innerHTML = `
    <div class="rd">
      <header class="rd-bar">
        <button class="rd-icon" id="rd-back" title="${RS.back}">${ICON.back}</button>
        <button class="rd-icon" id="rd-contents" title="${RS.contents}">${ICON.contents}</button>
        <button class="rd-icon" id="rd-find" title="${RS.find}">${ICON.find}</button>
        <div class="rd-title"><b>${esc(book.title)}</b><small>${esc(book.author || "")}</small></div>
        <div class="segmented" id="rd-lang">
          <button data-lang="orig">${RS.original}</button>
          ${langs.map((l) => `<button data-lang="${esc(l)}">${esc(langName(l))}</button>`).join("")}
        </div>
        <button class="rd-icon" id="rd-split" title="${RS.split}" ${langs.length ? "" : "disabled"}>${ICON.split}</button>
        <span class="rd-sep"></span>
        <div class="segmented" id="rd-mode"><button data-mode="scroll">${RS.scroll}</button><button data-mode="paged">${RS.paged}</button></div>
        <div class="rd-zoom">
          <button class="rd-icon" id="rd-zoom-out" title="${RS.zoomOut}">${ICON.minus}</button>
          <button class="rd-zoom-val" id="rd-zoom-val" title="${RS.fit}"></button>
          <button class="rd-icon" id="rd-zoom-in" title="${RS.zoomIn}">${ICON.plus}</button>
        </div>
        <div class="rd-menu-wrap" id="rd-light-wrap">
          <button class="rd-icon" id="rd-light" title="${RS.light} (N)">${ICON.light}</button>
          <div class="rd-light" id="rd-light-menu" hidden>
            <h3>${RS.light}</h3>
            <div class="segmented" id="rd-light-mode"><button data-light="paper">${RS.lightPaper}</button><button data-light="night">${RS.lightNight}</button><button data-light="dark">${RS.lightDark}</button></div>
            <label class="rd-slider" id="rd-warmth-row"><span>${RS.warmth}</span><input type="range" id="rd-warmth" min="0" max="100" step="1"></label>
            <label class="rd-slider"><span>${RS.brightness}</span><input type="range" id="rd-brightness" min="40" max="100" step="1"></label>
            <p class="rd-light-note" id="rd-light-note"></p>
          </div>
        </div>
        <label class="rd-pageno"><input id="rd-page" type="number" min="1" max="${book.pages}" value="1"><span>${RS.pageOf(book.pages)}</span><small class="rd-label" id="rd-label" title="${esc(RS.printedTitle)}" hidden></small></label>
        <span class="spacer"></span>
        <span class="rd-live" id="rd-live" hidden></span>
        <button class="primary-button" id="rd-translate">${ICON.translate}${RS.translate}</button>
        <div class="rd-menu-wrap">
          <button class="rd-icon" id="rd-more" title="${RS.more}">${ICON.more}</button>
          <div class="rd-menu" id="rd-menu" hidden>
            <button data-act="details">${RS.details}</button>
            ${lang !== "orig" ? `<button data-act="save">${RS.savePdf}</button>` : ""}
          </div>
        </div>
      </header>
      <div class="rd-main">
        <aside class="rd-side" id="rd-side" hidden>
          <div class="rd-side-head">
            <div class="segmented" id="rd-side-tabs"><button data-side="contents">${RS.contents}</button><button data-side="find">${RS.find}</button></div>
            <button class="rd-icon" id="rd-side-close" title="${RS.close}">${ICON.close}</button>
          </div>
          <div class="rd-side-body" id="rd-side-body"></div>
        </aside>
        <div class="rd-scroll" id="rd-scroll" tabindex="0"><div class="rd-pages" id="rd-pages"></div></div>
        <button class="rd-flip prev" id="rd-prev" title="${RS.prev}" hidden>${ICON.left}</button>
        <button class="rd-flip next" id="rd-next" title="${RS.next}" hidden>${ICON.right}</button>
      </div>
      <div class="rd-pop" id="rd-pop" hidden></div>
    </div>`;
  reader = new Reader(book, files, view);
  await reader.start(Math.max(1, Math.min(book.pages, r.page || 1)));
}

/** A letter as a search compares it: small, without its accents, and an I the same with or without its dot, so
    that what is looked for is found whatever the case and the language of the book. */
function fold(c) {
  return c.toLowerCase().normalize("NFD").replace(/\p{M}/gu, "").replace(/ı/g, "i");
}

class Reader {
  constructor(book, files, view) {
    this.book = book;
    this.files = files;
    this.view = view;
    this.scroller = $("#rd-scroll");
    this.pagesBox = $("#rd-pages");
    this.rows = []; // one per page: { el, slots: [{ el, canvas, task, drawn }] }
    this.sizes = new Map(); // page -> [w, h] in PDF points
    this.defaultSize = [612, 792];
    this.page = 1;
    this.translated = new Set();
    this.drawnRows = new Set();
    this.labels = null; // the book's own page numbers (i, ii, 1, 2…), when it has some that differ
    this.moves = 0; // counts each move of the view, so a search can tell whether the reader moved since
    // Keeping up with a job that writes the translated file every few seconds: pages whose translation
    // changed since the file was last read, pages a job reported done since the last write, and when the
    // file was last read.
    this.stale = new Set();
    this.finished = new Set();
    this.lastDone = new Map(); // job id -> pages done at its last report
    this.lastRead = Date.now();
    this.alive = true;
    this.onKey = (e) => this.key(e);
    this.onResize = () => this.layout(true);
    // A selection being drawn with the mouse ends wherever the button is let go.
    this.onUp = () => { for (const el of $$(".textLayer.selecting", this.pagesBox)) el.classList.remove("selecting"); };
    document.addEventListener("keydown", this.onKey);
    document.addEventListener("pointerup", this.onUp);
    window.addEventListener("resize", this.onResize);
  }

  destroy() {
    this.alive = false;
    this.findRun = null;
    this.observer?.disconnect();
    clearTimeout(this.writtenTimer);
    clearTimeout(this.lateTimer);
    clearTimeout(this.soonTimer);
    document.removeEventListener("keydown", this.onKey);
    document.removeEventListener("pointerup", this.onUp);
    window.removeEventListener("resize", this.onResize);
    for (const row of this.rows) for (const s of row.slots) s.task?.cancel();
  }

  /** The documents shown side by side, left to right. */
  sources() {
    const v = this.view;
    const trans = v.lang !== "orig" ? this.files.outputs[v.lang] : null;
    if (!trans) return [this.files.source];
    return v.split ? [this.files.source, trans] : [trans];
  }

  async start(page) {
    this.bindBar();
    applyReadingLight($(".rd"), readingLight());
    this.pagesBox.innerHTML = `<p class="rd-msg">${RS.loading}</p>`;
    try {
      this.docs = await Promise.all(this.sources().map(openPdf));
    } catch (e) {
      this.pagesBox.innerHTML = `<p class="rd-msg">${RS.failed}: ${esc(e?.message || e)}</p>`;
      return;
    }
    if (!this.alive) return;
    const first = await this.docs[0].getPage(1);
    const vp = first.getViewport({ scale: 1 });
    this.defaultSize = [vp.width, vp.height];
    this.sizes.set(1, this.defaultSize);
    // The sizes of all pages come from the last visit; else those near the page opened are read now, so that
    // the first view is right, and the others while the book is read.
    const known = this.cachedSizes();
    if (!known) await this.measure(page - 3, page + 3);
    await Promise.all([this.loadTranslated(), this.loadLabels()]);
    if (!this.alive) return;
    this.build();
    const kept = keptPlace?.id === this.book.id && Date.now() - keptPlace.at < 15000 ? keptPlace.place : null;
    keptPlace = null;
    if (kept) this.moveTo(kept.page, false, kept.frac);
    else this.goTo(page, false);
    this.updateLive();
    const side = viewPrefs().side;
    if (side) this.openSide(side, false);
    // The keys scroll the pages from the start, without a click on them first.
    this.scroller.focus({ preventScroll: true });
    if (!known) this.measureAll();
  }

  /* ----- page sizes and page numbers ----- */

  /** Page sizes kept from an earlier visit, put in place; false when there are none for this book. */
  cachedSizes() {
    const n = Math.min(this.book.pages, this.docs[0].numPages);
    try {
      const c = JSON.parse(localStorage.getItem(`celsus-sizes-${this.book.id}`));
      if (c?.n !== n || !Array.isArray(c.runs) || c.runs.reduce((a, r) => a + r[0], 0) !== n) return false;
      let p = 1;
      for (const [count, w, h] of c.runs) for (let i = 0; i < count; i++) this.sizes.set(p++, [w, h]);
      return true;
    } catch {
      return false;
    }
  }

  /** Read the sizes of pages `from` to `to` that are not known yet. True when one of them differs from the
      size the rows were given before it was known (the first page's). */
  async measure(from, to) {
    const doc = this.docs[0];
    const n = Math.min(this.book.pages, doc.numPages);
    const wanted = [];
    for (let p = Math.max(1, from); p <= Math.min(n, to); p++) if (!this.sizes.has(p)) wanted.push(p);
    const [w0, h0] = this.defaultSize;
    let differs = false;
    await Promise.all(wanted.map(async (p) => {
      try {
        const vp = (await doc.getPage(p)).getViewport({ scale: 1 });
        this.sizes.set(p, [vp.width, vp.height]);
        if (Math.abs(vp.width - w0) > 0.5 || Math.abs(vp.height - h0) > 0.5) differs = true;
      } catch {}
    }));
    return differs;
  }

  /** Read every page's size, a few dozen pages at a time, and keep them for the next visit. Rows are sized
      again when sizes that differ come in, with the spot being read kept where it is on screen. */
  async measureAll() {
    const n = Math.min(this.book.pages, this.docs[0].numPages);
    for (let p = 1; p <= n; p += 48) {
      const differs = await this.measure(p, p + 47);
      if (!this.alive) return;
      if (differs) this.layout(true);
    }
    // Kept as runs of equal sizes: a book whose pages are all one size takes a few bytes. Sizes are kept only
    // when every page could be read.
    const runs = [];
    for (let p = 1; p <= n; p++) {
      if (!this.sizes.has(p)) return;
      const [w, h] = this.sizes.get(p).map((x) => Math.round(x * 100) / 100);
      const last = runs[runs.length - 1];
      if (last && last[1] === w && last[2] === h) last[0]++;
      else runs.push([1, w, h]);
    }
    if (runs.length <= 500) {
      try { localStorage.setItem(`celsus-sizes-${this.book.id}`, JSON.stringify({ n, runs })); } catch {}
    }
  }

  /** The page numbers the book prints (i, ii, 1, 2…), kept only when some differ from the pages' places. */
  async loadLabels() {
    for (const doc of this.docs) {
      const labels = await doc.getPageLabels().catch(() => null);
      if (labels?.some((l, i) => l && l !== String(i + 1))) { this.labels = labels; break; }
    }
    if (!this.labels) return;
    const box = $("#rd-page");
    box.title = RS.pageBoxTitle;
    // A number such as "xii" can only be typed into a box that takes letters.
    if (this.labels.some((l) => l && !/^\d+$/.test(l.trim()))) box.type = "text";
  }

  /** Show page `p` in the page box, with the number the book prints on it when that is another. */
  showPageNo(p) {
    const box = $("#rd-page"), tag = $("#rd-label");
    if (!box) return;
    box.value = p;
    const label = this.labels?.[p - 1];
    tag.hidden = !label || label === String(p);
    tag.textContent = tag.hidden ? "" : RS.printedAs(label);
  }

  /** The page meant by what was typed into the page box: a number is the page's place in the file, anything
      else is looked for among the numbers the book prints. Null when it is neither. */
  pageFromBox(text) {
    const t = String(text).trim();
    if (/^\d+$/.test(t)) return Number(t);
    const i = this.labels ? this.labels.findIndex((l) => l && l.trim().toLowerCase() === t.toLowerCase()) : -1;
    return i >= 0 ? i + 1 : null;
  }

  /* ----- the side panel: the book's contents, and search ----- */

  openSide(tab, focus = true) {
    const side = $("#rd-side");
    const wasOpen = !side.hidden;
    side.hidden = false;
    this.sideTab = tab;
    saveViewPrefs({ side: tab });
    for (const b of $$("#rd-side-tabs button")) b.setAttribute("aria-pressed", String(b.dataset.side === tab));
    $("#rd-contents").setAttribute("aria-pressed", String(tab === "contents"));
    $("#rd-find").setAttribute("aria-pressed", String(tab === "find"));
    if (tab === "contents") this.showContents();
    else this.showFind(focus);
    // The pages have less room beside the panel.
    if (!wasOpen) this.layout(true);
  }

  closeSide() {
    $("#rd-side").hidden = true;
    saveViewPrefs({ side: null });
    $("#rd-contents").setAttribute("aria-pressed", "false");
    $("#rd-find").setAttribute("aria-pressed", "false");
    this.layout(true);
  }

  /** The outline the PDF carries (what a browser shows beside a PDF), with its titles in the language read. */
  async outline() {
    if (this.outlineItems) return this.outlineItems;
    const shown = this.docs[this.docs.length - 1];
    let items = await shown.getOutline().catch(() => null);
    let doc = shown;
    if (!items?.length && this.docs.length > 1) {
      doc = this.docs[0];
      items = await doc.getOutline().catch(() => null);
    }
    const flat = [];
    const walk = async (list, depth) => {
      for (const it of list || []) {
        let page = null;
        try {
          let dest = it.dest;
          if (typeof dest === "string") dest = await doc.getDestination(dest);
          if (Array.isArray(dest)) {
            const ref = dest[0];
            page = typeof ref === "number" ? ref + 1 : (await doc.getPageIndex(ref)) + 1;
          }
        } catch {}
        flat.push({ title: (it.title || "").trim(), page, depth });
        if (flat.length < 3000) await walk(it.items, depth + 1);
      }
    };
    await walk(items, 0);
    // Titles are headings of the book, translated with its pages: show them as the pages show them.
    if (this.view.lang !== "orig" && flat.length) {
      const got = await invoke("cached_translations", { lang: this.view.lang, texts: flat.map((f) => f.title) }).catch(() => null);
      if (got) flat.forEach((f, i) => { if (got[i]) f.title = got[i]; });
    }
    this.outlineItems = flat;
    return flat;
  }

  async showContents() {
    const body = $("#rd-side-body");
    body.innerHTML = `<p class="rd-side-note">${RS.loading}</p>`;
    const items = await this.outline();
    if (!this.alive || this.sideTab !== "contents") return;
    if (!items.length) { body.innerHTML = `<p class="rd-side-note">${RS.noContents}</p>`; return; }
    body.innerHTML = `<nav class="rd-toc">${items.map((it, i) => `<button data-toc="${i}" style="--depth:${Math.min(it.depth, 5)}" ${it.page ? "" : "disabled"}>
      <span>${esc(it.title || "—")}</span>${it.page ? `<small>${it.page}</small>` : ""}</button>`).join("")}</nav>`;
    for (const b of $$("[data-toc]", body)) b.onclick = () => this.goTo(items[Number(b.dataset.toc)].page, false);
    this.markContents(true);
  }

  /** Mark in the contents the part the page being read belongs to. */
  markContents(reveal) {
    if (this.sideTab !== "contents" || $("#rd-side").hidden || !this.outlineItems) return;
    let at = -1;
    this.outlineItems.forEach((it, i) => { if (it.page && it.page <= this.page) at = i; });
    for (const b of $$("[data-toc]")) {
      const on = Number(b.dataset.toc) === at;
      b.classList.toggle("here", on);
      if (on && reveal) b.scrollIntoView({ block: "center" });
    }
  }

  showFind(focus = true) {
    const body = $("#rd-side-body");
    const f = this.found || { query: "", hits: [] };
    body.innerHTML = `<form class="rd-find" id="rd-find-form">
        <input id="rd-find-q" type="search" placeholder="${esc(RS.findPlaceholder)}" value="${esc(f.query)}" autocomplete="off">
      </form>
      <p class="rd-side-note" id="rd-find-status"></p>
      <div class="rd-hits" id="rd-hits"></div>`;
    const q = $("#rd-find-q");
    if (focus) {
      q.focus();
      q.select();
    }
    if (f.query && !this.findRun) $("#rd-find-status").textContent = this.foundText();
    $("#rd-find-form").onsubmit = (e) => {
      e.preventDefault();
      const query = q.value.trim();
      if (query && query === this.found?.query && this.found.hits.length) this.nextHit(1);
      else this.search(query);
    };
    this.drawHits();
  }

  /** The text of a page of document `d`, with where each piece of it begins in that text. */
  async pageText(d, p) {
    this.texts ||= new Map();
    const key = `${d}:${p}`;
    if (this.texts.has(key)) return this.texts.get(key);
    const doc = this.docs[d];
    const page = await doc.getPage(Math.min(p, doc.numPages));
    const content = await page.getTextContent();
    let text = "";
    const pieces = [];
    for (const item of content.items) {
      if (typeof item.str !== "string") continue;
      // Pieces of one line seldom say where the spaces between them are: one is put in, and runs are one.
      if (text && !/\s$/.test(text) && !/^\s/.test(item.str)) text += " ";
      pieces.push({ start: text.length, item });
      text += item.str + (item.hasEOL ? " " : "");
    }
    // What is matched: the letters alone, folded, with no spaces, and no hyphen where a word was broken at the
    // end of a line. A word in capitals, set in small capitals or broken over two lines is found as it is
    // written. `at` says where each letter stands in `text`.
    let letters = "";
    const at = [];
    for (let i = 0; i < text.length; i++) {
      const c = text[i];
      if (/\s/.test(c)) continue;
      if ((c === "-" || c === "­") && /^\s/.test(text.slice(i + 1, i + 2))) continue;
      for (const f of fold(c)) {
        letters += f;
        at.push(i);
      }
    }
    const entry = { text, letters, at, pieces };
    this.texts.set(key, entry);
    return entry;
  }

  /** Look for `query` in the pages shown, page by page; each page is listed as soon as it is read. The first
      place found is shown at once, unless the reader has moved since the search began. */
  async search(query) {
    const run = {};
    this.findRun = run;
    this.found = { query, hits: [], at: -1, capped: false };
    this.clearHighlights();
    const status = $("#rd-find-status");
    if (!query) { this.findRun = null; this.drawHits(); if (status) status.textContent = ""; return; }
    const needle = [...query.replace(/\s+/g, "")].map(fold).join("");
    const pages = this.book.pages;
    const LIMIT = 1000;
    const moves = this.moves;
    let shown = false;
    for (let p = 1; p <= pages && !this.found.capped; p++) {
      for (let d = 0; d < this.docs.length && !this.found.capped; d++) {
        const page = await this.pageText(d, p).catch(() => null);
        if (this.findRun !== run || !this.alive) return;
        if (!page) continue;
        let from = 0, k;
        while ((k = page.letters.indexOf(needle, from)) >= 0) {
          // One more than are listed: the count says so, and the rest of the book is not read.
          if (this.found.hits.length >= LIMIT) { this.found.capped = true; break; }
          // Where it stands in the text as written, from its first letter to just after its last.
          const start = page.at[k], end = page.at[k + needle.length - 1] + 1;
          this.found.hits.push({
            page: p, doc: d, at: start, end,
            before: page.text.slice(Math.max(0, start - 40), start),
            match: page.text.slice(start, end),
            after: page.text.slice(end, end + 60),
          });
          from = k + needle.length;
        }
      }
      if (!shown && this.found.hits.length) {
        shown = true;
        this.drawHits();
        if (this.found.at < 0 && this.moves === moves) this.showHit(0);
      }
      if (p % 20 === 0 || p === pages || this.found.capped) {
        const st = $("#rd-find-status");
        if (st) st.textContent = p === pages || this.found.capped ? this.foundText() : RS.searching(p, pages);
        this.drawHits();
        await new Promise((r) => setTimeout(r));
        if (this.findRun !== run || !this.alive) return;
      }
    }
    this.findRun = null;
    for (const p of this.drawnRows) this.highlight(p);
  }

  /** How many places were found, said as "1000+" when the search stopped at its limit. */
  foundText() {
    const n = this.found?.hits.length || 0;
    return this.found?.capped ? RS.foundMany(n) : RS.found(n);
  }

  drawHits() {
    const box = $("#rd-hits");
    if (!box || !this.found) return;
    const side = (d) => (this.docs.length > 1 ? (d === 0 ? RS.original : langName(this.view.lang)) : "");
    box.innerHTML = this.found.hits.slice(0, 300).map((h, i) => {
      const text = `${esc(h.before)}<mark>${esc(h.match)}</mark>${esc(h.after)}`;
      return `<button data-hit="${i}" class="${i === this.found.at ? "here" : ""}"><small>${RS.pageN(h.page)}${side(h.doc) ? ` · ${esc(side(h.doc))}` : ""}</small><span>${text}</span></button>`;
    }).join("");
    for (const b of $$("[data-hit]", box)) b.onclick = () => this.showHit(Number(b.dataset.hit));
  }

  nextHit(step) {
    const f = this.found;
    if (!f?.hits.length) return;
    this.showHit((f.at + step + f.hits.length) % f.hits.length);
  }

  showHit(i) {
    const f = this.found;
    f.at = i;
    for (const b of $$("[data-hit]")) b.classList.toggle("here", Number(b.dataset.hit) === i);
    $(`[data-hit="${i}"]`)?.scrollIntoView({ block: "nearest" });
    this.goTo(f.hits[i].page, false);
    this.highlight(f.hits[i].page);
  }

  clearHighlights() {
    for (const el of $$(".rd-hit", this.pagesBox)) el.remove();
  }

  /** Mark where the words looked for stand on page `p`, in each document shown. */
  async highlight(p) {
    const f = this.found;
    const row = this.rows[p - 1];
    if (!row) return;
    for (const el of $$(".rd-hit", row.el)) el.remove();
    if (!f?.query) return;
    for (let d = 0; d < this.docs.length; d++) {
      const hits = f.hits.filter((h) => h.page === p && h.doc === d);
      if (!hits.length) continue;
      const { pieces } = await this.pageText(d, p);
      const doc = this.docs[d];
      const page = await doc.getPage(Math.min(p, doc.numPages));
      const slot = row.slots[d];
      const vp = page.getViewport({ scale: this.pageWidth(p) / page.getViewport({ scale: 1 }).width });
      for (const h of hits) {
        const end = h.end;
        for (const { start, item } of pieces) {
          const stop = start + item.str.length;
          if (stop <= h.at || start >= end || !item.str.length) continue;
          // The part of this piece that is looked for, by the share of its letters.
          const a = Math.max(h.at, start) - start, b = Math.min(end, stop) - start;
          const [, , c, dd, e, y] = item.transform;
          const tall = Math.hypot(c, dd) || item.height || 10;
          const x0 = e + (item.width * a) / item.str.length, x1 = e + (item.width * b) / item.str.length;
          const [l, t, r, bt] = vp.convertToViewportRectangle([x0, y - tall * 0.22, x1, y + tall * 0.8]);
          const mark = document.createElement("div");
          mark.className = "rd-hit" + (f.hits[f.at] === h ? " here" : "");
          Object.assign(mark.style, { left: `${Math.min(l, r)}px`, top: `${Math.min(t, bt)}px`, width: `${Math.abs(r - l)}px`, height: `${Math.abs(bt - t)}px` });
          slot.el.appendChild(mark);
        }
      }
    }
  }

  async loadTranslated() {
    if (this.view.lang === "orig") { this.translated = new Set(); return; }
    // The pages known before stay known while the list is asked for, and when it cannot be had.
    const pages = await invoke("book_pages", { id: this.book.id, lang: this.view.lang }).catch(() => null);
    if (pages) this.translated = new Set(pages.map(([p]) => p));
  }

  bindBar() {
    const v = this.view;
    $("#rd-back").onclick = () => go("library");
    $("#rd-contents").onclick = () => (!$("#rd-side").hidden && this.sideTab === "contents" ? this.closeSide() : this.openSide("contents"));
    $("#rd-find").onclick = () => (!$("#rd-side").hidden && this.sideTab === "find" ? this.closeSide() : this.openSide("find"));
    $("#rd-side-close").onclick = () => this.closeSide();
    for (const b of $$("#rd-side-tabs button")) b.onclick = () => this.openSide(b.dataset.side);
    // A view chosen here is remembered for the book, so that a translation coming in later does not take
    // the place of the original someone chose to read.
    for (const b of $$("#rd-lang button")) {
      b.setAttribute("aria-pressed", String(b.dataset.lang === v.lang));
      b.onclick = () => {
        saveReadingPrefs(this.book.id, { chose: true });
        go("reader", { id: this.book.id, lang: b.dataset.lang, page: this.page });
      };
    }
    const split = $("#rd-split");
    split.setAttribute("aria-pressed", String(v.split));
    split.onclick = () => {
      saveViewPrefs({ split: !v.split });
      saveReadingPrefs(this.book.id, { chose: true });
      go("reader", { id: this.book.id, lang: v.lang === "orig" ? Object.keys(this.files.outputs)[0] : v.lang, page: this.page });
    };
    for (const b of $$("#rd-mode button")) {
      b.setAttribute("aria-pressed", String(b.dataset.mode === v.mode));
      b.onclick = () => {
        if (v.mode === b.dataset.mode) return;
        v.mode = b.dataset.mode;
        saveViewPrefs({ mode: v.mode });
        for (const x of $$("#rd-mode button")) x.setAttribute("aria-pressed", String(x === b));
        this.layout(false);
        this.goTo(this.page, false);
      };
    }
    $("#rd-zoom-in").onclick = () => this.setZoom(v.zoom * 1.15);
    $("#rd-zoom-out").onclick = () => this.setZoom(v.zoom / 1.15);
    $("#rd-zoom-val").onclick = () => this.setZoom(1);
    $("#rd-page").onchange = (e) => {
      const p = this.pageFromBox(e.target.value);
      if (p) this.goTo(p, true);
      else this.showPageNo(this.page);
    };
    $("#rd-prev").onclick = () => this.goTo(this.page - 1, true);
    $("#rd-next").onclick = () => this.goTo(this.page + 1, true);
    $("#rd-translate").onclick = (e) => { e.stopPropagation(); this.togglePop(); };
    $("#rd-more").onclick = (e) => { e.stopPropagation(); $("#rd-light-menu").hidden = true; $("#rd-menu").hidden = !$("#rd-menu").hidden; };
    $("#rd-light").onclick = (e) => { e.stopPropagation(); $("#rd-menu").hidden = true; $("#rd-light-menu").hidden = !$("#rd-light-menu").hidden; };
    this.bindLight();
    for (const b of $$("#rd-menu button")) b.onclick = async () => {
      $("#rd-menu").hidden = true;
      if (b.dataset.act === "details") go("book", { id: this.book.id });
      if (b.dataset.act === "save") {
        const p = await call("save_output", { id: this.book.id, lang: v.lang });
        if (p) toast(T.savedTo(p));
      }
    };
    $(".rd").onclick = (e) => {
      if (!e.target.closest(".rd-pop, #rd-translate")) $("#rd-pop").hidden = true;
      if (!e.target.closest(".rd-menu-wrap")) $("#rd-menu").hidden = true;
      if (!e.target.closest("#rd-light-wrap")) $("#rd-light-menu").hidden = true;
    };
    // A button pressed with the mouse hands the keys back to the pages: they scroll them again, and Space
    // does not press the button a second time. A button reached with Tab keeps the keys (detail is 0 for a
    // click made from the keyboard), and so does a field the button has just opened. This is looked at
    // once the click is over, as some buttons keep their clicks to themselves.
    $(".rd").addEventListener("click", (e) => {
      const button = e.target.closest("button");
      if (!button || e.detail === 0) return;
      setTimeout(() => {
        const at = document.activeElement;
        if (this.alive && (at === button || at === document.body || !at)) this.scroller.focus({ preventScroll: true });
      });
    }, true);
    this.scroller.addEventListener("scroll", () => this.onScroll(), { passive: true });
    this.scroller.addEventListener("pointerdown", () => this.moves++, { passive: true });
    this.scroller.addEventListener("wheel", (e) => {
      if (e.ctrlKey) {
        e.preventDefault();
        this.setZoom(v.zoom * (e.deltaY < 0 ? 1.1 : 1 / 1.1));
        return;
      }
      this.moves++;
      if (v.mode !== "paged" || !e.deltaY) return;
      // In page mode the wheel turns the page. Zoomed in, it first moves over the page as usual, and turns it
      // only from the page's bottom (or, going back, its top), and not while a scroll over the page goes on.
      const s = this.scroller;
      const down = e.deltaY > 0;
      const atEdge = down ? s.scrollTop + s.clientHeight >= s.scrollHeight - 2 : s.scrollTop <= 1;
      if (!atEdge) { this.wheelOver = performance.now(); return; }
      e.preventDefault();
      // The wheel still turning from a scroll that reached the edge waits for a pause first.
      if (performance.now() - (this.wheelOver || 0) < 300) { this.wheelOver = performance.now(); return; }
      if (Math.abs(e.deltaY) <= 30 || this.wheelLock) return;
      this.wheelLock = true;
      setTimeout(() => (this.wheelLock = false), 250);
      // Going back, the page before is shown from its bottom, where reading left it.
      if (down) this.goTo(this.page + 1, true);
      else this.goTo(this.page - 1, true, 1);
    }, { passive: false });
  }

  /** The reading light's panel: what it shows follows the light chosen, and each change is seen at once. */
  bindLight() {
    const show = () => {
      const l = readingLight();
      for (const b of $$("#rd-light-mode button")) b.setAttribute("aria-pressed", String(b.dataset.light === l.mode));
      $("#rd-warmth").value = Math.round(l.warmth * 100);
      $("#rd-brightness").value = Math.round(l.brightness * 100);
      $("#rd-warmth-row").hidden = l.mode === "paper";
      $("#rd-light-note").textContent = l.mode === "dark" ? RS.lightDarkNote : l.mode === "night" ? RS.lightNightNote : "";
      $("#rd-light-note").hidden = l.mode === "paper";
      $("#rd-light").setAttribute("aria-pressed", String(l.mode !== "paper" || l.brightness < 1));
      applyReadingLight($(".rd"), l);
    };
    const set = (change) => { saveViewPrefs({ light: { ...readingLight(), ...change } }); show(); };
    for (const b of $$("#rd-light-mode button")) b.onclick = () => set({ mode: b.dataset.light });
    $("#rd-warmth").oninput = (e) => set({ warmth: Number(e.target.value) / 100 });
    $("#rd-brightness").oninput = (e) => set({ brightness: Number(e.target.value) / 100 });
    this.toggleLight = () => set({ mode: readingLight().mode === "night" ? "paper" : "night" });
    show();
  }

  /* ----- layout ----- */

  size(page) {
    return this.sizes.get(page) || this.defaultSize;
  }

  /** Width of one page on screen, in CSS pixels. */
  pageWidth(page) {
    const cols = this.docs.length;
    const box = this.scroller.clientWidth - 48 - (cols - 1) * 16;
    const [w, h] = this.size(page);
    let width = Math.min(box / cols, cols === 1 ? 920 : 760);
    if (this.view.mode === "paged") {
      const tall = this.scroller.clientHeight - 32;
      width = Math.min(box / cols, (tall * w) / h);
    }
    return Math.max(160, width * this.view.zoom);
  }

  build() {
    this.observer?.disconnect();
    this.pagesBox.innerHTML = "";
    this.rows = [];
    const frag = document.createDocumentFragment();
    for (let p = 1; p <= this.book.pages; p++) {
      const el = document.createElement("div");
      el.className = "rd-row";
      el.dataset.page = p;
      const slots = this.docs.map((_, i) => {
        const s = document.createElement("div");
        s.className = "rd-page";
        if (this.docs.length > 1) s.dataset.side = i === 0 ? RS.original : langName(this.view.lang);
        el.appendChild(s);
        // drawnFrom: the file the picture was drawn from; text, links, layersOf: what is laid over it.
        return { el: s, canvas: null, task: null, drawn: 0, drawnFrom: null, text: null, links: null, layersOf: null };
      });
      if (this.view.lang !== "orig" && !this.translated.has(p)) slots[slots.length - 1].el.appendChild(this.badge(p));
      frag.appendChild(el);
      this.rows.push({ el, slots });
    }
    this.pagesBox.appendChild(frag);
    this.pagesBox.onclick = (e) => {
      const from = e.target.closest("[data-from]");
      if (from) this.enqueue(this.view.lang, this.rangeFrom(Number(from.dataset.from), 10));
    };
    this.observer = new IntersectionObserver((entries) => {
      for (const en of entries) {
        const p = Number(en.target.dataset.page);
        if (en.isIntersecting) this.draw(p);
        else this.release(p);
      }
    }, { root: this.scroller, rootMargin: "120% 0px" });
    for (const row of this.rows) this.observer.observe(row.el);
    this.layout(false);
  }

  /** The note on a page not translated yet, with a button to translate from there. */
  badge(p) {
    const badge = document.createElement("div");
    badge.className = "rd-untranslated";
    badge.innerHTML = `<span>${RS.notTranslated}</span><button class="text-button" data-from="${p}">${RS.fromHere}</button>`;
    return badge;
  }

  /** Size the rows. With `keepPlace`, the spot at the top of the view stays where it is on screen, however
      the rows above it changed. */
  layout(keepPlace) {
    if (!this.rows.length) return;
    const v = this.view;
    const place = keepPlace ? this.place() : null;
    $(".rd").classList.toggle("paged", v.mode === "paged");
    $("#rd-prev").hidden = $("#rd-next").hidden = v.mode !== "paged";
    $("#rd-zoom-val").textContent = `${Math.round(v.zoom * 100)}%`;
    const page = this.page;
    this.rows.forEach((row, i) => {
      const p = i + 1;
      const [w, h] = this.size(p);
      const width = this.pageWidth(p);
      for (const s of row.slots) {
        s.el.style.width = `${width}px`;
        s.el.style.height = `${(width * h) / w}px`;
        // The text and links laid over the page follow its size at once; the picture of the page is drawn
        // again at the new size when the page is next seen.
        this.scaleLayers(s, width);
        if (s.drawn && Math.abs(s.drawn - width) > 1) s.stale = true;
      }
      row.el.hidden = v.mode === "paged" && p !== page;
    });
    if (place) this.moveTo(place.page, false, place.frac, true);
    for (const p of [...this.drawnRows]) if (this.rows[p - 1].slots.some((s) => s.stale)) this.draw(p);
  }

  /** The row under the line `y` (in the scrolled box's own height), found by halving. */
  rowAt(y) {
    let lo = 0, hi = this.rows.length - 1;
    while (lo < hi) {
      const m = (lo + hi + 1) >> 1;
      if (this.rows[m].el.offsetTop <= y) lo = m; else hi = m - 1;
    }
    return lo;
  }

  /** Where the view stands: the page at its top, and how far down that page (0 to 1) the top is. */
  place() {
    if (!this.rows.length) return { page: this.page, frac: 0 };
    const top = this.scroller.scrollTop + 16;
    const i = this.view.mode === "paged" ? this.page - 1 : this.rowAt(top);
    const el = this.rows[i].el;
    return { page: i + 1, frac: el.offsetHeight ? (top - el.offsetTop) / el.offsetHeight : 0 };
  }

  setZoom(z) {
    this.view.zoom = Math.max(0.4, Math.min(3, Math.round(z * 100) / 100));
    saveViewPrefs({ zoom: this.view.zoom });
    this.layout(true);
  }

  /* ----- drawing ----- */

  async draw(p) {
    const row = this.rows[p - 1];
    if (!row || !this.alive) return;
    this.drawnRows.add(p);
    // The translation of this page changed since the translated file was read: read it again first.
    if (this.stale.has(p)) this.refreshSoon();
    // A page whose size could not be read with the others is measured now.
    if (!this.sizes.has(p)) {
      const pg = await this.docs[0].getPage(Math.min(p, this.docs[0].numPages));
      const vp = pg.getViewport({ scale: 1 });
      this.sizes.set(p, [vp.width, vp.height]);
      if (Math.abs(vp.width / vp.height - this.defaultSize[0] / this.defaultSize[1]) > 0.01) this.layout(true);
    }
    const docs = this.docs;
    await Promise.all(row.slots.map((s, i) => this.drawSlot(s, docs[i], p)));
  }

  async drawSlot(slot, doc, p) {
    const width = this.pageWidth(p);
    if (slot.canvas && !slot.stale && Math.abs(slot.drawn - width) <= 1 && slot.drawnFrom === doc) {
      // Drawn already. Only its text and links can be missing, when they could not be read before.
      if (slot.layersOf !== doc) {
        const page = await doc.getPage(Math.min(p, doc.numPages)).catch(() => null);
        if (page && this.alive && slot.canvas) this.addLayers(slot, doc, page, page.getViewport({ scale: 1 }));
      }
      return;
    }
    slot.task?.cancel();
    const page = await doc.getPage(Math.min(p, doc.numPages));
    if (!this.alive) return;
    const base = page.getViewport({ scale: 1 });
    const ratio = window.devicePixelRatio || 1;
    const vp = page.getViewport({ scale: (width / base.width) * ratio });
    const canvas = document.createElement("canvas");
    canvas.width = Math.floor(vp.width);
    canvas.height = Math.floor(vp.height);
    slot.task = page.render({ canvasContext: canvas.getContext("2d", { alpha: false }), viewport: vp });
    try {
      await slot.task.promise;
    } catch {
      return; // cancelled
    }
    slot.task = null;
    if (!this.alive || !this.drawnRows.has(p)) return;
    slot.canvas?.remove();
    slot.el.prepend(canvas);
    slot.canvas = canvas;
    slot.drawn = width;
    slot.drawnFrom = doc;
    slot.stale = false;
    // The page's text and links are laid over it once for each file it is drawn from; a new size only
    // scales them.
    if (slot.layersOf !== doc) this.addLayers(slot, doc, page, base);
    // The marks of a search are placed for the page's size: place them again.
    if (this.found?.hits.some((h) => h.page === p)) this.highlight(p);
  }

  /** Lay over a drawn page its text, invisible but there to be selected and copied, and its links. Both are
      placed in the page's own measures, so that a new zoom only has to change `--scale-factor`. */
  async addLayers(slot, doc, page, base) {
    slot.layersOf = doc;
    const text = document.createElement("div");
    text.className = "textLayer";
    text.style.setProperty("--scale-factor", String(slot.drawn / base.width));
    const textLayer = new pdfjsLib.TextLayer({
      textContentSource: page.streamTextContent({ includeMarkedContent: true, disableNormalization: true }),
      container: text,
      viewport: page.getViewport({ scale: slot.drawn / base.width }),
    });
    let annotations;
    try {
      [, annotations] = await Promise.all([textLayer.render(), page.getAnnotations({ intent: "display" })]);
    } catch {
      if (slot.layersOf === doc) slot.layersOf = null; // tried again when the page is next drawn
      return;
    }
    // The page was let go, or drawn from a newer file, while its text was being read.
    if (!this.alive || slot.layersOf !== doc || !slot.canvas) return;
    // What pdf.js puts after the text: while a selection is drawn it covers the page, so that the selection
    // does not jump when the mouse passes between lines.
    const end = document.createElement("div");
    end.className = "endOfContent";
    text.append(end);
    text.addEventListener("pointerdown", () => text.classList.add("selecting"));
    const links = document.createElement("div");
    links.className = "rd-links";
    for (const a of annotations || []) {
      if (a.subtype !== "Link" || !a.rect) continue;
      // Only a web address pdf.js found sound is opened; a link to another file is left alone.
      const link = a.url ? { url: a.url } : a.dest ? { dest: a.dest } : a.action ? { action: a.action } : null;
      if (!link) continue;
      const [x1, y1, x2, y2] = base.convertToViewportRectangle(a.rect);
      const el = document.createElement("div");
      el.className = "rd-link";
      el.setAttribute("role", "link");
      if (link.url) el.title = link.url;
      Object.assign(el.style, {
        left: `${(100 * Math.min(x1, x2)) / base.width}%`,
        top: `${(100 * Math.min(y1, y2)) / base.height}%`,
        width: `${(100 * Math.abs(x2 - x1)) / base.width}%`,
        height: `${(100 * Math.abs(y2 - y1)) / base.height}%`,
      });
      el.onclick = (e) => { e.preventDefault(); this.follow(doc, link); };
      links.append(el);
    }
    slot.text?.remove();
    slot.links?.remove();
    slot.canvas.after(text, links);
    slot.text = text;
    slot.links = links;
    slot.pageWidth = base.width;
    // The zoom may have changed while the text was being read.
    this.scaleLayers(slot, parseFloat(slot.el.style.width) || slot.drawn);
  }

  /** Fit the text laid over a page to the page's width on screen. The links are placed in shares of the page
      and need nothing. */
  scaleLayers(slot, width) {
    if (slot.text && slot.pageWidth) slot.text.style.setProperty("--scale-factor", String(width / slot.pageWidth));
  }

  /** Follow a link of the book: to a place in it, a page turn, or a web page, opened in the browser. */
  async follow(doc, link) {
    if (link.url) { this.openLink(link.url); return; }
    if (link.action) {
      const p = { NextPage: this.page + 1, PrevPage: this.page - 1, FirstPage: 1, LastPage: this.book.pages }[link.action];
      if (p) this.goTo(p, true);
      return;
    }
    try {
      let dest = link.dest;
      if (typeof dest === "string") dest = await doc.getDestination(dest);
      if (!Array.isArray(dest)) return;
      const ref = dest[0];
      const p = (typeof ref === "number" ? ref : await doc.getPageIndex(ref)) + 1;
      // How far down the page the link points, when it says.
      const kind = dest[1]?.name;
      const y = kind === "XYZ" ? dest[3] : kind === "FitH" || kind === "FitBH" ? dest[2] : kind === "FitR" ? dest[5] : null;
      let at = null;
      if (typeof y === "number") {
        const vp = (await doc.getPage(p)).getViewport({ scale: 1 });
        const x = kind === "XYZ" && typeof dest[2] === "number" ? dest[2] : 0;
        at = Math.max(0, Math.min(1, vp.convertToViewportPoint(x, y)[1] / vp.height));
      }
      if (this.alive) this.goTo(p, true, at);
    } catch {}
  }

  /** Open a web address in the user's browser, never in the app's own window. */
  openLink(url) {
    if (window.__TAURI__) call("open_url", { url }).catch(() => {});
    else window.open(url, "_blank", "noopener,noreferrer");
  }

  /** Free a page that scrolled far away. */
  release(p) {
    const row = this.rows[p - 1];
    if (!row) return;
    this.drawnRows.delete(p);
    for (const s of row.slots) {
      s.task?.cancel();
      s.task = null;
      if (s.canvas) {
        s.canvas.width = s.canvas.height = 0;
        s.canvas.remove();
        s.canvas = null;
        s.drawn = 0;
      }
      s.text?.remove();
      s.links?.remove();
      s.text = s.links = s.layersOf = s.drawnFrom = null;
    }
  }

  /* ----- moving around ----- */

  /** Go to page `p` because the reader asked to; `at` (0 to 1) says how far down the page, else its top. */
  goTo(p, smooth, at = null) {
    this.moves++;
    this.moveTo(p, smooth, at);
  }

  /** Bring page `p` into view, `at` down it. The scroll is smooth only to a page close by: a smooth scroll
      over a long way would draw every page on the way. `quiet` leaves the page being read to be found from
      the scroll, as when the rows are only sized again. */
  moveTo(p, smooth, at = null, quiet = false) {
    p = Math.max(1, Math.min(this.book.pages, p || 1));
    const changed = p !== this.page;
    smooth = smooth && Math.abs(p - this.page) <= 3;
    const paged = this.view.mode === "paged";
    if (!quiet || paged) {
      this.page = p;
      this.showPageNo(p);
      saveReadingPrefs(this.book.id, { page: p });
      this.markContents(false);
    }
    const row = this.rows[p - 1];
    if (!row) return;
    if (paged) {
      if (changed || !row.el.offsetParent) {
        this.rows.forEach((r, i) => (r.el.hidden = i + 1 !== p));
      }
      this.scroller.scrollTop = at == null ? 0 : Math.max(0, row.el.offsetTop + at * row.el.offsetHeight - 16);
      this.draw(p);
      // Draw the next page ahead of time so turning is instant.
      if (p < this.book.pages) setTimeout(() => this.drawAhead(p + 1), 60);
      return;
    }
    const top = row.el.offsetTop - 16 + (at == null ? 0 : at * row.el.offsetHeight);
    this.scroller.scrollTo({ top, behavior: smooth ? "smooth" : "auto" });
  }

  async drawAhead(p) {
    const row = this.rows[p - 1];
    if (!row || !this.alive) return;
    // A hidden row has no size; draw it with the current page's size.
    this.drawnRows.add(p);
    const docs = this.docs;
    await Promise.all(row.slots.map((s, i) => this.drawSlot(s, docs[i], p)));
  }

  onScroll() {
    if (this.view.mode === "paged" || this.scrollFrame) return;
    this.scrollFrame = requestAnimationFrame(() => {
      this.scrollFrame = null;
      if (!this.alive) return;
      // The row under the reading line.
      const p = this.rowAt(this.scroller.scrollTop + this.scroller.clientHeight * 0.35) + 1;
      if (p !== this.page) {
        this.page = p;
        this.showPageNo(p);
        this.markContents(false);
        clearTimeout(this.saveTimer);
        this.saveTimer = setTimeout(() => saveReadingPrefs(this.book.id, { page: p }), 300);
      }
    });
  }

  /** Scroll the pages by `dy` pixels for a key. Presses that follow one another quickly add up, though the
      smooth scroll of the one before has not ended yet. */
  scrollByKey(dy, instant) {
    const s = this.scroller;
    this.moves++;
    const now = performance.now();
    const from = !instant && this.keyScroll && now - this.keyScroll.at < 500 ? this.keyScroll.top : s.scrollTop;
    const top = Math.max(0, Math.min(s.scrollHeight - s.clientHeight, from + dy));
    this.keyScroll = instant ? null : { top, at: now };
    s.scrollTo({ top, behavior: instant ? "auto" : "smooth" });
  }

  key(e) {
    if (state.route.view !== "reader") return;
    // Ctrl+F looks in the book; F3 goes to the next place found, Shift+F3 to the one before.
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") { e.preventDefault(); this.openSide("find"); return; }
    if (e.key === "F3" && this.found?.hits.length) { e.preventDefault(); this.nextHit(e.shiftKey ? -1 : 1); return; }
    if (e.target.matches?.("input, textarea, select, [contenteditable]")) return;
    // A button reached with Tab is pressed with Space or Enter, as anywhere else.
    if ((e.key === " " || e.key === "Enter") && e.target.closest?.("button, [role=button]")) return;
    const paged = this.view.mode === "paged";
    const plain = !e.ctrlKey && !e.metaKey && !e.altKey;
    if ((e.ctrlKey || e.metaKey) && (e.key === "+" || e.key === "=")) { e.preventDefault(); this.setZoom(this.view.zoom * 1.15); return; }
    if ((e.ctrlKey || e.metaKey) && e.key === "-") { e.preventDefault(); this.setZoom(this.view.zoom / 1.15); return; }
    if ((e.ctrlKey || e.metaKey) && e.key === "0") { e.preventDefault(); this.setZoom(1); return; }
    // Up and down move a little over the pages, in both modes (in page mode, over a page zoomed in).
    if (plain && (e.key === "ArrowDown" || e.key === "ArrowUp")) { e.preventDefault(); this.scrollByKey(e.key === "ArrowDown" ? 56 : -56, true); return; }
    const step = { ArrowRight: 1, ArrowLeft: -1, PageDown: 1, PageUp: -1, " ": e.shiftKey ? -1 : 1 }[e.key];
    if (paged && step && plain) { e.preventDefault(); this.goTo(this.page + step, false); return; }
    if (!paged && plain) {
      // A screen at a time, keeping a strip of the one before in view.
      const screen = Math.max(40, this.scroller.clientHeight - 48);
      const by = { PageDown: screen, PageUp: -screen, " ": e.shiftKey ? -screen : screen }[e.key];
      if (by) { e.preventDefault(); this.scrollByKey(by, false); return; }
    }
    if (!paged && (e.key === "ArrowRight" || e.key === "ArrowLeft")) { e.preventDefault(); this.goTo(this.page + (e.key === "ArrowRight" ? 1 : -1), true); return; }
    if (e.key === "Home") { e.preventDefault(); this.goTo(1, false); }
    if (e.key === "End") { e.preventDefault(); this.goTo(this.book.pages, false); }
    if (e.key === "Escape") { $("#rd-pop").hidden = true; $("#rd-menu").hidden = true; $("#rd-light-menu").hidden = true; }
    // N turns the night light on and off.
    if (e.key.toLowerCase() === "n" && plain) { e.preventDefault(); this.toggleLight?.(); }
  }

  /* ----- translating from the reader ----- */

  rangeFrom(p, n) {
    const out = [];
    for (let i = p; i <= this.book.pages && out.length < n; i++) if (!this.translated.has(i)) out.push(i);
    return out;
  }

  /** Queue `pages` for translation into `lang`. With `again`, those of them translated already are
      translated anew; their earlier translation stays in the file until the new one is done. */
  async enqueue(lang, pages, again = false) {
    if (!pages.length) { toast(RS.nothing); return; }
    // Scanned pages want the models that read them; without them, the pages with text go on.
    pages = await pagesToQueue(this.book.id, pages, null);
    if (!pages) return;
    await call("job_create", again ? { bookId: this.book.id, lang, pages, again: true } : { bookId: this.book.id, lang, pages });
    $("#rd-pop").hidden = true;
    toast(RS.queued);
    await refreshCounts();
    this.updateLive();
  }

  async togglePop() {
    const pop = $("#rd-pop");
    if (!pop.hidden) { pop.hidden = true; return; }
    const v = this.view;
    const lang = v.lang !== "orig" ? v.lang : state.settings.target_lang || (UI_LANG === "zh" ? "zh-hans" : UI_LANG);
    const done = new Set((await invoke("book_pages", { id: this.book.id, lang }).catch(() => [])).map(([p]) => p));
    const missing = Array.from({ length: this.book.pages }, (_, i) => i + 1).filter((p) => !done.has(p));
    const p = this.page;
    const ten = [];
    for (let i = p; i <= this.book.pages && ten.length < 10; i++) if (!done.has(i)) ten.push(i);
    // The ten pages from here, translated or not, for translating again those that are.
    const here = [];
    for (let i = p; i <= this.book.pages && here.length < 10; i++) here.push(i);
    // [what, label, pages, translated anew]
    const options = [
      done.has(p) ? ["pageAgain", RS.thisPageAgain(p), [p], true] : ["page", RS.thisPage(p), [p], false],
      ["ten", RS.nextTen(ten[0] || p, ten[ten.length - 1] || p), ten, false],
      ...(here.some((q) => done.has(q)) ? [["tenAgain", RS.nextTenAgain(here[0], here[here.length - 1]), here, true]] : []),
      ["all", RS.allMissing(missing.length), missing, false],
    ];
    // Ticked at first: the next ten pages, else what else there is to do; never work that was done already.
    const first = options.find((o) => o[0] === "ten" && o[2].length) || options.find((o) => !o[3] && o[2].length);
    pop.innerHTML = `<h3>${RS.popTitle}</h3>
      <label>${RS.popLang}<input id="pop-lang" list="languages" value="${esc(lang)}"></label>
      <div class="rd-options">${options.map((o) => `
        <label class="rd-option ${o[2].length ? "" : "off"}"><input type="radio" name="pop-range" value="${o[0]}" ${o === first ? "checked" : ""} ${o[2].length ? "" : "disabled"}>${esc(o[1])}</label>`).join("")}</div>
      <div class="rd-pop-actions"><button class="text-button" id="pop-more">${RS.moreOptions}</button><button class="primary-button" id="pop-go">${RS.start}</button></div>`;
    pop.hidden = false;
    $("#pop-more").onclick = () => go("book", { id: this.book.id });
    $("#pop-go").onclick = async () => {
      const chosenLang = $("#pop-lang").value.trim() || lang;
      const k = $("input[name=pop-range]:checked", pop)?.value;
      const option = options.find((o) => o[0] === k);
      let pages = option?.[2] || [];
      if (chosenLang !== lang) {
        // Another language: work the range out again for it. Pages asked for again stay the same pages.
        const d = new Set((await invoke("book_pages", { id: this.book.id, lang: chosenLang }).catch(() => [])).map(([q]) => q));
        const all = Array.from({ length: this.book.pages }, (_, i) => i + 1).filter((q) => !d.has(q));
        if (k === "page") pages = d.has(p) ? [] : [p];
        else if (k === "ten") pages = all.filter((q) => q >= p).slice(0, 10);
        else if (k === "all") pages = all;
      }
      this.enqueue(chosenLang, pages, !!option?.[3]);
    };
  }

  /** The "translating 3/10" chip, from the book's running or queued job. */
  updateLive(progress) {
    const chip = $("#rd-live");
    if (!chip) return;
    const job = state.jobs.find((j) => j.book_id === this.book.id && (j.status === "running" || j.status === "queued"));
    if (!job) { chip.hidden = true; return; }
    const live = progress && progress.job_id === job.id ? progress : state.live.get(job.id);
    chip.hidden = false;
    chip.classList.toggle("running", job.status === "running");
    chip.textContent = job.status === "running"
      ? `${langName(job.target_lang)} · ${RS.liveRunning(live ? live.done : job.done_pages, job.total_pages)}`
      : `${langName(job.target_lang)} · ${RS.liveQueued}`;
  }

  /** Note the pages a job of the language shown reports done: a page translated anew was translated before
      too, so only this tells that its translation changed. A report is of a page done when the job's count
      of done pages went up with it. */
  noteProgress(progress) {
    const last = this.lastDone.get(progress.job_id);
    this.lastDone.set(progress.job_id, progress.done);
    const job = state.jobs.find((j) => j.id === progress.job_id);
    if (job && job.target_lang !== this.view.lang) return;
    if (last != null && progress.done > last && progress.page) this.finished.add(progress.page);
  }

  /**
   * The translated file was written again while its job goes on, every few seconds. It is read again only
   * when that changes what is in view: at once when a page near the screen got its translation (or a new
   * one), when such a page comes into view later, and else half a minute after the file was last read; and
   * at once when the job ends (see reloadTranslation). The pages are drawn again where they stand, so the
   * place and the zoom stay as they are.
   */
  translationWritten(lang) {
    if (!this.alive) return;
    const path = this.files.outputs[lang];
    // A first translation into a language: the reader is built again with it (see reloadTranslation).
    if (!path) { readerFilesChanged(); return; }
    // A language not shown: only the copy of its file read before is let go.
    if (this.view.lang !== lang) { forgetPdf(path); return; }
    clearTimeout(this.writtenTimer);
    this.writtenTimer = setTimeout(() => this.checkWritten(), 300);
  }

  /** Work out which pages the last write changed, and whether that is worth reading the file again now. */
  async checkWritten() {
    const now = await invoke("book_pages", { id: this.book.id, lang: this.view.lang }).catch(() => null);
    if (!this.alive) return;
    if (now) {
      const fresh = new Set(now.map(([p]) => p));
      for (const p of fresh) if (!this.translated.has(p)) this.stale.add(p);
      for (const p of this.translated) if (!fresh.has(p)) this.stale.add(p);
    }
    for (const p of this.finished) this.stale.add(p);
    this.finished.clear();
    if ([...this.stale].some((p) => this.drawnRows.has(p))) this.refreshSoon();
    else if (this.stale.size || !now) this.refreshLate();
  }

  refreshSoon() {
    clearTimeout(this.soonTimer);
    this.soonTimer = setTimeout(() => this.refreshTranslation(), 300);
  }

  /** Read the file again half a minute after it was last read, if nothing calls for it sooner. */
  refreshLate() {
    if (this.lateTimer) return;
    const wait = Math.max(0, 25000 - (Date.now() - this.lastRead));
    this.lateTimer = setTimeout(() => { this.lateTimer = null; this.refreshTranslation(); }, wait);
  }

  /** Put the "not translated yet" note on the pages without a translation, and take it off the others. */
  syncBadges() {
    this.rows.forEach((row, i) => {
      const want = this.view.lang !== "orig" && !this.translated.has(i + 1);
      const has = row.el.querySelector(".rd-untranslated");
      if (want && !has) row.slots[row.slots.length - 1].el.appendChild(this.badge(i + 1));
      else if (!want && has) has.remove();
    });
  }

  /**
   * Read the translated file shown again, and draw the pages near the screen again from it where they stand;
   * pages further away are drawn from it when they come near. Only the translated file is read: the
   * original has not changed.
   */
  async refreshTranslation() {
    if (!this.alive) return;
    if (this.refreshing) { this.refreshAgain = true; return; }
    const path = this.view.lang !== "orig" ? this.files.outputs[this.view.lang] : null;
    if (!path) return;
    this.refreshing = true;
    clearTimeout(this.soonTimer);
    clearTimeout(this.lateTimer);
    this.lateTimer = null;
    this.stale.clear();
    this.lastRead = Date.now();
    try {
      const old = openDocs.get(path);
      openDocs.delete(path);
      const doc = await openPdf(path).catch(() => null);
      if (!doc) {
        // The file could not be read just now: the pages keep the copy they have.
        if (old) openDocs.set(path, old);
        return;
      }
      if (!this.alive) return;
      const source = openDocs.get(this.files.source);
      if (source) source.stamp = filesStamp;
      const docs = [...this.docs];
      docs[docs.length - 1] = doc;
      this.docs = docs;
      this.texts = null;
      await this.loadTranslated();
      if (!this.alive) return;
      this.syncBadges();
      // A slot drawn from the old copy is drawn again from the new one by draw(), the picture replaced only
      // once the new one is ready.
      for (const p of [...this.drawnRows]) this.draw(p);
      old?.doc.then((d) => d.destroy()).catch(() => {});
    } finally {
      this.refreshing = false;
      if (this.refreshAgain) {
        this.refreshAgain = false;
        this.refreshSoon();
      }
    }
  }

  /** The book's files changed on disk: a job ended, a translation came or was removed, a picture was put in.
      The translation shown is read again in place; when a language came or went, the reader is built again
      on the same spot. */
  async reloadTranslation() {
    if (!this.alive) return;
    const files = await invoke("book_files", { id: this.book.id }).catch(() => null);
    if (!files || !this.alive) return;
    const newLangs = Object.keys(files.outputs).filter((l) => !this.files.outputs[l]);
    const gone = Object.keys(this.files.outputs).filter((l) => !files.outputs[l]);
    if (newLangs.length || gone.length) {
      let lang = this.view.lang;
      if (newLangs.length) {
        toast(RS.ready(langName(newLangs[0])));
        // A first translation is shown to someone who never chose what to read in this book; someone who
        // chose the original keeps reading it.
        if (lang === "orig" && !readingPrefs(this.book.id).chose) lang = newLangs[0];
      }
      if (lang !== "orig" && !files.outputs[lang]) lang = "orig";
      keptPlace = { id: this.book.id, place: this.place(), at: Date.now() };
      go("reader", { id: this.book.id, lang, page: this.page });
      return;
    }
    this.files = files;
    await this.refreshTranslation();
  }
}

/** Let go of the copy of a file read before, so that it is read again when it is next opened. */
function forgetPdf(path) {
  const cached = openDocs.get(path);
  if (!cached) return;
  openDocs.delete(path);
  cached.doc.then((d) => d.destroy()).catch(() => {});
}

listen("job-progress", ({ payload: p }) => {
  if (state.route.view === "reader" && reader && p.book_id === reader.book.id) {
    reader.noteProgress(p);
    reader.updateLive(p);
  }
});
listen("book-written", ({ payload: [bookId, lang] }) => {
  if (state.route.view === "reader" && reader && bookId === reader.book.id) reader.translationWritten(lang);
});
listen("job-updated", ({ payload: job }) => {
  if (state.route.view === "reader" && reader && job.book_id === reader.book.id) {
    reader.updateLive();
    // A job that ended has written its last pages: they are shown at once.
    if (job.status === "done" || job.status === "cancelled" || job.status === "failed") readerFilesChanged();
  }
});
