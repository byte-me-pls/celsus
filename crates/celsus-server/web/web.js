/* Celsus on the web: the same library and reader as the desktop app, talking to the server's API.
   No inline scripts or styles anywhere, so the strict Content-Security-Policy holds. Every value from the server
   is escaped before it becomes HTML. */

const T = uiStrings("web");

// The full list (about 130 languages) comes from the backend; these are shown until it arrives.
let LANGUAGES = [
  ["tr", "Türkçe"], ["en", "English"], ["de", "Deutsch"], ["fr", "Français"], ["es", "Español"], ["ru", "Русский"],
  ["ar", "العربية"], ["zh-hans", "简体中文"], ["ja", "日本語"], ["ko", "한국어"], ["hi", "हिन्दी"],
];
const langName = (code) => (LANGUAGES.find(([c]) => c === String(code).toLowerCase()) || [code, String(code).toUpperCase()])[1];

/* ============================== helpers ============================== */

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const main = $("#main");
const num = (n) => Number(n || 0).toLocaleString(UI_LANG);
const fmtDate = (s) => (s ? new Date(s * 1000).toLocaleString(UI_LANG, { dateStyle: "medium", timeStyle: "short" }) : "");
const fmtBytes = (n) => (n > 1e9 ? `${(n / 1e9).toFixed(1)} GB` : n > 1e6 ? `${(n / 1e6).toFixed(1)} MB` : `${Math.round(n / 1e3)} KB`);

function toast(message, error = false) {
  const t = $("#toast");
  t.textContent = message;
  t.className = error ? "toast error" : "toast";
  t.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (t.hidden = true), error ? 7000 : 3200);
}

/* ============================== API ============================== */

const state = {
  me: null,
  csrf: null,
  route: { view: "library" },
  books: [],
  jobs: [],
  live: new Map(),
  settings: { target_lang: UI_LANG === "zh" ? "zh-hans" : UI_LANG },
  filter: "all",
  query: "",
};

class ApiError extends Error {
  constructor(status, body) {
    super(body?.message || `HTTP ${status}`);
    this.status = status;
    this.code = body?.error;
    this.retryAfter = body?.retry_after;
  }
}

async function api(method, path, body, extraHeaders = {}) {
  const headers = { ...extraHeaders };
  if (method !== "GET" && state.csrf) headers["X-CSRF-Token"] = state.csrf;
  let payload = body;
  if (body !== undefined && !(body instanceof Blob) && !(body instanceof ArrayBuffer)) {
    headers["Content-Type"] = "application/json";
    payload = JSON.stringify(body);
  }
  const res = await fetch(path, { method, headers, body: payload, credentials: "same-origin", cache: "no-store" });
  const text = await res.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch {}
  if (!res.ok) {
    const err = new ApiError(res.status, data);
    err.retryAfter = Number(res.headers.get("retry-after")) || undefined;
    if (res.status === 401 && state.me && !path.startsWith("/api/auth/")) {
      // The session ended (signed out elsewhere, expired, account disabled).
      state.me = null;
      showGate();
    }
    throw err;
  }
  return data;
}

/** Call the API and show a failure as a toast. */
async function apiCall(method, path, body) {
  try {
    return await api(method, path, body);
  } catch (e) {
    toast(e.status === 429 && e.retryAfter ? T.waitSeconds(e.retryAfter) : e.message, true);
    throw e;
  }
}

/* The reader (reader.js, shared with the desktop app) asks for things by name; these answer from the API. */
const handlers = {};
function listen(event, fn) {
  (handlers[event] ||= []).push(fn);
}
function emit(event, payload) {
  for (const fn of handlers[event] || []) fn({ payload });
}

const bookUrl = (id, which) => `/api/books/${encodeURIComponent(id)}/file/${encodeURIComponent(which)}`;

async function invoke(cmd, a = {}) {
  switch (cmd) {
    case "read_file": {
      const res = await fetch(a.path, { credentials: "same-origin" });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      return res.arrayBuffer();
    }
    case "book_get":
      return (await api("GET", `/api/books/${encodeURIComponent(a.id)}`)).book;
    case "book_files": {
      const book = (await api("GET", `/api/books/${encodeURIComponent(a.id)}`)).book;
      const outputs = Object.fromEntries(Object.keys(book.translated || {}).map((l) => [l, bookUrl(a.id, l)]));
      return { source: bookUrl(a.id, "source"), cover: null, outputs };
    }
    case "book_pages":
      return (await api("GET", `/api/books/${encodeURIComponent(a.id)}/pages/${encodeURIComponent(a.lang)}`)).pages;
    case "job_create":
      return api("POST", `/api/books/${encodeURIComponent(a.bookId)}/jobs`, { lang: a.lang, pages: a.pages });
    case "save_output": {
      const link = document.createElement("a");
      link.href = bookUrl(a.id, a.lang);
      link.download = "";
      document.body.appendChild(link);
      link.click();
      link.remove();
      return null;
    }
    default:
      throw new Error(`unknown command ${cmd}`);
  }
}
/** The reader (shared with the desktop app) calls commands by name and expects failures to be shown. */
async function call(cmd, args) {
  try { return await invoke(cmd, args); } catch (e) { toast(e.message, true); throw e; }
}

/** The reader keeps the open view here. */
let reader = null;

/* ============================== signed out ============================== */

function showGate() {
  $("#app").hidden = true;
  $("#gate").hidden = false;
  routeGate();
}

async function routeGate() {
  const hash = location.hash;
  if (hash.startsWith("#/invite/")) return gateInvite(decodeURIComponent(hash.slice(9)));
  try {
    const s = await api("GET", "/api/setup");
    if (s.needed) return gateSetup();
  } catch {}
  gateLogin();
}

function gateForm(title, text, fields, submit, onSubmit, extra = "") {
  const box = $("#gate-body");
  box.innerHTML = `<h1 class="gate-title">${esc(title)}</h1>${text ? `<p class="hint gate-text">${esc(text)}</p>` : ""}
    <form id="gate-form" class="stack" autocomplete="on">
      ${fields.map(([id, label, type, auto, hint]) => `<label>${esc(label)}<input id="${id}" type="${type}" autocomplete="${auto}" required>${hint ? `<small class="hint">${esc(hint)}</small>` : ""}</label>`).join("")}
      <div class="gate-error" id="gate-error" hidden></div>
      <button class="primary-button wide" type="submit">${esc(submit)}</button>
      ${extra}
    </form>`;
  const form = $("#gate-form");
  form.querySelector("input")?.focus();
  form.onsubmit = async (e) => {
    e.preventDefault();
    const btn = form.querySelector("button[type=submit]");
    btn.disabled = true;
    $("#gate-error").hidden = true;
    try {
      await onSubmit(Object.fromEntries(fields.map(([id]) => [id, $(`#${id}`).value])));
    } catch (err) {
      const msg = err.status === 429 && err.retryAfter ? T.waitSeconds(err.retryAfter) : err.message;
      $("#gate-error").textContent = msg;
      $("#gate-error").hidden = false;
    } finally {
      btn.disabled = false;
    }
  };
}

function signedIn(res) {
  state.me = res.user;
  state.csrf = res.csrf;
  if (location.hash.startsWith("#/invite/")) history.replaceState(null, "", location.pathname);
  startApp();
}

function gateLogin() {
  gateForm(T.signInTitle, "", [["g-email", T.email, "email", "username"], ["g-password", T.password, "password", "current-password"]], T.signIn, async (v) => {
    const res = await api("POST", "/api/auth/login", { email: v["g-email"], password: v["g-password"] });
    if (res.two_step_required) return gateTwoStep();
    signedIn(res);
  });
}

function gateTwoStep() {
  gateForm(T.twoStepTitle, T.twoStepText, [["g-code", T.code, "text", "one-time-code"]], T.verify, async (v) => {
    signedIn(await api("POST", "/api/auth/two-step", { code: v["g-code"] }));
  }, `<button class="text-button" type="button" id="g-back">${esc(T.back)}</button>`);
  $("#g-code").inputMode = "numeric";
  $("#g-back").onclick = gateLogin;
}

function gateSetup() {
  gateForm(T.setupTitle, T.setupText, [
    ["g-code", T.setupCode, "text", "off"],
    ["g-name", T.name, "text", "name"],
    ["g-email", T.email, "email", "username"],
    ["g-password", T.password, "password", "new-password", T.passwordHint],
  ], T.createOwner, async (v) => {
    signedIn(await api("POST", "/api/setup", { code: v["g-code"], name: v["g-name"], email: v["g-email"], password: v["g-password"] }));
  });
}

function gateInvite(token) {
  gateForm(T.inviteTitle, T.inviteText, [
    ["g-name", T.name, "text", "name"],
    ["g-email", T.email, "email", "username"],
    ["g-password", T.password, "password", "new-password", T.passwordHint],
  ], T.join, async (v) => {
    signedIn(await api("POST", "/api/invites/accept", { token, name: v["g-name"], email: v["g-email"], password: v["g-password"] }));
  });
}

/* ============================== shell ============================== */

function applyStrings(root = document) {
  for (const el of $$("[data-i18n]", root)) el.textContent = T[el.dataset.i18n];
  for (const el of $$("[data-i18n-title]", root)) el.title = el.ariaLabel = T[el.dataset.i18nTitle];
}

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

const initials = (name) => String(name || "").split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0].toUpperCase()).join("") || "C";

async function refreshCounts() {
  const [books, jobs] = await Promise.all([api("GET", "/api/books"), api("GET", "/api/jobs")]);
  state.books = books.books;
  state.jobs = jobs.jobs.map((j) => ({ ...j, target_lang: j.lang, book_title: j.book_title }));
  $("#count-books").textContent = state.books.length || "";
  const active = state.jobs.filter((j) => j.status === "queued" || j.status === "running").length;
  $("#count-jobs").textContent = active || "";
  $("#count-jobs").classList.toggle("live", active > 0);
}

/** Routes live in the address bar so the back button and links work. */
function routeToHash(view, p) {
  if (view === "book") return `#/book/${p.id}`;
  if (view === "reader") return `#/read/${p.id}/${p.lang || "orig"}/${p.page || 1}`;
  if (view === "admin") return `#/admin/${p.tab || "members"}`;
  return `#/${view}`;
}
function hashToRoute() {
  const parts = location.hash.replace(/^#\/?/, "").split("/").map(decodeURIComponent);
  switch (parts[0]) {
    case "book": return { view: "book", id: parts[1] };
    case "read": return { view: "reader", id: parts[1], lang: parts[2] || "orig", page: Number(parts[3]) || 1 };
    case "jobs": return { view: "jobs" };
    case "account": return { view: "account" };
    case "admin": return { view: "admin", tab: parts[1] || "members" };
    default: return { view: "library" };
  }
}

function go(view, params = {}) {
  const hash = routeToHash(view, params);
  if (location.hash !== hash) history.pushState(null, "", hash);
  route({ view, ...params });
}

window.addEventListener("popstate", () => state.me && route(hashToRoute()));

function route(r) {
  state.route = r;
  if (r.view !== "reader" && reader) { reader.destroy(); reader = null; }
  for (const b of $$("[data-go]")) b.classList.toggle("active", b.dataset.go === r.view || ((r.view === "book" || r.view === "reader") && b.dataset.go === "library"));
  main.className = r.view === "reader" ? "reader-host" : "";
  main.scrollTop = 0;
  setCrumbs({ library: T.navLibrary, book: T.navLibrary, reader: T.navLibrary, jobs: T.navJobs, account: T.navAccount, admin: T.navAdmin }[r.view]);
  render();
}

async function render() {
  const r = state.route;
  try {
    if (r.view === "library") await renderLibrary();
    else if (r.view === "book") await renderBook(r.id);
    else if (r.view === "reader") await renderReader(r);
    else if (r.view === "jobs") await renderJobs();
    else if (r.view === "account") await renderAccount();
    else if (r.view === "admin") await renderAdmin(r.tab);
  } catch (e) {
    if (e.status === 401) return;
    main.innerHTML = `<div class="empty-results"><h2>:(</h2><p>${esc(e.message)}</p></div>`;
  }
}

/* ============================== library ============================== */

function coverHtml(b) {
  return b.has_cover
    ? `<img src="/api/books/${esc(b.id)}/cover" alt="" loading="lazy">`
    : `<div class="made-cover"><b>${esc(b.title)}</b><small>${esc(b.author || "")}</small></div>`;
}

function bookStatus(book) {
  const job = state.jobs.find((j) => j.book_id === book.id && (j.status === "running" || j.status === "queued"));
  if (job) {
    const pct = job.total_pages ? (100 * job.done_pages) / job.total_pages : 0;
    return `<div class="book-status">${esc(T.translating)} ${job.done_pages}/${job.total_pages}<div class="meter"><span class="w" data-w="${pct}"></span></div></div>`;
  }
  const langs = Object.entries(book.translated || {});
  if (!langs.length) return `<div class="book-status">${esc(T.notTranslated)}</div>`;
  const [lang, n] = langs.sort((a, b) => b[1] - a[1])[0];
  return `<div class="book-status">${esc(T.translatedOf(lang, n, book.pages))}<div class="meter"><span class="w" data-w="${(100 * n) / book.pages}"></span></div></div>`;
}

/** Widths come from data attributes and are set through the DOM, which the CSP allows. */
function applyWidths(root = main) {
  for (const el of $$(".w[data-w]", root)) el.style.width = `${Math.max(0, Math.min(100, Number(el.dataset.w)))}%`;
}

function filteredBooks() {
  const q = state.query.trim().toLowerCase();
  return state.books.filter((b) => {
    if (q && !`${b.title} ${b.author}`.toLowerCase().includes(q)) return false;
    if (state.filter === "mine") return b.mine;
    if (state.filter === "shared") return b.visibility === "shared";
    if (state.filter === "busy") return b.busy;
    return true;
  });
}

async function renderLibrary() {
  await refreshCounts();
  if (state.route.view !== "library") return;
  const filters = [["all", T.fAll], ["mine", T.fMine], ["shared", T.fShared], ["busy", T.fBusy]];
  main.innerHTML = `
    <div class="page-heading">
      <h1>${esc(T.libraryTitle)}<span class="count">${state.books.length}</span></h1>
      <div class="heading-actions"><button class="primary-button" id="add-books"><svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>${esc(T.addBooks)}</button></div>
    </div>
    <input type="file" id="file-input" accept="application/pdf,.pdf" multiple hidden>
    <div id="upload-progress" class="upload-progress" hidden><span id="upload-text"></span><div class="meter"><span id="upload-meter"></span></div></div>
    ${state.books.length ? `
    <div class="toolbar">
      <label class="search-field"><svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/></svg>
        <input id="search" type="search" placeholder="${esc(T.search)}" value="${esc(state.query)}"></label>
    </div>
    <div class="filters">${filters.map(([k, l]) => `<button data-filter="${k}" class="${state.filter === k ? "selected" : ""}">${esc(l)}</button>`).join("")}</div>
    <div class="book-grid" id="grid"></div>` : `
    <div class="empty-results"><span class="hero-logo"></span><h2>${esc(T.emptyTitle)}</h2><p>${esc(T.emptyText)}</p></div>`}`;
  $("#add-books").onclick = () => $("#file-input").click();
  $("#file-input").onchange = (e) => uploadFiles([...e.target.files]);
  if (!state.books.length) return;
  $("#search").oninput = (e) => { state.query = e.target.value; drawGrid(); };
  for (const b of $$("[data-filter]")) b.onclick = () => { state.filter = b.dataset.filter; for (const x of $$("[data-filter]")) x.classList.toggle("selected", x === b); drawGrid(); };
  drawGrid();
}

function drawGrid() {
  const grid = $("#grid");
  if (!grid) return;
  const list = filteredBooks();
  if (!list.length) { grid.innerHTML = `<p class="caption">${esc(T.emptyFilter)}</p>`; return; }
  grid.innerHTML = list.map((b) => `<div class="book-card-wrap">
      <button class="book-card" data-book="${esc(b.id)}" title="${esc(T.read)}">
        <div class="book-cover">${coverHtml(b)}${b.busy ? `<span class="cover-badge">${esc(T.translating)}</span>` : ""}</div>
        <h3 class="book-title">${esc(b.title)}</h3>
        <p class="book-author">${esc(b.author)}</p>
        <div class="book-details"><span>${esc(T.pagesN(b.pages))}</span>
          ${Object.keys(b.translated || {}).map((l) => `<span class="language-badge">${esc(l)}</span>`).join("")}
          <span class="vis-badge ${b.visibility}">${esc(b.visibility === "shared" ? T.shared : T.private)}</span></div>
        ${bookStatus(b)}
      </button>
      <button class="card-action" data-translate-book="${esc(b.id)}">${esc(T.translateAction)}</button>
    </div>`).join("");
  applyWidths(grid);
  for (const c of $$("[data-book]", grid)) c.onclick = () => openReader(state.books.find((b) => b.id === c.dataset.book));
  for (const c of $$("[data-translate-book]", grid)) c.onclick = () => go("book", { id: c.dataset.translateBook });
}

/** Upload with a progress bar (fetch cannot report upload progress, XMLHttpRequest can). */
function uploadOne(file, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/books");
    xhr.setRequestHeader("X-CSRF-Token", state.csrf || "");
    xhr.setRequestHeader("X-Filename", encodeURIComponent(file.name));
    xhr.setRequestHeader("Content-Type", "application/pdf");
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(Math.round((100 * e.loaded) / e.total));
    xhr.onload = () => {
      let body = null;
      try { body = JSON.parse(xhr.responseText); } catch {}
      if (xhr.status >= 200 && xhr.status < 300) resolve(body.book);
      else reject(new ApiError(xhr.status, body || { message: xhr.status === 413 ? "File too large" : `HTTP ${xhr.status}` }));
    };
    xhr.onerror = () => reject(new Error("network error"));
    xhr.send(file);
  });
}

async function uploadFiles(files) {
  const pdfs = files.filter((f) => f.type === "application/pdf" || f.name.toLowerCase().endsWith(".pdf"));
  if (!pdfs.length) return;
  const bar = $("#upload-progress");
  let added = [];
  for (const f of pdfs) {
    if (bar) {
      bar.hidden = false;
      $("#upload-text").textContent = T.uploading(f.name, 0);
    }
    try {
      added.push(await uploadOne(f, (p) => {
        if ($("#upload-text")) $("#upload-text").textContent = T.uploading(f.name, p);
        if ($("#upload-meter")) $("#upload-meter").style.width = `${p}%`;
      }));
    } catch (e) {
      toast(`${f.name}: ${e.message}`, true);
    }
  }
  if (added.length) toast(T.added(added.length));
  if (added.length === 1 && pdfs.length === 1) openReader(added[0]);
  else go("library");
}

/* ============================== book (translate) ============================== */

let bookForm = { mode: "range", first: 1, last: 5 };

async function renderBook(id) {
  await refreshCounts();
  const book = (await api("GET", `/api/books/${encodeURIComponent(id)}`)).book;
  setCrumbs(T.navLibrary, book.title, T.translateAction);
  if (bookForm.bookId !== id) bookForm = { mode: "range", first: 1, last: Math.min(book.pages, 5), bookId: id, lang: state.settings.target_lang };
  const editable = book.editable;
  main.innerHTML = `
    <button class="back-link" id="back"><svg viewBox="0 0 24 24"><path d="M15 18l-6-6 6-6"/></svg>${esc(T.navLibrary)}</button>
    <div class="book-head">
      <div class="thumb">${coverHtml(book)}</div>
      <div class="head-text"><h1><span class="title-text">${esc(book.title)}</span><span class="count">${esc(T.pagesN(book.pages))}</span></h1>
        <p>${esc(book.author)}${book.mine ? "" : ` · ${esc(T.by(book.owner))}`}</p></div>
      <button class="primary-button big" id="b-read">${esc(T.read)}</button>
    </div>
    <div class="book-layout">
      <div>
        <section class="card stack">
          <div class="section-heading"><h2>${esc(T.startTitle)}</h2></div>
          <label>${esc(T.targetLang)}<input id="b-target" list="languages" value="${esc(bookForm.lang)}"></label>
          <div class="radio-row" id="b-mode"><button data-mode="range">${esc(T.mRange)}</button><button data-mode="missing">${esc(T.mMissing)}</button><button data-mode="all">${esc(T.mAll)}</button></div>
          <div class="row" id="b-range">
            <label>${esc(T.firstPage)}<input id="b-first" type="number" min="1" max="${book.pages}" value="${bookForm.first}"></label>
            <label>${esc(T.lastPage)}<input id="b-last" type="number" min="1" max="${book.pages}" value="${bookForm.last}"></label>
          </div>
          <button class="primary-button wide" id="b-enqueue">${esc(T.enqueue)}</button>
        </section>
        <section class="card stack">
          <div class="section-heading"><h2>${esc(T.detailsTitle)}</h2></div>
          ${editable ? "" : `<p class="hint">${esc(T.readOnly)}</p>`}
          <label>${esc(T.bookTitle)}<input id="b-title" value="${esc(book.title)}" ${editable ? "" : "disabled"}></label>
          <label>${esc(T.author)}<input id="b-author" value="${esc(book.author)}" ${editable ? "" : "disabled"}></label>
          <label>${esc(T.sourceLang)}<input id="b-source" list="languages" placeholder="${esc(T.auto)}" value="${esc(book.source_lang)}" ${editable ? "" : "disabled"}></label>
          <label>${esc(T.subject)}<textarea id="b-subject" rows="3" ${editable ? "" : "disabled"}>${esc(book.subject)}</textarea></label>
          <p class="hint">${esc(T.subjectHint)}</p>
          ${book.mine ? `<label>${esc(T.visibility)}<select id="b-vis"><option value="private">${esc(T.visPrivate)}</option><option value="shared">${esc(T.visShared)}</option></select></label><p class="hint">${esc(T.visHint)}</p>` : ""}
          ${editable ? `<div class="form-actions"><button class="quiet-button" id="b-save">${esc(T.save)}</button></div>` : ""}
        </section>
      </div>
      <div>
        <section class="card" id="b-translations"></section>
        <section class="card" id="b-history"></section>
        ${editable ? `<div class="danger-zone"><div><b>${esc(T.deleteTitle)}</b><p class="hint">${esc(T.deleteText)}</p></div><button class="quiet-button danger" id="b-delete">${esc(T.deleteButton)}</button></div>` : ""}
      </div>
    </div>`;
  $("#back").onclick = () => go("library");
  $("#b-read").onclick = () => openReader(book);
  if ($("#b-vis")) $("#b-vis").value = book.visibility;
  const setMode = (m) => {
    bookForm.mode = m;
    for (const b of $$("#b-mode button")) b.setAttribute("aria-pressed", String(b.dataset.mode === m));
    $("#b-range").hidden = m !== "range";
  };
  for (const b of $$("#b-mode button")) b.onclick = () => setMode(b.dataset.mode);
  setMode(bookForm.mode);
  $("#b-first").oninput = () => (bookForm.first = Number($("#b-first").value));
  $("#b-last").oninput = () => (bookForm.last = Number($("#b-last").value));
  $("#b-target").oninput = () => { bookForm.lang = $("#b-target").value.trim().toLowerCase(); renderBookStatus(book); };
  $("#b-enqueue").onclick = async () => {
    const lang = $("#b-target").value.trim().toLowerCase();
    const done = new Set((await api("GET", `/api/books/${book.id}/pages/${encodeURIComponent(lang)}`).catch(() => ({ pages: [] }))).pages.map(([p]) => p));
    const all = Array.from({ length: book.pages }, (_, i) => i + 1);
    let pages;
    if (bookForm.mode === "all") pages = all;
    else if (bookForm.mode === "missing") pages = all.filter((p) => !done.has(p));
    else {
      const a = Math.max(1, Math.min(book.pages, Number($("#b-first").value) || 1));
      const b = Math.max(a, Math.min(book.pages, Number($("#b-last").value) || a));
      pages = all.slice(a - 1, b);
    }
    if (!pages.length) return toast(T.nothingToDo);
    await apiCall("POST", `/api/books/${book.id}/jobs`, { lang, pages });
    toast(T.enqueued);
    poll(true);
    renderBookStatus(book);
  };
  if ($("#b-save")) $("#b-save").onclick = async () => {
    const body = { title: $("#b-title").value, author: $("#b-author").value, subject: $("#b-subject").value, source_lang: $("#b-source").value };
    if ($("#b-vis")) body.visibility = $("#b-vis").value;
    await apiCall("PATCH", `/api/books/${book.id}`, body);
    toast(T.saved);
    renderBook(book.id);
  };
  if ($("#b-delete")) $("#b-delete").onclick = async () => {
    if (!confirm(T.confirmDelete(book.title))) return;
    await apiCall("DELETE", `/api/books/${book.id}`);
    toast(T.deleted);
    go("library");
  };
  renderBookStatus(book);
}

async function renderBookStatus(book) {
  if (state.route.view !== "book" || state.route.id !== book.id) return;
  const fresh = (await api("GET", `/api/books/${book.id}`)).book;
  const langs = Object.keys(fresh.translated || {});
  const box = $("#b-translations");
  if (!box) return;
  box.innerHTML = `<div class="section-heading"><h2>${esc(T.translationsTitle)}</h2></div>` + (langs.length
    ? langs.map((l) => `<div class="lang-row"><span class="lang-name">${esc(langName(l))}</span>
        <span class="lang-meta">${esc(T.pagesDone(fresh.translated[l], fresh.pages))}<div class="meter"><span class="w" data-w="${(100 * fresh.translated[l]) / fresh.pages}"></span></div></span>
        <span class="lang-actions"><button class="quiet-button" data-read="${esc(l)}">${esc(T.read)}</button><a class="quiet-button" href="${esc(bookUrl(book.id, l))}" download>${esc(T.download)}</a></span></div>`).join("")
    : `<p class="caption">${esc(T.noTranslations)}</p>`);
  applyWidths(box);
  for (const b of $$("[data-read]", box)) b.onclick = () => go("reader", { id: book.id, lang: b.dataset.read, page: readingPrefs(book.id).page || 1 });
  const jobs = state.jobs.filter((j) => j.book_id === book.id).slice(0, 8);
  const hist = $("#b-history");
  hist.innerHTML = `<div class="section-heading"><h2>${esc(T.historyTitle)}</h2></div>` +
    (jobs.length ? `<div class="job-list">${jobs.map((j) => jobRow(j, false)).join("")}</div>` : `<p class="caption">${esc(T.noHistory)}</p>`);
  applyWidths(hist);
  bindJobActions(hist);
}

/* ============================== jobs ============================== */

function jobRow(job, showBook = true) {
  const pct = job.total_pages ? (100 * job.done_pages) / job.total_pages : 0;
  const sub = job.error
    ? `<p class="error">${esc(job.error)}</p>`
    : `<p>${esc(langName(job.lang))} · ${esc(T.pagesN(job.total_pages))} · ${esc(fmtDate(job.created_at))} · ${esc(job.by)}${job.tokens ? ` · ${esc(T.tokens(job.tokens))}` : ""}</p>`;
  const actions = [];
  if (job.status === "queued" || job.status === "running") actions.push(`<button class="quiet-button" data-cancel="${esc(job.id)}">${esc(T.cancel)}</button>`);
  if (job.status === "failed" || job.status === "cancelled") actions.push(`<button class="quiet-button" data-retry="${esc(job.id)}">${esc(T.retry)}</button>`);
  if (showBook) actions.push(`<button class="quiet-button" data-open="${esc(job.book_id)}">${esc(T.openBook)}</button>`);
  return `<div class="job-row">
    <div class="job-main"><h3>${esc(showBook ? job.book_title : `${T.pagesN(job.total_pages)} · ${langName(job.lang)}`)}</h3>${sub}</div>
    <span><span class="chip ${esc(job.status)}">${esc(T["st_" + job.status])}</span></span>
    <div class="job-progress"><span>${job.done_pages}/${job.total_pages}</span><div class="meter"><span class="w" data-w="${pct}"></span></div></div>
    <div class="job-actions">${actions.join("")}</div>
  </div>`;
}

function bindJobActions(root) {
  for (const b of $$("[data-cancel]", root)) b.onclick = async () => { await apiCall("POST", `/api/jobs/${b.dataset.cancel}/cancel`, {}); poll(true); };
  for (const b of $$("[data-retry]", root)) b.onclick = async () => { await apiCall("POST", `/api/jobs/${b.dataset.retry}/retry`, {}); poll(true); };
  for (const b of $$("[data-open]", root)) b.onclick = () => go("book", { id: b.dataset.open });
}

async function renderJobs() {
  await refreshCounts();
  main.innerHTML = `<div class="page-heading"><h1>${esc(T.jobsTitle)}<span class="count">${state.jobs.length}</span></h1></div>` +
    (state.jobs.length ? `<section class="card"><div class="job-list">${state.jobs.map((j) => jobRow(j)).join("")}</div></section>`
      : `<div class="empty-results"><span class="hero-logo"></span><p>${esc(T.noJobs)}</p></div>`);
  applyWidths();
  bindJobActions(main);
}

/* Job progress: the server does not push, so the page asks while something is running. */
let pollTimer;
let lastJobs = new Map();
async function poll(soon = false) {
  clearTimeout(pollTimer);
  if (!state.me) return;
  if (!soon) {
    try {
      await refreshCounts();
    } catch { return; }
    let finished = false;
    for (const j of state.jobs) {
      const before = lastJobs.get(j.id);
      if (before && (before.status !== j.status || before.done_pages !== j.done_pages)) {
        emit("job-progress", { job_id: j.id, book_id: j.book_id, done: j.done_pages, total: j.total_pages, page: 0 });
        if (before.status !== j.status) emit("job-updated", j);
        if (j.status === "done") finished = true;
      }
    }
    lastJobs = new Map(state.jobs.map((j) => [j.id, j]));
    if (finished && state.route.view === "library") drawGrid();
    if (state.route.view === "jobs") renderJobs();
    if (state.route.view === "book") renderBookStatus({ id: state.route.id });
  }
  const active = state.jobs.some((j) => j.status === "queued" || j.status === "running");
  pollTimer = setTimeout(() => poll(), soon ? 800 : active ? 3000 : 20000);
}

/* ============================== account ============================== */

async function renderAccount() {
  const me = (await api("GET", "/api/auth/me")).user;
  state.me = me;
  main.innerHTML = `<div class="page-heading"><h1>${esc(T.accountTitle)}</h1></div>
    <div class="account-grid">
      <section class="card stack">
        <div class="section-heading"><h2>${esc(T.profile)}</h2></div>
        <div class="profile-row"><span class="avatar big">${esc(initials(me.name))}</span>
          <dl class="kv"><dt>${esc(T.name)}</dt><dd>${esc(me.name)}</dd><dt>${esc(T.email)}</dt><dd>${esc(me.email)}</dd><dt>${esc(T.role)}</dt><dd>${esc(T.roles[me.role])}</dd></dl></div>
        <label>${esc(T.uiLang)}<select id="ui-lang"><option value="system">${esc(T.system)}</option>${UI_LANGUAGES.map(([c, n]) => `<option value="${c}">${esc(n)}</option>`).join("")}</select></label>
      </section>
      <section class="card stack" id="acc-password">
        <div class="section-heading"><h2>${esc(T.changePassword)}</h2></div>
        <form class="stack" id="pw-form" autocomplete="on">
          <input type="email" autocomplete="username" value="${esc(me.email)}" hidden>
          <label>${esc(T.currentPassword)}<input id="pw-current" type="password" autocomplete="current-password" required></label>
          <label>${esc(T.newPassword)}<input id="pw-new" type="password" autocomplete="new-password" required><small class="hint">${esc(T.passwordHint)}</small></label>
          <div class="form-actions"><button class="primary-button" type="submit">${esc(T.save)}</button></div>
        </form>
      </section>
      <section class="card stack" id="acc-two-step"></section>
      <section class="card stack" id="acc-sessions"></section>
      <section class="card stack wide-card" id="acc-keys"></section>
    </div>`;
  $("#pw-form").onsubmit = async (e) => {
    e.preventDefault();
    const r = await apiCall("POST", "/api/auth/password", { current: $("#pw-current").value, new: $("#pw-new").value });
    toast(T.passwordChanged(r.sessions_ended));
    $("#pw-form").reset();
  };
  try { $("#ui-lang").value = localStorage.getItem("celsus-ui-lang") || "system"; } catch {}
  $("#ui-lang").onchange = (e) => {
    try {
      if (e.target.value === "system") localStorage.removeItem("celsus-ui-lang");
      else localStorage.setItem("celsus-ui-lang", e.target.value);
    } catch {}
    location.reload();
  };
  drawTwoStep(me);
  drawSessions();
  drawKeys(me);
}

function drawTwoStep(me) {
  const box = $("#acc-two-step");
  box.innerHTML = `<div class="section-heading"><h2>${esc(T.twoStep)}</h2><span class="chip ${me.two_step ? "done" : ""}">${esc(me.two_step ? T.twoStepOn : T.twoStepOff)}</span></div>
    <p class="hint">${esc(me.two_step ? T.disableText : T.twoStepIntro)}</p>
    ${me.two_step
      ? `<form class="stack" id="ts-off"><label>${esc(T.password)}<input id="ts-pw" type="password" autocomplete="current-password" required></label>
          <label>${esc(T.code)}<input id="ts-code" autocomplete="one-time-code" required></label>
          <div class="form-actions"><button class="quiet-button danger" type="submit">${esc(T.turnOff)}</button></div></form>`
      : `<div class="form-actions"><button class="primary-button" id="ts-start">${esc(T.turnOn)}</button></div><div id="ts-setup"></div>`}`;
  if (me.two_step) {
    $("#ts-off").onsubmit = async (e) => {
      e.preventDefault();
      await apiCall("POST", "/api/auth/two-step/disable", { password: $("#ts-pw").value, code: $("#ts-code").value });
      renderAccount();
    };
    return;
  }
  $("#ts-start").onclick = async () => {
    const s = await apiCall("POST", "/api/auth/two-step/setup", {});
    // The QR code is an SVG the server drew; shown as an image so it cannot run anything.
    const qr = `data:image/svg+xml;base64,${btoa(s.qr_svg)}`;
    $("#ts-start").hidden = true;
    $("#ts-setup").innerHTML = `<div class="qr-box"><img alt="QR" class="qr" src="${esc(qr)}"><div class="stack">
        <p class="hint">${esc(T.scanQr)}</p><p class="hint">${esc(T.orKey)}</p><code class="secret">${esc(s.secret.replace(/(.{4})/g, "$1 ").trim())}</code>
        <form class="row" id="ts-enable"><label>${esc(T.code)}<input id="ts-new-code" inputmode="numeric" autocomplete="one-time-code" required></label>
        <button class="primary-button" type="submit">${esc(T.verify)}</button></form></div></div>`;
    $("#ts-new-code").focus();
    $("#ts-enable").onsubmit = async (e) => {
      e.preventDefault();
      const r = await apiCall("POST", "/api/auth/two-step/enable", { code: $("#ts-new-code").value });
      $("#ts-setup").innerHTML = `<div class="backup-codes"><h3>${esc(T.backupTitle)}</h3><p class="hint">${esc(T.backupText)}</p>
        <ul>${r.backup_codes.map((c) => `<li><code>${esc(c)}</code></li>`).join("")}</ul>
        <div class="form-actions"><button class="quiet-button" id="bc-copy">${esc(T.copyCodes)}</button><button class="primary-button" id="bc-done">${esc(T.backupDone)}</button></div></div>`;
      $("#bc-copy").onclick = async () => { await navigator.clipboard.writeText(r.backup_codes.join("\n")); toast(T.copied); };
      $("#bc-done").onclick = () => renderAccount();
    };
  };
}

async function drawSessions() {
  const box = $("#acc-sessions");
  const { sessions } = await api("GET", "/api/auth/sessions");
  box.innerHTML = `<div class="section-heading"><h2>${esc(T.sessionsTitle)}</h2></div>
    <div class="list">${sessions.map((s) => `<div class="list-row"><div><b>${esc(shortAgent(s.user_agent))}${s.current ? ` <span class="chip done">${esc(T.thisDevice)}</span>` : ""}</b>
      <small>${esc(s.ip)} · ${esc(T.lastSeen)} ${esc(fmtDate(s.last_seen))}</small></div>
      ${s.current ? "" : `<button class="text-button danger" data-end="${esc(s.id)}">${esc(T.endSession)}</button>`}</div>`).join("")}</div>
    ${sessions.length > 1 ? `<div class="form-actions"><button class="quiet-button" id="end-others">${esc(T.endOthers)}</button></div>` : ""}`;
  for (const b of $$("[data-end]", box)) b.onclick = async () => { await apiCall("DELETE", `/api/auth/sessions/${encodeURIComponent(b.dataset.end)}`); drawSessions(); };
  if ($("#end-others")) $("#end-others").onclick = async () => { const r = await apiCall("POST", "/api/auth/sessions/end-others", {}); toast(T.ended(r.ended)); drawSessions(); };
}

function shortAgent(ua) {
  const browser = /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : ua ? ua.split(" ")[0] : "?";
  const os = /Windows/.test(ua) ? "Windows" : /Android/.test(ua) ? "Android" : /iPhone|iPad/.test(ua) ? "iOS" : /Mac OS/.test(ua) ? "macOS" : /Linux/.test(ua) ? "Linux" : "";
  return os ? `${browser} · ${os}` : browser;
}

async function drawKeys(me) {
  const box = $("#acc-keys");
  const { keys } = await api("GET", "/api/keys");
  const isAdmin = me.role === "owner" || me.role === "admin";
  box.innerHTML = `<div class="section-heading"><h2>${esc(T.keysTitle)}</h2></div><p class="hint">${esc(T.keysText)}</p>
    <div class="list">${keys.length ? keys.map((k) => `<div class="list-row"><div><b>${esc(k.name)}</b>
      <small>${k.scopes.map(esc).join(", ")} · ${esc(k.last_used ? fmtDate(k.last_used) : T.neverUsed)}${k.expires_at ? ` · → ${esc(fmtDate(k.expires_at))}` : ""}</small></div>
      <button class="text-button danger" data-revoke="${esc(k.id)}">${esc(T.revoke)}</button></div>`).join("") : `<p class="caption">${esc(T.noKeys)}</p>`}</div>
    <form class="key-form" id="key-form">
      <label>${esc(T.keyName)}<input id="k-name" required maxlength="100"></label>
      <fieldset class="scopes"><legend>${esc(T.scopes)}</legend>
        <label class="check"><input type="checkbox" value="read" checked>${esc(T.scopeRead)}</label>
        <label class="check"><input type="checkbox" value="translate">${esc(T.scopeTranslate)}</label>
        ${isAdmin ? `<label class="check"><input type="checkbox" value="admin">${esc(T.scopeAdmin)}</label>` : ""}</fieldset>
      <label>${esc(T.expires)}<input id="k-days" type="number" min="1" max="3650"></label>
      <div class="form-actions"><button class="primary-button" type="submit">${esc(T.createKey)}</button></div>
    </form><div id="k-new"></div>`;
  for (const b of $$("[data-revoke]", box)) b.onclick = async () => { await apiCall("DELETE", `/api/keys/${encodeURIComponent(b.dataset.revoke)}`); drawKeys(me); };
  $("#key-form").onsubmit = async (e) => {
    e.preventDefault();
    const scopes = $$(".scopes input:checked", box).map((i) => i.value);
    const days = Number($("#k-days").value) || null;
    const r = await apiCall("POST", "/api/keys", { name: $("#k-name").value, scopes, expires_in_days: days });
    await drawKeys(me);
    $("#k-new").innerHTML = `<div class="new-key"><p class="hint">${esc(T.newKey)}</p><code class="secret">${esc(r.key)}</code><button class="quiet-button" id="k-copy">${esc(T.copyCodes.split(" ")[0])}</button></div>`;
    $("#k-copy").onclick = async () => { await navigator.clipboard.writeText(r.key); toast(T.copied); };
  };
}

/* ============================== admin ============================== */

async function renderAdmin(tab = "members") {
  if (!(state.me?.role === "owner" || state.me?.role === "admin")) return go("library");
  const tabs = [["members", T.tMembers], ["connections", T.tConnections], ["audit", T.tAudit]];
  main.innerHTML = `<div class="page-heading"><h1>${esc(T.adminTitle)}</h1></div>
    <div class="settings-layout"><nav class="settings-tabs">${tabs.map(([k, l]) => `<button data-tab="${k}" class="${k === tab ? "active" : ""}">${esc(l)}</button>`).join("")}</nav><div id="tab"></div></div>`;
  for (const b of $$("[data-tab]")) b.onclick = () => go("admin", { tab: b.dataset.tab });
  const box = $("#tab");
  if (tab === "members") await adminMembers(box);
  if (tab === "connections") await adminConnections(box);
  if (tab === "audit") await adminAudit(box);
}

async function adminMembers(box) {
  const { users } = await api("GET", "/api/admin/users");
  const owner = state.me.role === "owner";
  box.innerHTML = `<section class="card stack">
      <div class="section-heading"><h2>${esc(T.tMembers)}</h2><span class="caption">${users.length}</span></div>
      <div class="list">${users.map((u) => {
        const self = u.id === state.me.id;
        const canTouch = !self && u.role !== "owner" && (owner || u.role === "member");
        return `<div class="list-row"><div><b>${esc(u.name)}${self ? ` <span class="caption">(${esc(T.you)})</span>` : ""}</b>
          <small>${esc(u.email)} · ${esc(T.roles[u.role])}${u.two_step ? ` · ${esc(T.twoStepShort)}` : ""}${u.disabled ? ` · ${esc(T.disabled)}` : ""} · ${esc(T.lastSeen)} ${esc(u.last_seen ? fmtDate(u.last_seen) : T.never)}</small></div>
          ${canTouch ? `<span class="row-actions">
            ${owner ? `<button class="quiet-button" data-role="${u.id}" data-to="${u.role === "admin" ? "member" : "admin"}">${esc(u.role === "admin" ? T.makeMember : T.makeAdmin)}</button>` : ""}
            <button class="quiet-button ${u.disabled ? "" : "danger"}" data-disable="${u.id}" data-to="${u.disabled ? "false" : "true"}">${esc(u.disabled ? T.enable : T.disable)}</button></span>` : ""}</div>`;
      }).join("")}</div>
    </section>
    <section class="card stack">
      <div class="section-heading"><h2>${esc(T.invite)}</h2></div>
      <div class="row">${owner ? `<label>${esc(T.inviteAs)}<select id="inv-role"><option value="member">${esc(T.roles.member)}</option><option value="admin">${esc(T.roles.admin)}</option></select></label>` : ""}
        <button class="primary-button" id="inv-make">${esc(T.invite)}</button></div>
      <div id="inv-out"></div>
    </section>`;
  for (const b of $$("[data-role]", box)) b.onclick = async () => { await apiCall("PATCH", `/api/admin/users/${b.dataset.role}`, { role: b.dataset.to }); adminMembers(box); };
  for (const b of $$("[data-disable]", box)) b.onclick = async () => { await apiCall("PATCH", `/api/admin/users/${b.dataset.disable}`, { disabled: b.dataset.to === "true" }); adminMembers(box); };
  $("#inv-make").onclick = async () => {
    const r = await apiCall("POST", "/api/admin/invites", { role: $("#inv-role")?.value || "member" });
    const link = `${location.origin}/#/invite/${encodeURIComponent(r.token)}`;
    $("#inv-out").innerHTML = `<div class="new-key"><p class="hint">${esc(T.inviteLink)}</p><code class="secret">${esc(link)}</code><button class="quiet-button" id="inv-copy">${esc(T.copyCodes.split(" ")[0])}</button></div>`;
    $("#inv-copy").onclick = async () => { await navigator.clipboard.writeText(link); toast(T.copied); };
  };
}

async function adminConnections(box) {
  const { providers, models } = await api("GET", "/api/admin/providers");
  const tasks = [["translate", T.taskTranslate], ["metadata", T.taskMetadata], ["image", T.taskImage]];
  box.innerHTML = `<section class="card stack">
      <div class="section-heading"><h2>${esc(T.tConnections)}</h2><button class="primary-button" id="p-add">${esc(T.addConnection)}</button></div>
      <p class="hint">${esc(T.providersText)}</p>
      <div class="list">${providers.map((p) => `<div class="list-row"><div><b>${esc(p.name)}</b><small>${esc(p.url)} · ${esc(p.has_key ? T.hasKey : T.noKey)}${p.allow_local ? ` · ${esc(T.localNet)}` : ""}</small></div>
        <span class="row-actions"><button class="quiet-button" data-edit="${p.id}">${esc(T.edit)}</button><button class="text-button danger" data-remove="${p.id}">${esc(T.remove)}</button></span></div>`).join("")}</div>
      <form class="stack" id="p-form" hidden></form>
    </section>
    <section class="card stack">
      <div class="section-heading"><h2>${esc(T.tasksTitle)}</h2></div>
      ${tasks.map(([k, label]) => {
        const m = models.find((x) => x.task === k) || {};
        return `<div class="task-row" data-task="${k}"><div class="task-head"><h3>${esc(label)}</h3></div>
          <div class="task-fields"><select data-conn><option value="">${esc(T.none)}</option>${providers.map((p) => `<option value="${p.id}" ${m.provider_id === p.id ? "selected" : ""}>${esc(p.name)}</option>`).join("")}</select>
          <input data-model list="dl-${k}" placeholder="${esc(T.model)}" value="${esc(m.model || "")}"><datalist id="dl-${k}"></datalist>
          <button class="quiet-button" data-fetch>${esc(T.fetchModels)}</button><button class="primary-button" data-save>${esc(T.save)}</button></div></div>`;
      }).join("")}
    </section>`;
  $("#p-add").onclick = () => providerForm(box, { id: 0, name: "", url: "https://api.openai.com/v1", has_key: false, allow_local: false });
  for (const b of $$("[data-edit]", box)) b.onclick = () => providerForm(box, providers.find((p) => p.id === Number(b.dataset.edit)));
  for (const b of $$("[data-remove]", box)) b.onclick = async () => {
    const p = providers.find((x) => x.id === Number(b.dataset.remove));
    if (!confirm(`${T.remove}: ${p.name}?`)) return;
    await apiCall("DELETE", `/api/admin/providers/${p.id}`);
    adminConnections(box);
  };
  for (const row of $$(".task-row", box)) {
    const conn = $("[data-conn]", row), input = $("[data-model]", row);
    $("[data-fetch]", row).onclick = async () => {
      if (!conn.value) return;
      const { models: list } = await apiCall("GET", `/api/admin/providers/${conn.value}/models`);
      $("datalist", row).innerHTML = list.map((m) => `<option value="${esc(m)}">`).join("");
      toast(T.modelsFound(list.length));
    };
    $("[data-save]", row).onclick = async () => {
      await apiCall("PUT", "/api/admin/models", { task: row.dataset.task, provider_id: conn.value ? Number(conn.value) : null, model: input.value });
      toast(T.saved);
    };
  }
}

function providerForm(box, p) {
  const form = $("#p-form", box);
  form.hidden = false;
  form.innerHTML = `<label>${esc(T.name)}<input id="pf-name" value="${esc(p.name)}" required maxlength="100"></label>
    <label>${esc(T.url)}<input id="pf-url" value="${esc(p.url)}" required></label>
    <label>${esc(T.apiKey)}<input id="pf-key" type="password" autocomplete="off" placeholder="${esc(p.has_key ? T.keyKeep : "")}"></label>
    <label class="check"><input id="pf-local" type="checkbox" ${p.allow_local ? "checked" : ""}>${esc(T.localNet)}</label>
    <p class="hint">${esc(T.localNetHint)}</p>
    <div class="form-actions"><button class="quiet-button" type="button" id="pf-cancel">${esc(T.cancelEdit)}</button><button class="primary-button" type="submit">${esc(T.save)}</button></div>`;
  $("#pf-name").focus();
  $("#pf-cancel").onclick = () => (form.hidden = true);
  form.onsubmit = async (e) => {
    e.preventDefault();
    const body = { name: $("#pf-name").value, url: $("#pf-url").value, allow_local: $("#pf-local").checked };
    const key = $("#pf-key").value.trim();
    if (key) body.key = key;
    if (p.id) await apiCall("PATCH", `/api/admin/providers/${p.id}`, body);
    else await apiCall("POST", "/api/admin/providers", body);
    toast(T.saved);
    adminConnections(box);
  };
}

async function adminAudit(box, before) {
  const { entries } = await api("GET", `/api/admin/audit?limit=100${before ? `&before=${before}` : ""}`);
  const rows = entries.map((e) => `<tr><td>${esc(fmtDate(e.at))}</td><td>${esc(e.user || "—")}</td><td><code>${esc(e.event)}</code>${e.target ? ` <small>${esc(e.target)}</small>` : ""}</td><td>${esc(e.ip)}</td></tr>`).join("");
  if (!before) {
    box.innerHTML = `<section class="card"><table class="audit"><thead><tr><th>${esc(T.when)}</th><th>${esc(T.who)}</th><th>${esc(T.what)}</th><th>${esc(T.where)}</th></tr></thead><tbody id="audit-body"></tbody></table>
      <div class="form-actions"><button class="quiet-button" id="audit-more">${esc(T.more)}</button></div></section>`;
  }
  $("#audit-body").insertAdjacentHTML("beforeend", rows);
  const more = $("#audit-more");
  more.hidden = entries.length < 100;
  more.onclick = () => adminAudit(box, entries[entries.length - 1].id);
}

/* ============================== start ============================== */

async function startApp() {
  $("#gate").hidden = true;
  $("#app").hidden = false;
  const me = state.me;
  $("#profile-name").textContent = me.name;
  $("#profile-role").textContent = T.roles[me.role];
  $("#avatar").textContent = initials(me.name);
  $("#nav-admin").hidden = !(me.role === "owner" || me.role === "admin");
  route(hashToRoute());
  poll();
}


for (const b of $$("[data-go]")) b.onclick = () => go(b.dataset.go);
for (const b of $$("[data-theme-choice]")) b.onclick = () => setTheme(b.dataset.themeChoice);
$("#sign-out").onclick = async () => {
  try { await api("POST", "/api/auth/logout", {}); } catch {}
  state.me = null;
  state.csrf = null;
  location.hash = "";
  showGate();
};
function fillLanguages() {
  $("#languages").innerHTML = LANGUAGES.map(([c, n, en]) =>
    `<option value="${esc(c)}">${esc(en && en !== n ? `${n} · ${en}` : n)}</option>`).join("");
}
fillLanguages();
api("GET", "/api/languages").then((list) => {
  LANGUAGES = list.map((l) => [l.code, l.native, l.english]);
  fillLanguages();
}).catch(() => {});
applyStrings();
setTheme((() => { try { return localStorage.getItem("celsus-theme") || "dark"; } catch { return "dark"; } })());

// Drop PDFs anywhere to add them.
let dragDepth = 0;
window.addEventListener("dragenter", (e) => { if (state.me && e.dataTransfer?.types.includes("Files")) { dragDepth++; $("#drop-overlay").hidden = false; } });
window.addEventListener("dragleave", () => { if (--dragDepth <= 0) { dragDepth = 0; $("#drop-overlay").hidden = true; } });
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  $("#drop-overlay").hidden = true;
  if (state.me) uploadFiles([...(e.dataTransfer?.files || [])]);
});

(async () => {
  try {
    const r = await api("GET", "/api/auth/me");
    state.me = r.user;
    state.csrf = r.csrf;
    startApp();
  } catch {
    showGate();
  }
})();
