# Changelog

Versions are 0.YYMM.N: year and month of the release, then a counter for further releases in that month.

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
