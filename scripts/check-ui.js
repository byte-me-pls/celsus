// Checks the scripts of an interface the way its page loads them: all in one scope. A name declared in two of
// them (a `const` in one file, a function of the same name in another) passes when each file is checked alone,
// and stops the second file from loading at all in the app.
// Usage: node scripts/check-ui.js [--shared DIR] page.html...
// --shared names a folder to look in for scripts that are not beside the page (the web edition is served the
// desktop interface's reader, model list and languages).
const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync } = require("child_process");

let failed = false;
const args = process.argv.slice(2);
const shared = [];
while (args[0] === "--shared") shared.push(args.splice(0, 2)[1]);
for (const page of args) {
  const dir = path.dirname(page);
  const html = fs.readFileSync(page, "utf8");
  const files = [...html.matchAll(/<script[^>]*\ssrc="([^"]+)"[^>]*><\/script>/g)]
    .map((m) => m[1].split("?")[0])
    .filter((src) => !/^https?:/.test(src))
    .map((src) => {
      const found = [dir, ...shared].map((d) => path.join(d, src)).find((f) => fs.existsSync(f));
      if (!found) { failed = true; console.log(page + ": " + src + " is not there"); }
      return found;
    })
    .filter(Boolean);
  const together = path.join(os.tmpdir(), `celsus-ui-${process.pid}.js`);
  fs.writeFileSync(together, files.map((f) => fs.readFileSync(f, "utf8")).join("\n;\n"));
  const r = spawnSync(process.execPath, ["--check", together], { encoding: "utf8" });
  fs.rmSync(together, { force: true });
  if (r.status === 0) {
    console.log(`ok: ${page} (${files.length} scripts together)`);
  } else {
    failed = true;
    console.log(`${page}: its scripts do not load together`);
    console.log(r.stderr.split("\n").slice(0, 8).join("\n"));
  }
}
process.exit(failed ? 1 : 0);
