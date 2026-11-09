/* Interface languages, shared by the desktop app, the reader and the web edition. Every other file in this folder
   adds one language with addLocale; keys missing from a language fall back to English. Loaded before the app. */

/** Languages the interface is available in: code, own name, written right to left. */
const UI_LANGUAGES = [
  ["en", "English"], ["tr", "Türkçe"], ["de", "Deutsch"], ["fr", "Français"], ["es", "Español"], ["it", "Italiano"],
  ["pt", "Português"], ["nl", "Nederlands"], ["pl", "Polski"], ["ru", "Русский"], ["uk", "Українська"],
  ["ar", "العربية", true], ["hi", "हिन्दी"], ["id", "Bahasa Indonesia"], ["zh", "简体中文"], ["ja", "日本語"], ["ko", "한국어"],
];

const LOCALES = {};
function addLocale(code, parts) {
  LOCALES[code] = parts;
}

/** The chosen language, else the first of the system's languages Celsus speaks, else English. */
const UI_LANG = (() => {
  const known = (code) => UI_LANGUAGES.some(([c]) => c === code);
  try {
    const chosen = localStorage.getItem("celsus-ui-lang");
    if (chosen && known(chosen)) return chosen;
  } catch {}
  for (const tag of navigator.languages?.length ? navigator.languages : [navigator.language || "en"]) {
    const base = String(tag).toLowerCase().split("-")[0];
    if (known(base)) return base;
  }
  return "en";
})();

/** One part of the strings (app, guides, reader, web) in the interface language, completed from English. */
function uiStrings(part) {
  return { ...(LOCALES.en?.[part] || {}), ...(LOCALES[UI_LANG]?.[part] || {}) };
}

document.documentElement.lang = UI_LANG;
document.documentElement.dir = UI_LANGUAGES.find(([c]) => c === UI_LANG)?.[2] ? "rtl" : "ltr";
