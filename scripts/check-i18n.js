// Checks every interface language against en.js: same keys, same function arity, functions run with sample
// arguments, placeholders such as ${n} are kept. Usage: node scripts/check-i18n.js crates/celsus-desktop/ui/i18n [code...]
const fs = require("fs");
const path = require("path");
const vm = require("vm");
const dir = process.argv[2];
const ctx = { LOCALES: {}, console };
ctx.addLocale = (code, parts) => { ctx.LOCALES[code] = parts; };
vm.createContext(ctx);
const files = fs.readdirSync(dir).filter((f) => f.endsWith(".js") && f !== "core.js");
for (const f of files) {
  try { vm.runInContext(fs.readFileSync(path.join(dir, f), "utf8"), ctx, { filename: f }); }
  catch (e) { console.log(`${f}: does not load: ${e.message}`); process.exitCode = 1; }
}
const en = ctx.LOCALES.en;
const wanted = process.argv.slice(3);
const sample = (fn) => fn(...Array.from({ length: fn.length }, (_, i) => [3, 12, 40][i] ?? 1).map((v, i) => (i === 0 && /\.toUpperCase|\$\{l\}|\(l[,)]/.test(fn.toString()) ? "Deutsch" : v)));
let problems = 0;
function compare(code, a, b, where) {
  for (const k of Object.keys(b)) {
    const p = `${code}.${where}${k}`;
    if (!(k in a)) { console.log(`${p}: missing`); problems++; continue; }
    const x = a[k], y = b[k];
    if (typeof y === "function") {
      if (typeof x !== "function") { console.log(`${p}: should be a function`); problems++; continue; }
      if (x.length !== y.length) { console.log(`${p}: takes ${x.length} arguments, English takes ${y.length}`); problems++; }
      try { const out = sample(x); if (typeof out !== "string" || !out.trim()) { console.log(`${p}: returns ${JSON.stringify(out)}`); problems++; } }
      catch (e) { console.log(`${p}: throws ${e.message}`); problems++; }
      const used = (s) => new Set([...s.toString().matchAll(/\$\{([a-z])/g)].map((m) => m[1]));
      for (const v of used(y)) if (!used(x).has(v)) { console.log(`${p}: does not use \${${v}}`); problems++; }
    } else if (y && typeof y === "object" && !Array.isArray(y)) {
      if (!x || typeof x !== "object") { console.log(`${p}: should be an object`); problems++; continue; }
      compare(code, x, y, `${where}${k}.`);
    } else if (typeof y === "boolean") {
      if (x !== y) { console.log(`${p}: should be ${y}`); problems++; }
    } else if (Array.isArray(y)) {
      if (!Array.isArray(x) || x.length !== y.length) { console.log(`${p}: should be a list of ${y.length}`); problems++; }
    } else if (typeof x !== "string" || (y.trim() && !x.trim())) { console.log(`${p}: empty or not text`); problems++; }
  }
  for (const k of Object.keys(a)) if (!(k in b)) { console.log(`${code}.${where}${k}: not in English (typo?)`); problems++; }
}
for (const code of Object.keys(ctx.LOCALES)) {
  if (code === "en" || (wanted.length && !wanted.includes(code))) continue;
  compare(code, ctx.LOCALES[code], en, "");
}
console.log(problems ? `${problems} problems` : `ok: ${Object.keys(ctx.LOCALES).filter((c) => !wanted.length || wanted.includes(c)).join(", ")}`);
if (problems) process.exitCode = 1;
