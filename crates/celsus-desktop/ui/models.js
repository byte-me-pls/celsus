/* Choosing a model: the list under a model field, in the desktop app and in the web interface. */
"use strict";

/** Models whose names say they do something other than answer in text: speech, search, safety, pictures. */
const NOT_CHAT = /embed|rerank|retriev|guard|safety|moderation|whisper|parakeet|canary|riva|tts|speech|asr|audio|clip|sdxl|diffusion|flux|image|video|cosmos|bge-|ocr|detect|segment|deplot|parse|reward/i;

/** A model server on this computer: its models are the ones the user installed. */
const LOCAL_URL = /\/\/(localhost|127\.0\.0\.1|\[::1\])[:/]/;

/** How each model did when it was last tried here: "connection:model" -> { ok, ms }. */
const modelTests = (() => { try { return JSON.parse(localStorage.getItem("celsus-model-tests")) || {}; } catch { return {}; } })();
function rememberTest(connection, model, ok, ms) {
  modelTests[`${connection}:${model}`] = { ok, ms };
  try { localStorage.setItem("celsus-model-tests", JSON.stringify(modelTests)); } catch {}
}

/**
 * Try every model of a connection with one short sentence. A connection lists what it offers, not what is there
 * to answer right now, so trying is the only way to tell. One request goes out every second and a half, which
 * stays under the usual limit of forty a minute. `ask(model)` gives { ok, millis, wait }.
 */
async function tryModels(connection, models, ask, onStep) {
  const pause = (ms) => new Promise((r) => setTimeout(r, ms));
  let done = 0, answered = 0;
  const running = new Set();
  for (const model of models) {
    const one = (async () => {
      let r = await ask(model);
      if (!r.ok && r.wait) { await pause(Math.min(r.wait, 60) * 1000); r = await ask(model); }
      rememberTest(connection, model, r.ok, r.millis);
      if (r.ok) answered++;
      onStep(++done, answered);
    })().finally(() => running.delete(one));
    running.add(one);
    await pause(1500);
    if (running.size >= 6) await Promise.race(running);
  }
  await Promise.all(running);
  // Under the empty name: the connection's models have all been tried.
  rememberTest(connection, "", answered > 0, 0);
  return answered;
}

/**
 * The list under a model field: the connection's models under the names of their makers, narrowed by what is
 * typed, each with how it did when it was last tried. `models()` gives [shown, all].
 */
function modelPicker(input, list, models, connection, onPick) {
  let all = false;
  const draw = () => {
    const [usual, every] = models();
    const typed = input.value.trim().toLowerCase();
    // What is in the field when the list opens is the model in use, not a search.
    const query = every.includes(input.value.trim()) ? "" : typed;
    const shown = (all ? every : usual).filter((m) => m.toLowerCase().includes(query));
    const groups = new Map();
    for (const m of shown) {
      const cut = m.indexOf("/");
      const maker = cut > 0 ? m.slice(0, cut) : "";
      if (!groups.has(maker)) groups.set(maker, []);
      groups.get(maker).push(m);
    }
    const mark = (m) => {
      const t = modelTests[`${connection()}:${m}`];
      return t ? `<small class="${t.ok ? "ok" : "bad"}">${t.ok ? esc(T.testOk(t.ms)) : "✕"}</small>` : "";
    };
    list.innerHTML = [...groups].map(([maker, names]) =>
      `${maker ? `<h4>${esc(maker)}</h4>` : ""}${names.map((m) =>
        `<button type="button" data-m="${esc(m)}" class="${m === input.value.trim() ? "on" : ""}"><span>${esc(maker ? m.slice(maker.length + 1) : m)}</span>${mark(m)}</button>`).join("")}`).join("")
      + (!all && every.length > usual.length ? `<button type="button" class="more" data-all>${esc(T.modelsAll(every.length))}</button>` : "");
    list.hidden = !list.innerHTML;
  };
  input.onfocus = input.oninput = draw;
  input.onblur = () => { list.hidden = true; all = false; };
  input.onkeydown = (e) => { if (e.key === "Escape") list.hidden = true; };
  // Keep the field focused while the list is clicked, so the click lands before the list goes away.
  list.onmousedown = (e) => e.preventDefault();
  list.onclick = (e) => {
    const b = e.target.closest("button");
    if (!b) return;
    if (b.dataset.all !== undefined) { all = true; draw(); return; }
    input.value = b.dataset.m;
    list.hidden = true;
    onPick();
  };
  return () => { if (!list.hidden) draw(); };
}

/**
 * One model field with its list, its "see which ones answer" button and its result line.
 *   connection()        the chosen connection's id, or "" when none is chosen
 *   models()            [the models fit for the job, all the connection's models]
 *   ask(model, id)      try one model of connection `id` quickly: { ok, millis, wait }
 *   canTry()            whether trying them all makes sense for this connection and job: a service that lists
 *                       more than it runs, not models on this computer, and never picture models
 * Gives `refresh()`, to call when the connection or its models change.
 */
function modelField({ input, list, button, out, connection, models, ask, canTry }) {
  let trying = false;
  // Once the connection's models have all been tried, the list is of those that answered.
  const lists = () => {
    const [fit, every] = models();
    const id = connection();
    const answered = fit.filter((m) => modelTests[`${id}:${m}`]?.ok);
    return [canTry() && modelTests[`${id}:`]?.ok && answered.length ? answered : fit, every];
  };
  const redraw = modelPicker(input, list, lists, connection, () => { if (!trying) out.innerHTML = ""; });
  const refresh = () => {
    button.hidden = !canTry() || models()[0].length < 2;
    button.disabled = trying;
  };
  button.onclick = async () => {
    const id = connection(), all = models()[0];
    if (trying || !id || all.length < 2) return;
    trying = true;
    refresh();
    const say = (text, ok) => (out.innerHTML = `<div class="test-result ${ok ? "ok" : ""}">${ok ? `<b>${esc(text)}</b>` : esc(text)}</div>`);
    say(T.modelsTrying(0, all.length));
    const answered = await tryModels(id, all, (model) => ask(model, id), (done) => { say(T.modelsTrying(done, all.length)); redraw(); });
    trying = false;
    refresh();
    say(T.modelsTried(answered, all.length), true);
    redraw();
  };
  refresh();
  return refresh;
}
