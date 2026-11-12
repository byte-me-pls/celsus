# Changelog

Versions are 0.YYMM.N: year and month of the release, then a counter for further releases in that month.

## Unreleased

- **Columns for languages read from right to left.** On a page of two or more columns translated into Arabic,
  Hebrew, Persian or Urdu, the columns change places, each with its pictures, formulas, rules and links, so the
  first column is the one on the right. What lies across the columns stays where it is. Tables keep the order of
  their columns.
- **Stop works while a model is silent.** A job waiting for a model that is slow or does not answer stops at
  once instead of holding on until the request times out.
- **A silent model ends the job.** A model that has not answered in 180 seconds (set under *Settings →
  Translation*) is given up: the job stops, keeps its finished pages and says to choose another model. A model
  that had been answering gets one more try first.
- **Choosing a model.** The list shows the models of the chosen connection under the names of their makers, can
  be searched by typing, leaves out models that do not answer in text, and remembers how each model did when it
  was last tried.
- **New versions.** The desktop app asks GitHub at start whether a newer release is out. The Windows and macOS
  apps and the AppImage then update themselves at the press of a button: the new version is downloaded, its
  signature is checked against the project's key, and the app starts again. The `.deb` and `.rpm` packages are
  shown the download page. *Settings → General* turns the check off.
- **Pages translated before are not paid for twice.** A new job writes the earlier pages of a book from the
  translations that were kept and asks the model nothing for them. A page whose translation is no longer kept
  goes back to the original and is shown as not translated, instead of being sent to the model again unasked.
- **Clearing the cache keeps what books are written from.** Only translations that no translated page uses are
  cleared, so a book's pages no longer go back to the original the next time it is written.
- **One spelling for a language.** Redrawing a picture in a translation such as zh-hans or pt-br no longer writes
  a second, nearly empty translation over the first.
- **Sturdier against bad input.** A font table with an impossible range of codes no longer keeps the reader busy
  for ever. On the server, a list of pages has a total limit however it is written, and passwords are hashed only
  for a valid invitation, a few at a time.

## 0.2611.1 (2026-11-10)

- **A whole page is placed at once.** Each block knows the column it stands in, what it must stay above, and the
  pictures and text its lines run around. A translation that is longer first takes the room the page has and only
  then is set smaller.
- **Styles inside a paragraph are kept.** Words in italics, bold, a fixed-pitch face or another colour are marked
  for the translator and come back in the same style. Right-to-left targets keep the style of the block.
- **Nearest typeface.** For each family of fonts a book uses, the installed family nearest in letter widths stands
  in for it (the same family where it is installed). For Latin, Cyrillic and Greek targets.
- **Reading the page.** Table cells, headed rows, lists, the dotted lines of a table of contents, running heads and
  notes are told apart from running text. A paragraph beside a table is no longer cut in two, and a heading set
  across two columns is not taken for a column of its own.
- **Jobs.** A job asked to stop says "Stopping…" at once. The same request sent twice is one job. A page where the
  model gave no usable answer for some of the text is not marked as done, so "missing pages" picks it up again. A
  run that stopped early keeps its finished pages. Removing a book stops its translation.
- **Library.** A book's cover is its first page, as the reader draws it, in the app and on the server.
- A new library translates into the language of the interface. The server says that no model is chosen before a
  job is queued.

## 0.2611.0 (2026-11-09)

First release.

- Own PDF reader and writer: finds text blocks, keeps protected content (code, formulas, URLs), writes translations
  back with embedded font subsets, fitted into the original boxes.
- About 130 target languages with text shaping (rustybuzz), right-to-left lines, Unicode line breaking and
  per-character font fallback; TrueType collections supported.
- Desktop app for Windows, macOS (Apple silicon and Intel) and Linux (`.deb`, `.rpm`, AppImage): library, built-in reader with side-by-side view, translation queue with cache and retries,
  model connections per job (OpenAI, NVIDIA, OpenRouter, Ollama, LM Studio, any OpenAI-compatible endpoint), book
  recognition, cover and picture redrawing, interface in 17 languages.
- MCP server: translate books from Claude, Codex, Gemini CLI, Cursor, VS Code and other MCP clients; assistants that
  can draw redraw covers themselves.
- Server edition: everything the desktop app does (covers and pictures, book recognition, estimates, usage,
  translation settings, model tests), accounts, two-step sign-in, API keys, audit log, PostgreSQL, web interface,
  Docker image for amd64 and arm64, install script.
- MCP on the server (`/mcp`): ChatGPT and Claude connect with OAuth 2.1 (registration, PKCE, rotating refresh
  tokens), other apps with API keys; connected apps can be disconnected.
