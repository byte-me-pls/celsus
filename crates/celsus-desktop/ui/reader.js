/* The reader: pages drawn by pdf.js into our own view. Pages are drawn only when they come near the screen and
   are let go again when far away, so a 900-page book scrolls as smoothly as a short one. */

const RS = uiStrings("reader");

/* ---------- pdf.js ---------- */

let pdfjsLib = null;
async function pdfjs() {
  if (!pdfjsLib) {
    pdfjsLib = await import("./pdfjs/pdf.min.js");
    pdfjsLib.GlobalWorkerOptions.workerSrc = "pdfjs/pdf.worker.min.js";
  }
  return pdfjsLib;
}

/** Open documents by file path; a stamp forces a fresh load after the file was rewritten. */
const openDocs = new Map(); // path -> { stamp, doc }
let filesStamp = 0;
async function openPdf(path) {
  const cached = openDocs.get(path);
  if (cached && cached.stamp === filesStamp) return cached.doc;
  if (cached) cached.doc.then((d) => d.destroy()).catch(() => {});
  const doc = (async () => {
    const lib = await pdfjs();
    const bytes = await invoke("read_file", { path });
    return lib.getDocument({
      data: new Uint8Array(bytes),
      cMapUrl: "pdfjs/cmaps/",
      cMapPacked: true,
      standardFontDataUrl: "pdfjs/standard_fonts/",
      isEvalSupported: false,
    }).promise;
  })();
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
};

async function renderReader(r) {
  reader?.destroy();
  reader = null;
  const book = await call("book_get", { id: r.id });
  setCrumbs(T.navLibrary, book.title, T.crumbReader);
  const files = await call("book_files", { id: r.id });
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
        <label class="rd-pageno"><input id="rd-page" type="number" min="1" max="${book.pages}" value="1"><span>${RS.pageOf(book.pages)}</span></label>
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
      <div class="rd-scroll" id="rd-scroll" tabindex="0"><div class="rd-pages" id="rd-pages"></div></div>
      <button class="rd-flip prev" id="rd-prev" title="${RS.prev}" hidden>${ICON.left}</button>
      <button class="rd-flip next" id="rd-next" title="${RS.next}" hidden>${ICON.right}</button>
      <div class="rd-pop" id="rd-pop" hidden></div>
    </div>`;
  reader = new Reader(book, files, view);
  await reader.start(Math.max(1, Math.min(book.pages, r.page || 1)));
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
    this.alive = true;
    this.onKey = (e) => this.key(e);
    this.onResize = () => this.layout(true);
    document.addEventListener("keydown", this.onKey);
    window.addEventListener("resize", this.onResize);
  }

  destroy() {
    this.alive = false;
    this.observer?.disconnect();
    document.removeEventListener("keydown", this.onKey);
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
    await this.loadTranslated();
    this.build();
    this.goTo(page, false);
    this.updateLive();
  }

  async loadTranslated() {
    this.translated = new Set();
    if (this.view.lang === "orig") return;
    const pages = await invoke("book_pages", { id: this.book.id, lang: this.view.lang }).catch(() => []);
    this.translated = new Set(pages.map(([p]) => p));
  }

  bindBar() {
    const v = this.view;
    $("#rd-back").onclick = () => go("library");
    for (const b of $$("#rd-lang button")) {
      b.setAttribute("aria-pressed", String(b.dataset.lang === v.lang));
      b.onclick = () => go("reader", { id: this.book.id, lang: b.dataset.lang, page: this.page });
    }
    const split = $("#rd-split");
    split.setAttribute("aria-pressed", String(v.split));
    split.onclick = () => {
      saveViewPrefs({ split: !v.split });
      go("reader", { id: this.book.id, lang: v.lang === "orig" ? Object.keys(this.files.outputs)[0] : v.lang, page: this.page });
    };
    for (const b of $$("#rd-mode button")) {
      b.setAttribute("aria-pressed", String(b.dataset.mode === v.mode));
      b.onclick = () => {
        if (v.mode === b.dataset.mode) return;
        v.mode = b.dataset.mode;
        saveViewPrefs({ mode: v.mode });
        for (const x of $$("#rd-mode button")) x.setAttribute("aria-pressed", String(x === b));
        this.layout(true);
      };
    }
    $("#rd-zoom-in").onclick = () => this.setZoom(v.zoom * 1.15);
    $("#rd-zoom-out").onclick = () => this.setZoom(v.zoom / 1.15);
    $("#rd-zoom-val").onclick = () => this.setZoom(1);
    $("#rd-page").onchange = (e) => this.goTo(Number(e.target.value), true);
    $("#rd-prev").onclick = () => this.goTo(this.page - 1, true);
    $("#rd-next").onclick = () => this.goTo(this.page + 1, true);
    $("#rd-translate").onclick = (e) => { e.stopPropagation(); this.togglePop(); };
    $("#rd-more").onclick = (e) => { e.stopPropagation(); $("#rd-menu").hidden = !$("#rd-menu").hidden; };
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
    };
    this.scroller.addEventListener("scroll", () => this.onScroll(), { passive: true });
    this.scroller.addEventListener("wheel", (e) => {
      if (e.ctrlKey) {
        e.preventDefault();
        this.setZoom(v.zoom * (e.deltaY < 0 ? 1.1 : 1 / 1.1));
      } else if (v.mode === "paged" && Math.abs(e.deltaY) > 30 && !this.wheelLock) {
        // In page mode a wheel step turns the page.
        this.wheelLock = true;
        setTimeout(() => (this.wheelLock = false), 250);
        this.goTo(this.page + (e.deltaY > 0 ? 1 : -1), true);
      }
    }, { passive: false });
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
        return { el: s, canvas: null, task: null, drawn: 0 };
      });
      if (this.view.lang !== "orig" && !this.translated.has(p)) {
        const badge = document.createElement("div");
        badge.className = "rd-untranslated";
        badge.innerHTML = `<span>${RS.notTranslated}</span><button class="text-button" data-from="${p}">${RS.fromHere}</button>`;
        slots[slots.length - 1].el.appendChild(badge);
      }
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

  layout(keepPlace) {
    if (!this.rows.length) return;
    const v = this.view;
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
        // Redraw at the new size when the page is next seen.
        if (s.drawn && Math.abs(s.drawn - width) > 1) s.stale = true;
      }
      row.el.hidden = v.mode === "paged" && p !== page;
    });
    if (keepPlace) this.goTo(page, false);
    for (const p of [...this.drawnRows]) if (this.rows[p - 1].slots.some((s) => s.stale)) this.draw(p);
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
    // Learn the page's real size first; books mix page sizes now and then.
    if (!this.sizes.has(p)) {
      const pg = await this.docs[0].getPage(p);
      const vp = pg.getViewport({ scale: 1 });
      this.sizes.set(p, [vp.width, vp.height]);
      if (Math.abs(vp.width / vp.height - this.defaultSize[0] / this.defaultSize[1]) > 0.01) this.layout(false);
    }
    await Promise.all(row.slots.map((s, i) => this.drawSlot(s, this.docs[i], p)));
  }

  async drawSlot(slot, doc, p) {
    const width = this.pageWidth(p);
    if (slot.canvas && !slot.stale && Math.abs(slot.drawn - width) <= 1) return;
    slot.task?.cancel();
    const page = await doc.getPage(Math.min(p, doc.numPages));
    if (!this.alive) return;
    const ratio = window.devicePixelRatio || 1;
    const vp = page.getViewport({ scale: (width / page.getViewport({ scale: 1 }).width) * ratio });
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
    slot.stale = false;
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
    }
  }

  /* ----- moving around ----- */

  goTo(p, smooth) {
    p = Math.max(1, Math.min(this.book.pages, p || 1));
    const changed = p !== this.page;
    this.page = p;
    $("#rd-page").value = p;
    saveReadingPrefs(this.book.id, { page: p });
    if (this.view.mode === "paged") {
      if (changed || !this.rows[p - 1].el.offsetParent) {
        this.rows.forEach((row, i) => (row.el.hidden = i + 1 !== p));
      }
      this.scroller.scrollTop = 0;
      this.draw(p);
      // Draw the next page ahead of time so turning is instant.
      if (p < this.book.pages) setTimeout(() => this.drawAhead(p + 1), 60);
      return;
    }
    const top = this.rows[p - 1].el.offsetTop - 16;
    this.scroller.scrollTo({ top, behavior: smooth ? "smooth" : "auto" });
  }

  async drawAhead(p) {
    const row = this.rows[p - 1];
    if (!row) return;
    // A hidden row has no size; draw it with the current page's size.
    this.drawnRows.add(p);
    await Promise.all(row.slots.map((s, i) => this.drawSlot(s, this.docs[i], p)));
  }

  onScroll() {
    if (this.view.mode === "paged" || this.scrollFrame) return;
    this.scrollFrame = requestAnimationFrame(() => {
      this.scrollFrame = null;
      const mid = this.scroller.scrollTop + this.scroller.clientHeight * 0.35;
      // Binary search for the row under the reading line.
      let lo = 0, hi = this.rows.length - 1;
      while (lo < hi) {
        const m = (lo + hi + 1) >> 1;
        if (this.rows[m].el.offsetTop <= mid) lo = m; else hi = m - 1;
      }
      const p = lo + 1;
      if (p !== this.page) {
        this.page = p;
        $("#rd-page").value = p;
        clearTimeout(this.saveTimer);
        this.saveTimer = setTimeout(() => saveReadingPrefs(this.book.id, { page: p }), 300);
      }
    });
  }

  key(e) {
    if (state.route.view !== "reader" || e.target.matches?.("input, textarea, select")) return;
    const paged = this.view.mode === "paged";
    if ((e.ctrlKey || e.metaKey) && (e.key === "+" || e.key === "=")) { e.preventDefault(); this.setZoom(this.view.zoom * 1.15); return; }
    if ((e.ctrlKey || e.metaKey) && e.key === "-") { e.preventDefault(); this.setZoom(this.view.zoom / 1.15); return; }
    if ((e.ctrlKey || e.metaKey) && e.key === "0") { e.preventDefault(); this.setZoom(1); return; }
    const step = { ArrowRight: 1, ArrowLeft: -1, PageDown: 1, PageUp: -1, " ": e.shiftKey ? -1 : 1 }[e.key];
    if (paged && step) { e.preventDefault(); this.goTo(this.page + step, false); return; }
    if (!paged && (e.key === "ArrowRight" || e.key === "ArrowLeft")) { e.preventDefault(); this.goTo(this.page + (e.key === "ArrowRight" ? 1 : -1), true); return; }
    if (e.key === "Home") { e.preventDefault(); this.goTo(1, false); }
    if (e.key === "End") { e.preventDefault(); this.goTo(this.book.pages, false); }
    if (e.key === "Escape") { $("#rd-pop").hidden = true; $("#rd-menu").hidden = true; }
  }

  /* ----- translating from the reader ----- */

  rangeFrom(p, n) {
    const out = [];
    for (let i = p; i <= this.book.pages && out.length < n; i++) if (!this.translated.has(i)) out.push(i);
    return out;
  }

  async enqueue(lang, pages) {
    if (!pages.length) { toast(RS.nothing); return; }
    await call("job_create", { bookId: this.book.id, lang, pages });
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
    const options = [
      ["page", RS.thisPage(p), done.has(p) ? [] : [p]],
      ["ten", RS.nextTen(ten[0] || p, ten[ten.length - 1] || p), ten],
      ["all", RS.allMissing(missing.length), missing],
    ];
    pop.innerHTML = `<h3>${RS.popTitle}</h3>
      <label>${RS.popLang}<input id="pop-lang" list="languages" value="${esc(lang)}"></label>
      <div class="rd-options">${options.map(([k, label, list], i) => `
        <label class="rd-option ${list.length ? "" : "off"}"><input type="radio" name="pop-range" value="${k}" ${i === 1 && list.length ? "checked" : ""} ${list.length ? "" : "disabled"}>${esc(label)}</label>`).join("")}</div>
      <div class="rd-pop-actions"><button class="text-button" id="pop-more">${RS.moreOptions}</button><button class="primary-button" id="pop-go">${RS.start}</button></div>`;
    if (!$("input[name=pop-range]:checked", pop)) $("input[name=pop-range]:not(:disabled)", pop)?.setAttribute("checked", "");
    pop.hidden = false;
    $("#pop-more").onclick = () => go("book", { id: this.book.id });
    $("#pop-go").onclick = async () => {
      const chosenLang = $("#pop-lang").value.trim() || lang;
      const k = $("input[name=pop-range]:checked", pop)?.value;
      let pages = options.find((o) => o[0] === k)?.[2] || [];
      if (chosenLang !== lang) {
        // Another language: work the range out again for it.
        const d = new Set((await invoke("book_pages", { id: this.book.id, lang: chosenLang }).catch(() => [])).map(([q]) => q));
        const all = Array.from({ length: this.book.pages }, (_, i) => i + 1).filter((q) => !d.has(q));
        pages = k === "page" ? (d.has(p) ? [] : [p]) : k === "ten" ? all.filter((q) => q >= p).slice(0, 10) : all;
      }
      this.enqueue(chosenLang, pages);
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

  /** The translated file was rewritten: load it again and stay on the same page. */
  async reloadTranslation() {
    if (!this.alive) return;
    const files = await invoke("book_files", { id: this.book.id });
    const newLangs = Object.keys(files.outputs).filter((l) => !this.files.outputs[l]);
    if (newLangs.length) {
      // A first translation arrived: show it.
      toast(RS.ready(langName(newLangs[0])));
      go("reader", { id: this.book.id, lang: this.view.lang === "orig" ? newLangs[0] : this.view.lang, page: this.page });
      return;
    }
    if (this.view.lang === "orig") return;
    this.files = files;
    const page = this.page;
    for (const row of this.rows) for (const s of row.slots) s.task?.cancel();
    this.docs = await Promise.all(this.sources().map(openPdf));
    await this.loadTranslated();
    this.build();
    this.goTo(page, false);
    this.updateLive();
  }
}

listen("job-progress", ({ payload: p }) => {
  if (state.route.view === "reader" && reader && p.book_id === reader.book.id) reader.updateLive(p);
});
listen("job-updated", ({ payload: job }) => {
  if (state.route.view === "reader" && reader && job.book_id === reader.book.id) {
    reader.updateLive();
    if (job.status === "done" || job.status === "cancelled") readerFilesChanged();
  }
});
