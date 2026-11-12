const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

/* ============================== strings ============================== */

const T = uiStrings("app");
// A new library translates into the language the interface is shown in.
const DEFAULT_TARGET = UI_LANG === "zh" ? "zh-hans" : UI_LANG;

// The full list (about 130 languages) comes from the backend; these are shown until it arrives.
let LANGUAGES = [
  ["tr", "Türkçe"], ["en", "English"], ["de", "Deutsch"], ["fr", "Français"], ["es", "Español"], ["ru", "Русский"],
  ["ar", "العربية"], ["zh-hans", "简体中文"], ["ja", "日本語"], ["ko", "한국어"], ["hi", "हिन्दी"],
];
const langName = (code) => (LANGUAGES.find(([c]) => c === String(code).toLowerCase()) || [code, String(code).toUpperCase()])[1];
const PRESETS = [
  ["OpenAI", "https://api.openai.com/v1"],
  ["NVIDIA", "https://integrate.api.nvidia.com/v1"],
  ["OpenRouter", "https://openrouter.ai/api/v1"],
  ["Ollama", "http://localhost:11434/v1"],
  ["LM Studio", "http://localhost:1234/v1"],
];

/** How to set up each kind of connection. `link` opens in the browser; `local` servers need no key. */
const GUIDES = uiStrings("guides");

const presetOf = (url) => (PRESETS.find(([, u]) => u === url) || ["custom"])[0];

/* ============================== helpers ============================== */

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const main = $("#main");
const fmtBytes = (n) => (n > 1e9 ? `${(n / 1e9).toFixed(1)} GB` : n > 1e6 ? `${(n / 1e6).toFixed(1)} MB` : `${Math.round(n / 1e3)} KB`);
const fmtDate = (s) => (s ? new Date(s * 1000).toLocaleString(UI_LANG, { dateStyle: "medium", timeStyle: "short" }) : "");
const num = (n) => Number(n || 0).toLocaleString(UI_LANG);

function toast(message, error = false) {
  const t = $("#toast");
  t.textContent = message;
  t.className = error ? "toast error" : "toast";
  t.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (t.hidden = true), error ? 7000 : 3200);
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    // Some failures come as a code, shown in the interface language.
    toast(T.errors?.[String(e)] || String(e), true);
    throw e;
  }
}

async function loadSettings() {
  const s = await invoke("settings_get");
  if (!s.target_lang) s.target_lang = DEFAULT_TARGET;
  return s;
}

const state = {
  route: { view: "library" },
  books: [],
  jobs: [],
  settings: {},
  live: new Map(), // job id -> latest progress
  stopping: new Set(), // running jobs the user has asked to stop
  covers: new Map(), // book id -> object URL
  filter: "all",
  query: "",
};

/** The open reader (see reader.js), if any. */
let reader = null;

async function coverUrl(book) {
  if (state.covers.has(book.id)) return state.covers.get(book.id);
  if (!book.has_cover) return null;
  const files = await invoke("book_files", { id: book.id });
  if (!files.cover) return null;
  const bytes = await invoke("read_file", { path: files.cover });
  const url = URL.createObjectURL(new Blob([bytes], { type: "image/jpeg" }));
  state.covers.set(book.id, url);
  return url;
}

function coverHtml(book) {
  return `<div class="made-cover" data-cover="${book.id}"><b>${esc(book.title)}</b><small>${esc(book.author || "")}</small></div>`;
}

async function fillCovers(root = main) {
  for (const el of $$("[data-cover]", root)) {
    const book = state.books.find((b) => b.id === Number(el.dataset.cover));
    if (!book) continue;
    if (!book.has_cover && !state.covers.has(book.id)) {
      wantCover(book);
      continue;
    }
    const url = await coverUrl(book).catch(() => null);
    if (url && el.isConnected) {
      const img = document.createElement("img");
      img.src = url;
      img.alt = "";
      el.replaceWith(img);
    }
  }
}

/** A book without a cover gets its first page as one. The picture is kept, so this happens once for a book. */
function wantCover(book) {
  const source = async () => (await invoke("book_files", { id: book.id })).source;
  makeCover(`${book.id}:${book.bytes}`, source, async (blob) => {
    await invoke("cover_save", { id: book.id, jpeg: Array.from(new Uint8Array(await blob.arrayBuffer())) });
    state.covers.set(book.id, URL.createObjectURL(blob));
    const listed = state.books.find((b) => b.id === book.id);
    if (listed) listed.has_cover = true;
    fillCovers();
  });
}

/* ============================== shell ============================== */

function applyStrings(root = document) {
  for (const el of $$("[data-i18n]", root)) el.textContent = T[el.dataset.i18n];
  for (const el of $$("[data-i18n-title]", root)) el.title = el.ariaLabel = T[el.dataset.i18nTitle];
}

/* ---------- window frame ---------- */
const appWindow = window.__TAURI__.window?.getCurrentWindow?.();
async function syncMaximized() {
  if (appWindow) document.documentElement.classList.toggle("maximized", await appWindow.isMaximized().catch(() => false));
}
if (appWindow) {
  $("#win-min").onclick = () => appWindow.minimize();
  $("#win-max").onclick = () => appWindow.toggleMaximize();
  $("#win-close").onclick = () => appWindow.close();
  appWindow.onResized(syncMaximized);
  syncMaximized();
}

/** The title bar's location trail: section, then book, then reader. */
function setCrumbs(...parts) {
  $("#crumbs").innerHTML = parts
    .filter(Boolean)
    .map((p, i, all) => (i === all.length - 1 ? `<b>${esc(p)}</b>` : `<span>${esc(p)}</span><i>/</i>`))
    .join("");
}

function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  try { localStorage.setItem("celsus-theme", theme); } catch {}
  for (const b of $$("[data-theme-choice]")) b.setAttribute("aria-pressed", String(b.dataset.themeChoice === theme));
}

async function refreshCounts() {
  state.books = await invoke("library_list");
  state.jobs = await invoke("jobs_list", { bookId: null });
  $("#count-books").textContent = state.books.length || "";
  const active = state.jobs.filter((j) => j.status === "queued" || j.status === "running").length;
  $("#count-jobs").textContent = active || "";
  $("#count-jobs").classList.toggle("live", active > 0);
}

async function refreshProfile() {
  const tasks = await invoke("task_models");
  const model = tasks.find((t) => t.task === "translate")?.model;
  const name = state.settings.profile_name || T.profileDefault;
  $("#profile-name").textContent = name;
  $("#avatar").textContent = initials(name);
  $("#profile-model").textContent = model ? model.split("/").pop() : T.noModel;
}

const initials = (name) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0].toUpperCase()).join("") || "C";

function go(view, params = {}) {
  state.route = { view, ...params };
  for (const b of $$("[data-go]")) b.classList.toggle("active", b.dataset.go === view || (view === "book" && b.dataset.go === "library") || (view === "reader" && b.dataset.go === "library"));
  if (view !== "reader" && reader) { reader.destroy(); reader = null; }
  main.className = view === "reader" ? "reader-host" : "";
  main.scrollTop = 0;
  const section = { library: T.navLibrary, book: T.navLibrary, reader: T.navLibrary, jobs: T.navJobs, settings: T.navSettings, profile: T.profileTitle }[view];
  setCrumbs(section);
  render();
}

async function render() {
  const r = state.route;
  try {
    if (r.view === "library") await renderLibrary();
    else if (r.view === "book") await renderBook(r.id);
    else if (r.view === "jobs") await renderJobs();
    else if (r.view === "reader") await renderReader(r);
    else if (r.view === "settings") await renderSettings(r.tab || "general");
    else if (r.view === "profile") await renderProfile();
  } catch (e) {
    main.innerHTML = `<div class="empty-results"><h2>:(</h2><p>${esc(e)}</p></div>`;
  }
}

/* ============================== library ============================== */

function bookStatus(book) {
  const job = state.jobs.find((j) => j.book_id === book.id && (j.status === "running" || j.status === "queued"));
  if (job) {
    const live = state.live.get(job.id);
    const done = live ? live.done : job.done_pages;
    const pct = job.total_pages ? (100 * done) / job.total_pages : 0;
    const label = job.status === "running" ? T.translating : T.queued;
    return `<div class="book-status">${label} ${done}/${job.total_pages}<div class="meter"><span data-job-meter="${job.id}" style="width:${pct}%"></span></div></div>`;
  }
  const langs = Object.entries(book.translated);
  if (!langs.length) return `<div class="book-status">${T.notTranslated}</div>`;
  const [lang, n] = langs.sort((a, b) => b[1] - a[1])[0];
  return `<div class="book-status">${T.translatedOf(lang, n, book.pages)}<div class="meter"><span style="width:${(100 * n) / book.pages}%"></span></div></div>`;
}

function filteredBooks() {
  const q = state.query.trim().toLowerCase();
  return state.books.filter((b) => {
    if (q && !`${b.title} ${b.author}`.toLowerCase().includes(q)) return false;
    const translated = Object.keys(b.translated).length > 0;
    if (state.filter === "translated") return translated;
    if (state.filter === "busy") return b.busy;
    if (state.filter === "untranslated") return !translated;
    return true;
  });
}

async function renderLibrary() {
  await refreshCounts();
  if (!state.books.length) {
    main.innerHTML = `
      <div class="page-heading"><h1>${T.libraryTitle}</h1></div>
      <div class="empty-results"><span class="hero-logo"></span><h2>${T.emptyTitle}</h2><p>${T.emptyText}</p>
      <button class="primary-button" id="add-books">${T.addBooks}</button></div>`;
    $("#add-books").onclick = addBooks;
    return;
  }
  const filters = [["all", T.fAll], ["translated", T.fTranslated], ["busy", T.fBusy], ["untranslated", T.fUntranslated]];
  main.innerHTML = `
    <div class="page-heading">
      <h1>${T.libraryTitle}<span class="count">${state.books.length}</span></h1>
      <div class="heading-actions"><button class="primary-button" id="add-books">
        <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>${T.addBooks}</button></div>
    </div>
    <div class="toolbar">
      <label class="search-field"><svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/></svg>
        <input id="search" type="search" placeholder="${T.search}" value="${esc(state.query)}"></label>
      <span class="result-count" id="result-count"></span>
    </div>
    <div class="filters">${filters.map(([k, l]) => `<button data-filter="${k}" class="${state.filter === k ? "selected" : ""}">${l}</button>`).join("")}</div>
    <div class="book-grid" id="grid"></div>`;
  $("#add-books").onclick = addBooks;
  $("#search").oninput = (e) => { state.query = e.target.value; drawGrid(); };
  for (const b of $$("[data-filter]")) b.onclick = () => { state.filter = b.dataset.filter; for (const x of $$("[data-filter]")) x.classList.toggle("selected", x === b); drawGrid(); };
  drawGrid();
}

function drawGrid() {
  const list = filteredBooks();
  $("#result-count").textContent = T.results(list.length);
  const grid = $("#grid");
  if (!list.length) {
    grid.outerHTML = `<div class="book-grid" id="grid"><p class="caption">${T.emptyFilter}</p></div>`;
    return;
  }
  grid.innerHTML = list.map((b) => {
    const langs = Object.keys(b.translated).map((l) => `<span class="language-badge">${esc(l)}</span>`).join("");
    return `<div class="book-card-wrap">
      <button class="book-card" data-book="${b.id}" title="${T.read}">
        <div class="book-cover">${coverHtml(b)}${b.busy ? `<span class="cover-badge">${T.translating}</span>` : ""}</div>
        <h3 class="book-title">${esc(b.title)}</h3>
        <p class="book-author">${esc(b.author)}</p>
        <div class="book-details"><span>${T.pagesN(b.pages)}</span>${langs}</div>
        ${bookStatus(b)}
      </button>
      <button class="card-action" data-translate-book="${b.id}">
        <svg viewBox="0 0 24 24"><path d="M4 5h9M8.5 3v2M6 5c.6 3.6 2.8 6.5 6 8"/><path d="M11 5c-.6 3.4-2.9 6.4-7 8.5"/><path d="M13 21l4-10 4 10M14.5 17.5h5"/></svg>${T.translateAction}</button>
    </div>`;
  }).join("");
  for (const c of $$("[data-book]", grid)) c.onclick = () => openReader(state.books.find((b) => b.id === Number(c.dataset.book)));
  for (const c of $$("[data-translate-book]", grid)) c.onclick = () => go("book", { id: Number(c.dataset.translateBook) });
  fillCovers(grid);
}

async function addBooks(paths) {
  const list = Array.isArray(paths) ? paths : await call("pick_pdfs");
  if (!list.length) return;
  const res = await call("books_add", { paths: list, uiLang: UI_LANG });
  if (res.added.length) toast(T.added(res.added.length));
  for (const f of res.failed) toast(`${T.addFailed}: ${f}`, true);
  if (res.added.length === 1 && list.length === 1) openReader(res.added[0]);
  else go("library");
}

/* ============================== book ============================== */

let bookForm = { mode: "range", first: 1, last: 5 };

async function renderBook(id) {
  await refreshCounts();
  const book = await call("book_get", { id });
  setCrumbs(T.navLibrary, book.title, T.translateAction);
  const target = bookForm.lang || state.settings.target_lang || DEFAULT_TARGET;
  bookForm = { ...bookForm, bookId: id, lang: bookForm.bookId === id ? bookForm.lang : target };
  if (bookForm.last > book.pages || bookForm.bookId !== id) bookForm.last = Math.min(book.pages, 5);
  main.innerHTML = `
    <button class="back-link" id="back"><svg viewBox="0 0 24 24"><path d="M15 18l-6-6 6-6"/></svg>${T.back}</button>
    <div class="book-head">
      <div class="thumb">${coverHtml(book)}</div>
      <div class="head-text"><h1><span class="title-text">${esc(book.title)}</span><span class="count">${T.pagesN(book.pages)}</span></h1>
        <p>${esc(book.author)}</p></div>
      <button class="primary-button big" id="b-read"><svg viewBox="0 0 24 24"><path d="M2 5.5C4.5 4 8 4 12 6.5 16 4 19.5 4 22 5.5V19c-2.5-1.5-6-1.5-10 1-4-2.5-7.5-2.5-10-1z"/><path d="M12 6.5V20"/></svg>${T.read}</button>
    </div>
    <div class="book-layout">
      <div>
        <section class="card stack">
          <div class="section-heading"><h2>${T.startTitle}</h2></div>
          <div class="row">
            <label>${T.sourceLang}<input id="b-source" list="languages" placeholder="${T.auto}" value="${esc(book.source_lang)}"></label>
            <span class="arrow">→</span>
            <label>${T.targetLang}<input id="b-target" list="languages" value="${esc(bookForm.lang)}"></label>
          </div>
          <div class="radio-row" id="b-mode">
            <button data-mode="range">${T.mRange}</button><button data-mode="missing">${T.mMissing}</button><button data-mode="all">${T.mAll}</button>
          </div>
          <div class="row" id="b-range">
            <label>${T.firstPage}<input id="b-first" type="number" min="1" max="${book.pages}" value="${bookForm.first}"></label>
            <label>${T.lastPage}<input id="b-last" type="number" min="1" max="${book.pages}" value="${bookForm.last}"></label>
          </div>
          <div class="estimate" id="b-estimate"><div><b>–</b><small>${T.estChars}</small></div><div><b>–</b><small>${T.estTokens}</small></div><div><b>–</b><small>${T.estTime}</small></div></div>
          <button class="primary-button wide" id="b-enqueue">${T.enqueue}</button>
        </section>
        <section class="card stack">
          <div class="section-heading"><h2>${T.detailsTitle}</h2>
            <button class="text-button spark" id="b-recognise"><svg viewBox="0 0 24 24"><path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z"/><path d="M19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8z"/></svg>${T.recognise}</button></div>
          <label>${T.bookTitle}<input id="b-title" value="${esc(book.title)}"></label>
          <label>${T.author}<input id="b-author" value="${esc(book.author)}"></label>
          <label>${T.subject}<textarea id="b-subject" rows="3">${esc(book.subject)}</textarea></label>
          <p class="hint">${T.subjectHint}</p>
          <div class="form-actions"><button class="quiet-button" id="b-save">${T.saveDetails}</button></div>
        </section>
      </div>
      <div>
        <section class="card" id="b-translations"></section>
        <section class="card stack" id="b-pictures">
          <div class="section-heading" style="margin:0"><h2>${T.picturesTitle}</h2><span class="caption" id="pic-lang"></span></div>
          <p class="hint">${T.picturesHint}</p>
          <div class="row" style="align-items:center">
            <label class="page-jump">${T.picPage}<input id="pic-page" type="number" min="1" max="${book.pages}" value="1" style="width:76px;padding:7px 10px"></label>
            <button class="quiet-button" id="pic-show">${T.picShow}</button>
            <input id="pic-note" placeholder="${T.picNotePh}" title="${T.picNote}" style="flex:1;padding:7px 10px">
          </div>
          <div id="pic-list"></div>
        </section>
        <section class="card" id="b-history"></section>
        <div class="danger-zone"><div><b>${T.deleteTitle}</b><p class="hint">${T.deleteText}</p></div>
          <button class="quiet-button danger" id="b-delete">${T.deleteButton}</button></div>
      </div>
    </div>`;
  $("#back").onclick = () => go("library");
  $("#b-read").onclick = () => openReader(book);
  fillCovers();
  const setMode = (m) => {
    bookForm.mode = m;
    for (const b of $$("#b-mode button")) b.setAttribute("aria-pressed", String(b.dataset.mode === m));
    $("#b-range").hidden = m !== "range";
    estimate(book);
  };
  for (const b of $$("#b-mode button")) b.onclick = () => setMode(b.dataset.mode);
  setMode(bookForm.mode);
  for (const id2 of ["#b-first", "#b-last"]) $(id2).oninput = () => { bookForm.first = Number($("#b-first").value); bookForm.last = Number($("#b-last").value); estimate(book); };
  $("#b-target").oninput = () => { bookForm.lang = $("#b-target").value.trim(); renderBookStatus(book); estimate(book); renderPictures(book); };
  $("#b-enqueue").onclick = () => enqueue(book);
  $("#b-save").onclick = async () => {
    await call("book_update", { id: book.id, title: $("#b-title").value, author: $("#b-author").value, subject: $("#b-subject").value, sourceLang: $("#b-source").value });
    toast(T.savedDetails);
    go("book", { id: book.id });
  };
  $("#b-recognise").onclick = async () => {
    const btn = $("#b-recognise");
    const label = btn.lastChild.textContent;
    btn.disabled = true;
    btn.lastChild.textContent = T.recognising;
    try {
      const info = await call("book_recognise", { id: book.id, uiLang: UI_LANG });
      const set = (sel, v) => { if (v) $(sel).value = v; };
      set("#b-title", info.title);
      set("#b-author", info.author);
      set("#b-subject", info.subject);
      set("#b-source", info.language);
      toast(info.recognised ? T.recognised : T.notRecognised);
    } finally {
      btn.disabled = false;
      btn.lastChild.textContent = label;
    }
  };
  $("#pic-show").onclick = () => renderPictures(book);
  $("#pic-page").onchange = () => renderPictures(book);
  renderPictures(book);
  $("#b-delete").onclick = async () => {
    if (!confirm(T.confirmDelete(book.title))) return;
    await call("book_delete", { id: book.id });
    state.covers.delete(book.id);
    toast(T.deleted);
    go("library");
  };
  await renderBookStatus(book);
}

async function chosenPages(book) {
  const lang = ($("#b-target")?.value.trim() || DEFAULT_TARGET).toLowerCase();
  if (bookForm.mode === "all") return Array.from({ length: book.pages }, (_, i) => i + 1);
  if (bookForm.mode === "missing") {
    const done = new Set((await invoke("book_pages", { id: book.id, lang })).map(([p]) => p));
    return Array.from({ length: book.pages }, (_, i) => i + 1).filter((p) => !done.has(p));
  }
  const a = Math.max(1, Math.min(book.pages, Number($("#b-first").value) || 1));
  const b = Math.max(a, Math.min(book.pages, Number($("#b-last").value) || a));
  return Array.from({ length: b - a + 1 }, (_, i) => a + i);
}

let estimateTimer;
function estimate(book) {
  clearTimeout(estimateTimer);
  estimateTimer = setTimeout(async () => {
    const pages = await chosenPages(book);
    const cells = $$("#b-estimate b");
    if (!cells.length) return;
    if (!pages.length) { cells.forEach((c) => (c.textContent = "0")); return; }
    const chars = await invoke("book_estimate", { id: book.id, pages: pages.slice(0, 2000) });
    // About 3.5 characters per token each way, plus the prompt and context sent with every request.
    const tokens = Math.round((chars / 3.5) * 2.2 + pages.length * 600);
    cells[0].textContent = num(chars);
    cells[1].textContent = num(tokens);
    cells[2].textContent = T.minutes(pages.length * 16 / 60);
  }, 250);
}

async function enqueue(book) {
  const pages = await chosenPages(book);
  if (!pages.length) { toast(T.nothingToDo); return; }
  const lang = ($("#b-target").value.trim() || DEFAULT_TARGET).toLowerCase();
  // One click, one job: the button waits until this one is in the queue.
  const button = $("#b-enqueue");
  if (button.disabled) return;
  button.disabled = true;
  try {
    if ($("#b-source").value.trim() !== book.source_lang) {
      await call("book_update", { id: book.id, title: book.title, author: book.author, subject: book.subject, sourceLang: $("#b-source").value });
    }
    await call("job_create", { bookId: book.id, lang, pages });
    toast(T.enqueued);
    await refreshCounts();
    renderBookStatus(book);
  } finally {
    button.disabled = false;
  }
}

async function renderBookStatus(book) {
  if (state.route.view !== "book" || state.route.id !== book.id) return;
  const fresh = await invoke("book_get", { id: book.id });
  const jobs = await invoke("jobs_list", { bookId: book.id });
  const langs = Object.keys(fresh.translated);
  const selected = langs.includes(bookForm.lang) ? bookForm.lang : langs[0];
  const box = $("#b-translations");
  if (!box) return;
  if (!langs.length) {
    box.innerHTML = `<div class="section-heading"><h2>${T.translationsTitle}</h2></div><p class="caption">${T.noTranslations}</p>`;
  } else {
    const rows = [];
    for (const lang of langs) {
      const pages = await invoke("book_pages", { id: book.id, lang });
      const issues = pages.filter(([, k]) => k > 0).length;
      rows.push(`<div class="lang-row">
        <span class="lang-name">${esc(langName(lang))}</span>
        <span class="lang-meta">${T.pagesDone(pages.length, fresh.pages)}${issues ? ` · ${T.issues(issues)}` : ""}
          <div class="meter"><span style="width:${(100 * pages.length) / fresh.pages}%"></span></div></span>
        <span class="lang-actions"><button class="quiet-button" data-read="${esc(lang)}">${T.read}</button>
          <button class="quiet-button" data-save="${esc(lang)}">${T.savePdf}</button></span></div>`);
    }
    const map = await invoke("book_pages", { id: book.id, lang: selected });
    const status = new Map(map.map(([p, k]) => [p, k > 0 ? "k" : "t"]));
    const running = jobs.find((j) => j.status === "running" && j.target_lang === selected);
    const livePage = running ? state.live.get(running.id)?.page : null;
    const cells = Array.from({ length: fresh.pages }, (_, i) => {
      const p = i + 1;
      const cls = p === livePage ? "r" : status.get(p) || "";
      return `<i class="${cls}" data-page="${p}" title="${p}"></i>`;
    }).join("");
    box.innerHTML = `<div class="section-heading"><h2>${T.translationsTitle}</h2><span class="caption">${esc(langName(selected))}</span></div>
      ${rows.join("")}
      <div class="page-map" id="page-map">${cells}</div>
      <div class="legend"><span class="t">${T.legendT}</span><span class="k">${T.legendK}</span><span>${T.legendN}</span></div>`;
    for (const b of $$("[data-read]", box)) b.onclick = () => go("reader", { id: book.id, lang: b.dataset.read, page: readingPrefs(book.id).page || 1 });
    for (const b of $$("[data-save]", box)) b.onclick = async () => { const p = await call("save_output", { id: book.id, lang: b.dataset.save }); if (p) toast(T.savedTo(p)); };
    for (const c of $$("#page-map i", box)) c.onclick = () => go("reader", { id: book.id, lang: selected, page: Number(c.dataset.page) });
  }
  const hist = $("#b-history");
  hist.innerHTML = `<div class="section-heading"><h2>${T.historyTitle}</h2></div>` +
    (jobs.length ? `<div class="job-list">${jobs.slice(0, 8).map((j) => jobRow(j, false)).join("")}</div>` : `<p class="caption">${T.noHistory}</p>`);
  bindJobActions(hist);
}

const pictureUrls = new Map(); // "book/image" or a file path -> object URL
async function pictureUrl(key, load) {
  if (!pictureUrls.has(key)) pictureUrls.set(key, URL.createObjectURL(new Blob([await load()])));
  return pictureUrls.get(key);
}
const drawing = new Set(); // "book/lang/page/image" being redrawn

async function renderPictures(book) {
  const list = $("#pic-list");
  if (!list) return;
  const lang = ($("#b-target")?.value.trim() || state.settings.target_lang || DEFAULT_TARGET).toLowerCase();
  const page = Math.max(1, Math.min(book.pages, Number($("#pic-page").value) || 1));
  $("#pic-lang").textContent = langName(lang);
  const images = await call("page_images", { id: book.id, page, lang });
  if (!images.length) {
    list.innerHTML = `<p class="caption">${T.picNone}</p>`;
    return;
  }
  list.innerHTML = images.map((im, i) => {
    const key = `${book.id}/${lang}/${page}/${im.image}`;
    const busy = drawing.has(key);
    return `<div class="pic-row" data-image="${esc(im.image)}">
      <figure><div class="pic-frame" data-orig="${i}"></div><figcaption>${T.picOriginal} · ${im.width}×${im.height}</figcaption></figure>
      <span class="pic-arrow">→</span>
      <figure><div class="pic-frame ${im.redrawn ? "" : "empty"}" data-new="${i}">${busy ? `<span class="pic-busy">${T.picDrawing}</span>` : ""}</div>
        <figcaption>${im.redrawn ? T.picRedrawn : langName(lang)}</figcaption></figure>
      <div class="pic-actions">
        <button class="primary-button" data-redraw ${busy ? "disabled" : ""}>${busy ? T.picDrawing : im.redrawn ? T.picAgain : T.picRedraw(langName(lang))}</button>
        ${im.redrawn && !busy ? `<button class="quiet-button" data-restore>${T.picRestore}</button>` : ""}
      </div>
    </div>`;
  }).join("");
  images.forEach(async (im, i) => {
    const orig = await pictureUrl(`${book.id}/${im.image}`, () => invoke("image_original", { id: book.id, image: im.image })).catch(() => null);
    const o = $(`[data-orig="${i}"]`, list);
    if (orig && o) o.innerHTML = `<img src="${orig}" alt="">`;
    if (im.redrawn) {
      // A new drawing replaces the file, so the key carries a stamp.
      const url = await pictureUrl(`${im.redrawn}?${state.picStamp || 0}`, () => invoke("read_file", { path: im.redrawn })).catch(() => null);
      const n = $(`[data-new="${i}"]`, list);
      if (url && n) n.innerHTML = `<img src="${url}" alt="">`;
    }
  });
  for (const row of $$(".pic-row", list)) {
    const image = row.dataset.image;
    const key = `${book.id}/${lang}/${page}/${image}`;
    $("[data-redraw]", row).onclick = async () => {
      drawing.add(key);
      renderPictures(book);
      try {
        await call("image_redraw", { id: book.id, lang, page, image, note: $("#pic-note")?.value || null });
        state.picStamp = Date.now();
        toast(T.picDone);
      } finally {
        drawing.delete(key);
        if (state.route.view === "book" && state.route.id === book.id) {
          renderPictures(book);
          renderBookStatus(book);
        }
      }
    };
    const restore = $("[data-restore]", row);
    if (restore) restore.onclick = async () => {
      await call("image_restore", { id: book.id, lang, page, image });
      toast(T.picRestored);
      renderPictures(book);
    };
  }
}

/* ============================== jobs ============================== */

function jobRow(job, showBook = true) {
  const live = state.live.get(job.id);
  const done = live && job.status === "running" ? live.done : job.done_pages;
  const pct = job.total_pages ? (100 * done) / job.total_pages : 0;
  const tokens = job.prompt_tokens + job.completion_tokens;
  const stopping = job.status === "running" && state.stopping.has(job.id);
  const sub = job.error
    ? `<p class="error">${esc(T.errors?.[job.error] || job.error)}</p>`
    : `<p>${esc(langName(job.target_lang))} · ${T.jobPages(job.pages.length)} · ${fmtDate(job.created_at)}${tokens ? ` · ${T.tokens(tokens)}` : ""}</p>`;
  const waiting = live?.message?.startsWith("wait:") && job.status === "running" ? ` · ${T.waitingS(live.message.slice(5))}` : "";
  const actions = [];
  if ((job.status === "queued" || job.status === "running") && !stopping) actions.push(`<button class="quiet-button" data-cancel="${job.id}">${T.cancel}</button>`);
  if (job.status === "failed" || job.status === "cancelled") actions.push(`<button class="quiet-button" data-retry="${job.id}">${T.retry}</button>`);
  if (showBook) actions.push(`<button class="quiet-button" data-open="${job.book_id}">${T.openBook}</button>`);
  return `<div class="job-row" data-job-row="${job.id}">
    <div class="job-main"><h3>${showBook ? esc(job.book_title) : T.jobPages(job.pages.length) + " · " + esc(langName(job.target_lang))}</h3>${sub}</div>
    <span><span class="chip ${job.status}">${stopping ? T.st_stopping : T["st_" + job.status]}</span></span>
    <div class="job-progress"><span data-job-text="${job.id}">${done}/${job.total_pages}${waiting}</span><div class="meter"><span data-job-meter="${job.id}" style="width:${pct}%"></span></div></div>
    <div class="job-actions">${actions.join("")}</div>
  </div>`;
}

function bindJobActions(root) {
  for (const b of $$("[data-cancel]", root)) b.onclick = () => {
    const id = Number(b.dataset.cancel);
    // A running job stops after the request it is waiting for; say so at once.
    if (state.jobs.find((j) => j.id === id)?.status === "running") {
      state.stopping.add(id);
      const chip = b.closest(".job-row")?.querySelector(".chip");
      if (chip) chip.textContent = T.st_stopping;
      b.remove();
    }
    call("job_cancel", { id });
  };
  for (const b of $$("[data-retry]", root)) b.onclick = () => call("job_retry", { id: Number(b.dataset.retry) });
  for (const b of $$("[data-open]", root)) b.onclick = () => go("book", { id: Number(b.dataset.open) });
}

async function renderJobs() {
  await refreshCounts();
  main.innerHTML = `<div class="page-heading"><h1>${T.jobsTitle}<span class="count">${state.jobs.length}</span></h1></div>` +
    (state.jobs.length
      ? `<section class="card"><div class="job-list">${state.jobs.map((j) => jobRow(j)).join("")}</div></section>`
      : `<div class="empty-results"><span class="hero-logo"></span><p>${T.noJobs}</p></div>`);
  bindJobActions(main);
}

/* The reader lives in reader.js. */

/* ============================== settings ============================== */

async function renderSettings(tab) {
  state.settings = await loadSettings();
  const tabs = [["general", T.tGeneral], ["connections", T.tConnections], ["models", T.tModels], ["translation", T.tTranslation], ["fonts", T.tFonts], ["storage", T.tStorage], ["about", T.tAbout]];
  main.innerHTML = `<div class="page-heading"><h1>${T.settingsTitle}</h1></div>
    <div class="settings-layout">
      <nav class="settings-tabs">${tabs.map(([k, l]) => `<button data-tab="${k}" class="${k === tab ? "active" : ""}">${l}</button>`).join("")}</nav>
      <div id="tab"></div>
    </div>`;
  for (const b of $$("[data-tab]")) b.onclick = () => go("settings", { tab: b.dataset.tab });
  const box = $("#tab");
  if (tab === "general") settingsGeneral(box);
  if (tab === "models") await settingsModels(box);
  if (tab === "translation") settingsSimple(box, [["batch_chars", T.batchChars, T.batchCharsHint, "number"], ["answer_wait", T.answerWait, T.answerWaitHint, "number"]], `<div class="setting-row"><div><h3>${T.readable}</h3><p>${T.readableHint}</p></div><span class="caption">70% · 6 pt</span></div>`);
  if (tab === "fonts") settingsSimple(box, [["fonts_dir", T.fontsDir, T.fontsHint, "text"]]);
  if (tab === "storage") await settingsStorage(box);
  if (tab === "connections") await settingsConnections(box);
  if (tab === "about") await settingsAbout(box);
}

async function saveSettings(values) {
  await call("settings_set", { values });
  Object.assign(state.settings, values);
  toast(T.settingsSaved);
}

function settingsGeneral(box) {
  const ui = (() => { try { return localStorage.getItem("celsus-ui-lang") || "system"; } catch { return "system"; } })();
  const theme = document.documentElement.dataset.theme;
  box.innerHTML = `<section class="card">
    <div class="setting-row"><div><h3>${T.uiLang}</h3><p>${T.uiLangHint}</p></div>
      <select id="g-ui"><option value="system">${T.system}</option>${UI_LANGUAGES.map(([c, n]) => `<option value="${c}">${n}</option>`).join("")}</select></div>
    <div class="setting-row"><div><h3>${T.theme}</h3><p>${T.themeHint}</p></div>
      <select id="g-theme"><option value="dark">${T.dark}</option><option value="light">${T.light}</option></select></div>
    <div class="setting-row"><div><h3>${T.defaultTarget}</h3><p>${T.defaultTargetHint}</p></div>
      <input id="g-target" list="languages" value="${esc(state.settings.target_lang)}"></div>
    <div class="setting-row"><div><h3>${T.updateCheck}</h3><p>${T.updateCheckHint}</p></div>
      <input id="g-update" type="checkbox" style="justify-self:end;width:auto" ${state.settings.update_check === "0" ? "" : "checked"}></div>
  </section>`;
  $("#g-update").onchange = (e) => saveSettings({ update_check: e.target.checked ? "1" : "0" });
  $("#g-ui").value = ui;
  $("#g-theme").value = theme;
  $("#g-ui").onchange = (e) => { try { if (e.target.value === "system") localStorage.removeItem("celsus-ui-lang"); else localStorage.setItem("celsus-ui-lang", e.target.value); } catch {} location.reload(); };
  $("#g-theme").onchange = (e) => setTheme(e.target.value);
  $("#g-target").onchange = (e) => saveSettings({ target_lang: e.target.value.trim().toLowerCase() || DEFAULT_TARGET });
}

function settingsSimple(box, fields, extra = "") {
  box.innerHTML = `<section class="card">${fields.map(([k, label, hint, type]) => `
    <div class="setting-row"><div><h3>${label}</h3><p>${hint}</p></div><input data-key="${k}" type="${type}" value="${esc(state.settings[k])}"></div>`).join("")}${extra}
    <div class="form-actions"><button class="primary-button" id="s-save">${T.save}</button></div></section>`;
  $("#s-save").onclick = () => saveSettings(Object.fromEntries($$("[data-key]", box).map((i) => [i.dataset.key, i.value.trim()])));
}

const TASKS = () => [
  ["translate", T.taskTranslate, T.taskTranslateHint],
  ["metadata", T.taskMeta, T.taskMetaHint],
  ["image", T.taskImage, T.taskImageHint],
];
const modelLists = new Map(); // connection id -> model names


async function modelsOf(provider) {
  if (!modelLists.has(provider.id)) {
    modelLists.set(provider.id, await invoke("provider_models", { url: provider.url, key: null, id: provider.id }).catch(() => []));
  }
  return modelLists.get(provider.id);
}

/** The newest picture model a connection offers, for a first suggestion. */
function suggestImageModel(models) {
  const pics = models.filter((m) => /gpt-image|chatgpt-image/.test(m) && !/mini/.test(m));
  return pics.sort((a, b) => b.localeCompare(a, "en", { numeric: true }))[0] || "";
}

async function settingsModels(box) {
  const [providers, tasks] = await Promise.all([invoke("providers_list"), invoke("task_models")]);
  if (!providers.length) {
    box.innerHTML = `<section class="card stack"><p class="caption">${T.noConnFirst}</p>
      <div><button class="primary-button" id="go-conn">${T.goConnections}</button></div></section>`;
    $("#go-conn").onclick = () => go("settings", { tab: "connections" });
    return;
  }
  box.innerHTML = `<section class="card"><div class="section-heading"><h2>${T.tModels}</h2></div>
    <p class="hint" style="margin-bottom:6px">${T.tasksHint} ${T.modelsNote}</p>
    ${TASKS().map(([k, label, hint]) => {
      const t = tasks.find((x) => x.task === k) || {};
      const shown = t.chosen || k === "translate";
      const empty = k === "translate" ? "" : `<option value="">${k === "metadata" ? T.sameAsTranslate : T.none}</option>`;
      return `<div class="task-row" data-task="${k}">
        <div class="task-head"><h3>${label}</h3><p>${hint}</p></div>
        <div class="task-fields">
          <select data-conn>${empty}${providers.map((p) => `<option value="${p.id}" ${shown && t.provider_id === p.id ? "selected" : ""}>${esc(p.name)}</option>`).join("")}</select>
          <div class="model-pick"><input data-model placeholder="${T.modelPlaceholder}" value="${esc(shown ? t.model : "")}" autocomplete="off" spellcheck="false">
            <div class="model-list" data-list hidden></div></div>
        </div>
        <div class="task-actions">
          <button class="quiet-button" data-try hidden>${T.modelsTry}</button>
          <button class="quiet-button" data-test>${T.testShort}</button>
          <button class="primary-button" data-save>${T.save}</button>
        </div>
        <div data-result></div>
      </div>`;
    }).join("")}
  </section>`;
  for (const row of $$(".task-row", box)) {
    const task = row.dataset.task;
    const conn = $("[data-conn]", row), input = $("[data-model]", row), out = $("[data-result]", row);
    const provider = () => providers.find((p) => p.id === Number(conn.value));
    // The models of the chosen connection: those fit for the job, and all of them.
    let offered = [[], []];
    const fill = async () => {
      const p = provider();
      input.disabled = !p;
      offered = [[], []];
      if (!p) { input.value = ""; return; }
      const models = await modelsOf(p);
      if (p !== provider()) return;
      offered = task === "image"
        ? [models.filter((m) => /image|dall-e/.test(m)), models]
        : [models.filter((m) => !NOT_CHAT.test(m)), models];
      if (!input.value && task === "image") input.value = suggestImageModel(offered[0]);
      refresh();
    };
    const refresh = modelField({
      input, list: $("[data-list]", row), button: $("[data-try]", row), out,
      connection: () => conn.value,
      models: () => offered,
      ask: (model, id) => {
        const p = providers.find((x) => x.id === Number(id));
        return invoke("model_test", { providerId: p.id, url: p.url, model, picture: false, quick: true })
          .catch((e) => ({ ok: false, millis: 0, error: String(e) }));
      },
      canTry: () => { const p = provider(); return !!p && task !== "image" && !LOCAL_URL.test(p.url); },
    });
    conn.onchange = () => { input.value = ""; out.innerHTML = ""; fill(); };
    fill();
    $("[data-test]", row).onclick = async () => {
      const p = provider();
      if (!p || !input.value.trim()) return;
      out.innerHTML = `<div class="test-result">${T.testing}</div>`;
      const model = input.value.trim();
      const r = await invoke("model_test", { providerId: p.id, url: p.url, model, picture: task === "image" });
      if (task !== "image") rememberTest(p.id, model, r.ok, r.millis);
      out.innerHTML = r.ok
        ? `<div class="test-result ok"><b>${task === "image" ? T.pictureModelOk : T.testOk(r.millis)}</b><br>${esc(r.sample)}</div>`
        : `<div class="test-result bad">${esc(r.error)}</div>`;
    };
    $("[data-save]", row).onclick = async () => {
      const p = provider();
      await call("task_model_set", { task, providerId: p ? p.id : null, model: p ? input.value.trim() : "" });
      toast(T.settingsSaved);
      refreshProfile();
    };
  }
}

async function apiConnections(box) {
  const providers = await invoke("providers_list");
  const list = providers.length
    ? providers.map((p) => `<div class="provider">
        <div class="info"><h3>${esc(p.name)}</h3><p>${esc(p.url)} · ${p.has_key ? T.keyStored : T.noKey}</p></div>
        <button class="quiet-button" data-edit="${p.id}">${T.edit}</button>
        <button class="text-button danger" data-remove="${p.id}">${T.remove}</button></div>`).join("")
    : `<p class="caption">${T.noConnections}</p>`;
  box.innerHTML = `<div class="section-heading"><h2>${T.apiTitle}</h2></div>
    <p class="hint" style="font-size:13px;margin-bottom:12px">${T.apiText}</p>
    <div class="preset-row">${PRESETS.map(([n, u]) => `<button class="preset" data-preset="${esc(u)}" data-name="${esc(n)}">${esc(n)}</button>`).join("")}
      <button class="preset" data-preset="" data-name="">${T.custom}</button></div>
    <div class="provider-list">${list}</div>
    <div class="stack" id="m-form" hidden></div>`;
  for (const b of $$("[data-preset]", box)) b.onclick = () => connectionForm({ id: 0, name: b.dataset.name, url: b.dataset.preset, model: "", is_default: !providers.length, has_key: false });
  for (const b of $$("[data-edit]", box)) b.onclick = () => connectionForm(providers.find((p) => p.id === Number(b.dataset.edit)));
  for (const b of $$("[data-remove]", box)) b.onclick = async () => {
    const p = providers.find((x) => x.id === Number(b.dataset.remove));
    if (!confirm(T.confirmRemoveModel(p.name))) return;
    await call("provider_delete", { id: p.id });
    modelLists.delete(p.id);
    refreshProfile();
    apiConnections(box);
  };

  function connectionForm(p) {
    const form = $("#m-form", box);
    const kind = presetOf(p.url);
    const guide = GUIDES[kind] || GUIDES.custom;
    form.hidden = false;
    form.innerHTML = `<div class="section-heading" style="margin:8px 0 0"><h2>${p.id ? esc(p.name) : `${T.addConnection}${kind !== "custom" ? ` · ${esc(kind)}` : ""}`}</h2></div>
      <div class="guide">
        <ol>${guide.steps.map((s) => `<li>${esc(s)}</li>`).join("")}</ol>
        ${guide.link ? `<button class="quiet-button" id="f-link"><svg viewBox="0 0 24 24"><path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/></svg>${esc(guide.link[1])}</button>` : ""}
        ${guide.note ? `<p class="hint">${esc(guide.note)}</p>` : ""}
      </div>
      ${guide.local ? `<div id="f-local" class="local-status"></div>` : ""}
      <div class="row"><label>${T.name}<input id="f-name" value="${esc(p.name)}"></label></div>
      ${guide.local
        ? `<input id="f-url" type="hidden" value="${esc(p.url)}">`
        : `<label>${T.url}<input id="f-url" value="${esc(p.url)}" placeholder="https://…/v1"></label>`}
      <label ${guide.local ? "hidden" : ""}>${T.apiKey}<input id="f-key" type="password" placeholder="${p.has_key ? T.keyKeep : kind === "NVIDIA" ? "nvapi-…" : ""}" autocomplete="off"></label>
      <div id="f-result"></div>
      <div class="form-actions"><button class="quiet-button" id="f-cancel">${T.cancelEdit}</button>
        <button class="quiet-button" id="f-check">${guide.local ? T.tryAgain : T.checkConnection}</button><button class="primary-button" id="f-save">${T.save}</button></div>`;
    form.scrollIntoView({ behavior: "smooth", block: "nearest" });
    if (!guide.local) $("#f-key").focus();
    if (guide.link) $("#f-link").onclick = () => call("open_url", { url: guide.link[0] });
    const vals = () => ({ url: $("#f-url").value.trim(), key: $("#f-key")?.value.trim() || null, id: p.id || null });
    const probeLocal = async () => {
      const out = $("#f-local");
      out.className = "local-status";
      out.innerHTML = `<span>${T.testing}</span>`;
      try {
        const models = await invoke("provider_models", { url: p.url, key: null, id: null });
        out.className = "local-status ok";
        out.innerHTML = `<b>${T.localRunning(kind, models.length)}</b>${models.length ? `<small>${esc(models.slice(0, 6).join(" · "))}${models.length > 6 ? " …" : ""}</small>` : `<small>${T.localNoModels}</small>`}`;
        $("#f-save").disabled = false;
      } catch {
        out.className = "local-status bad";
        out.innerHTML = `<b>${T.localDown(kind)}</b><small>${T.localDownHint}</small>`;
      }
    };
    if (guide.local) probeLocal();
    $("#f-check").onclick = async () => {
      if (guide.local) return probeLocal();
      const out = $("#f-result");
      out.innerHTML = `<div class="test-result">${T.testing}</div>`;
      try {
        const models = await invoke("provider_models", vals());
        out.innerHTML = `<div class="test-result ok"><b>${T.connectionOk(models.length)}</b></div>`;
      } catch (e) {
        out.innerHTML = `<div class="test-result bad">${esc(e)}</div>`;
      }
    };
    $("#f-cancel").onclick = () => (form.hidden = true);
    $("#f-save").onclick = async () => {
      const v = vals();
      const provider = { id: p.id, name: $("#f-name").value.trim() || "API", url: v.url, model: p.model, is_default: p.is_default, has_key: false };
      await call("provider_save", { provider, key: v.key });
      modelLists.delete(p.id);
      refreshProfile();
      const tasks = await invoke("task_models");
      if (!tasks.find((t) => t.task === "translate")?.model) {
        toast(T.chooseModelsNow);
        go("settings", { tab: "models" });
      } else {
        toast(T.settingsSaved);
        apiConnections(box);
      }
    };
  }
}

async function settingsStorage(box) {
  const s = await invoke("storage_info");
  box.innerHTML = `<section class="card"><dl class="kv">
      <dt>${T.libraryPath}</dt><dd>${esc(s.library)}</dd>
      <dt>${T.libraryBytes}</dt><dd>${fmtBytes(s.library_bytes)}</dd>
      <dt>${T.database}</dt><dd>${esc(s.database)}</dd>
      <dt>${T.cache}</dt><dd>${T.cacheText(s.cached_translations, s.cached_chars)}</dd></dl>
    <div class="form-actions"><button class="quiet-button danger" id="c-clear">${T.clearCache}</button></div></section>`;
  $("#c-clear").onclick = async () => {
    if (!confirm(T.confirmClear)) return;
    await call("cache_clear");
    toast(T.cleared);
    settingsStorage(box);
  };
}

async function settingsConnections(box) {
  box.innerHTML = `<section class="card" id="api-connections"></section><section class="card stack" id="mcp-card"></section>`;
  await apiConnections($("#api-connections"));
  await mcpCard($("#mcp-card"));
}

async function mcpCard(box) {
  const m = await invoke("mcp_info");
  const found = m.clients.filter((c) => c.found);
  const others = m.clients.filter((c) => !c.found);
  const added = m.clients.filter((c) => c.state === "added").length;
  const chip = (c) => ({
    added: `<span class="chip done">${T.installed}</span>`,
    outdated: `<span class="chip queued">${T.mcpOutdated}</span>`,
    unreadable: `<span class="chip failed">${T.mcpUnreadable}</span>`,
    not_added: `<span class="chip">${T.notInstalled}</span>`,
  })[c.state];
  const row = (c) => `<div class="mcp-app" data-app="${c.id}">
      <span class="mcp-mark">${esc(c.name.slice(0, 1))}</span>
      <div class="mcp-info"><h3>${esc(c.name)}<small>${esc(c.about)}</small></h3><p title="${esc(c.file)}">${esc(c.file)}</p></div>
      ${chip(c)}
      <div class="mcp-actions">
        ${c.state === "added"
          ? `<button class="text-button danger" data-mcp-remove>${T.remove}</button>`
          : `<button class="primary-button" data-mcp-add ${m.command_exists && c.state !== "unreadable" ? "" : "disabled"}>${c.state === "outdated" ? T.mcpUpdate : T.mcpAdd}</button>`}
        <button class="quiet-button" data-mcp-snippet title="${T.manual}">{ }</button>
      </div>
      <pre class="code" data-snippet hidden>${esc(c.snippet)}</pre>
    </div>`;
  box.innerHTML = `
    <div class="section-heading"><h2>${T.mcpTitle}</h2><span class="caption">${T.mcpCount(added, m.clients.length)}</span></div>
    <p class="hint" style="font-size:13px">${T.mcpText}</p>
    <div class="mcp-server ${m.command_exists ? "" : "bad"}"><b>${T.mcpServer}</b><span>${esc(m.command)}</span>
      <span class="chip ${m.command_exists ? "done" : "failed"}">${m.command_exists ? T.mcpReady : T.mcpMissing}</span></div>
    ${found.length ? `<div class="mcp-group"><h4>${T.mcpFound}</h4>${found.map(row).join("")}</div>` : ""}
    ${others.length ? `<details class="mcp-group"><summary>${T.mcpOthers(others.length)}</summary>${others.map(row).join("")}</details>` : ""}
    <div class="mcp-note"><b>ChatGPT</b><span>${T.mcpChatGpt}</span></div>
    <details class="mcp-group"><summary>${T.manual}</summary>
      <p class="hint">${T.mcpManualHint}</p>
      <div class="section-heading" style="margin:10px 0 6px"><h2>${T.claudeCode}</h2><button class="text-button" data-copy="code">${T.copy}</button></div>
      <pre class="code">${esc(m.claude_code_command)}</pre>
      <div class="section-heading" style="margin:10px 0 6px"><h2>JSON</h2><button class="text-button" data-copy="json">${T.copy}</button></div>
      <pre class="code">${esc(m.generic_snippet)}</pre>
    </details>
    <p class="hint">${T.mcpTry}</p>`;
  for (const el of $$(".mcp-app", box)) {
    const id = el.dataset.app;
    const c = m.clients.find((x) => x.id === id);
    const add = $("[data-mcp-add]", el);
    if (add) add.onclick = async () => {
      await call("mcp_client_add", { id });
      toast(T.mcpAddedTo(c.name));
      mcpCard(box);
    };
    const remove = $("[data-mcp-remove]", el);
    if (remove) remove.onclick = async () => {
      if (!confirm(T.mcpConfirmRemove(c.name))) return;
      await call("mcp_client_remove", { id });
      toast(T.mcpRemoved(c.name));
      mcpCard(box);
    };
    $("[data-mcp-snippet]", el).onclick = () => {
      const pre = $("[data-snippet]", el);
      pre.hidden = !pre.hidden;
    };
  }
  for (const b of $$("[data-copy]", box)) b.onclick = async () => {
    await navigator.clipboard.writeText(b.dataset.copy === "code" ? m.claude_code_command : m.generic_snippet);
    toast(T.copied);
  };
}

async function settingsAbout(box) {
  const version = await invoke("app_version");
  box.innerHTML = `<section class="card stack">
      <div class="about"><span class="about-logo"></span><div><h2>Celsus</h2><span class="caption">${T.version} ${esc(version)}</span></div></div>
      <p class="hint" style="font-size:13px">${T.aboutText}</p>
      <div class="row" id="about-update" style="align-items:center;gap:12px" hidden></div></section>
    <section class="card"><div class="section-heading"><h2>${T.licenses}</h2></div><dl class="kv">
      <dt>Celsus</dt><dd>MIT</dd>
      <dt>pdf.js</dt><dd>Apache-2.0</dd>
      <dt>rustybuzz</dt><dd>MIT</dd>
      <dt>DM Sans</dt><dd>SIL Open Font License 1.1</dd>
      <dt>Instrument Serif</dt><dd>SIL Open Font License 1.1</dd>
      <dt>Tauri</dt><dd>MIT / Apache-2.0</dd>
      <dt>SQLite</dt><dd>Public domain</dd></dl></section>`;
  if (state.settings.update_check === "0") return;
  const update = await invoke("update_check").catch(() => undefined);
  const line = $("#about-update");
  if (update === undefined || !line) return;
  line.hidden = false;
  line.innerHTML = update
    ? `<span class="caption">${esc(T.updateOut(update.version))}</span><button class="primary-button">${update.installs_itself ? T.updateInstall : T.updateGet}</button>`
    : `<span class="caption">${T.upToDate}</span>`;
  if (update) $("button", line).onclick = (e) => getUpdate(update, e.currentTarget);
}

/**
 * Put a newer release in place: the app downloads it, checks its signature and starts again. Where the app
 * cannot replace itself, or that fails, the release's page is opened instead.
 */
async function getUpdate(update, button) {
  if (!update.installs_itself) { call("open_url", { url: update.url }); return; }
  button.disabled = true;
  button.textContent = T.updateInstalling;
  try {
    await invoke("update_install");
  } catch {
    toast(T.updateFailed, true);
    call("open_url", { url: update.url });
    button.disabled = false;
    button.textContent = T.updateGet;
    update.installs_itself = false;
  }
}

/** Say that a newer release is out, once for each release, until it is closed. */
async function announceUpdate() {
  if (state.settings.update_check === "0") return;
  const update = await invoke("update_check").catch(() => null);
  if (!update) return;
  try { if (localStorage.getItem("celsus-update-seen") === update.version) return; } catch {}
  const bar = document.createElement("div");
  bar.className = "update-bar";
  bar.innerHTML = `<span>${esc(T.updateOut(update.version))}</span><button class="primary-button" data-get>${update.installs_itself ? T.updateInstall : T.updateGet}</button><button class="quiet-button" data-close aria-label="${T.winClose}">✕</button>`;
  document.body.append(bar);
  const close = () => { try { localStorage.setItem("celsus-update-seen", update.version); } catch {} bar.remove(); };
  $("[data-get]", bar).onclick = async (e) => {
    const itself = update.installs_itself;
    await getUpdate(update, e.currentTarget);
    // Sent to the download page: the notice has done its work.
    if (!itself) close();
  };
  $("[data-close]", bar).onclick = close;
}

/* ============================== profile ============================== */

async function renderProfile() {
  state.settings = await loadSettings();
  const u = await invoke("usage_stats");
  const name = state.settings.profile_name || "";
  main.innerHTML = `<div class="page-heading"><h1>${T.profileTitle}</h1></div>
    <section class="card" style="max-width:720px">
      <div class="row" style="align-items:center;gap:18px">
        <span class="avatar big" id="p-avatar">${esc(initials(name || T.profileDefault))}</span>
        <label>${T.yourName}<input id="p-name" value="${esc(name)}"></label>
        <button class="primary-button" id="p-save" style="align-self:flex-end">${T.save}</button>
      </div>
      <p class="hint" style="margin-top:12px">${T.profileHint}</p>
    </section>
    <div class="stats-grid" style="max-width:720px;margin-top:16px">
      <div class="stat"><b>${num(u.books)}</b><small>${T.sBooks}</small></div>
      <div class="stat"><b>${num(u.pages)}</b><small>${T.sPages}</small></div>
      <div class="stat"><b>${num(u.prompt_tokens + u.completion_tokens)}</b><small>${T.sTokens}</small></div>
      <div class="stat"><b>${num(u.month_pages)}</b><small>${T.sMonth} · ${T.pagesN(u.month_pages).replace(/^\d+\s*/, "")}</small></div>
    </div>`;
  $("#p-name").oninput = (e) => ($("#p-avatar").textContent = initials(e.target.value || T.profileDefault));
  $("#p-save").onclick = async () => { await saveSettings({ profile_name: $("#p-name").value.trim() }); refreshProfile(); };
}

/* ============================== events ============================== */

listen("book-recognised", ({ payload: id }) => {
  if (state.route.view === "book" && state.route.id === id && !main.contains(document.activeElement)) go("book", { id });
});

listen("library-changed", async () => {
  // Translated files may have been rewritten under the same name.
  readerFilesChanged();
  await refreshCounts();
  if (state.route.view === "library") {
    if ($("#grid")) drawGrid();
    else renderLibrary();
  }
});

listen("job-updated", async ({ payload: job }) => {
  if (job.status !== "running") state.stopping.delete(job.id);
  // A job that stopped on its own says why, wherever the user is in the app.
  if (job.status === "failed" && job.error) toast(T.errors?.[job.error] || job.error, true);
  await refreshCounts();
  const v = state.route.view;
  if (v === "jobs") renderJobs();
  if (v === "book" && state.route.id === job.book_id) renderBookStatus({ id: job.book_id });
  if (v === "library" && $("#grid")) drawGrid();
});

listen("job-progress", ({ payload: p }) => {
  state.live.set(p.job_id, p);
  for (const el of $$(`[data-job-meter="${p.job_id}"]`)) el.style.width = `${p.total ? (100 * p.done) / p.total : 0}%`;
  for (const el of $$(`[data-job-text="${p.job_id}"]`)) {
    el.textContent = `${p.done}/${p.total}${p.message?.startsWith("wait:") ? ` · ${T.waitingS(p.message.slice(5))}` : ""}`;
  }
  if (state.route.view === "book" && state.route.id === p.book_id && p.page) {
    for (const c of $$("#page-map i.r")) c.classList.remove("r");
    const cell = $(`#page-map i[data-page="${p.page}"]`);
    if (cell && !p.message) cell.className = "t";
    const next = $(`#page-map i[data-page="${p.page + 1}"]`);
    if (next && !next.className) next.className = "r";
  }
});

listen("tauri://drag-enter", () => ($("#drop-overlay").hidden = false));
listen("tauri://drag-leave", () => ($("#drop-overlay").hidden = true));
listen("tauri://drag-drop", ({ payload }) => {
  $("#drop-overlay").hidden = true;
  const pdfs = (payload.paths || []).filter((p) => p.toLowerCase().endsWith(".pdf"));
  if (pdfs.length) addBooks(pdfs);
});

/* ============================== start ============================== */

for (const b of $$("[data-go]")) b.onclick = () => !b.disabled && go(b.dataset.go);
for (const b of $$("[data-theme-choice]")) b.onclick = () => setTheme(b.dataset.themeChoice);
function fillLanguages() {
  $("#languages").innerHTML = LANGUAGES.map(([c, n, en]) =>
    `<option value="${esc(c)}">${esc(en && en !== n ? `${n} · ${en}` : n)}</option>`).join("");
}
fillLanguages();
invoke("languages").then((list) => {
  LANGUAGES = list.map((l) => [l.code, l.native, l.english]);
  fillLanguages();
}).catch(() => {});
applyStrings();
setTheme(document.documentElement.dataset.theme || "dark");
(async () => {
  state.settings = await loadSettings().catch(() => ({ target_lang: DEFAULT_TARGET }));
  await refreshProfile().catch(() => {});
  go("library");
  announceUpdate();
})();
