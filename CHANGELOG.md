# Changelog

## 0.1.0 (unreleased)

First release.

- Own PDF reader and writer: finds text blocks, keeps protected content (code, formulas, URLs), writes translations
  back with embedded font subsets, fitted into the original boxes.
- About 130 target languages with text shaping (rustybuzz), right-to-left lines, Unicode line breaking and
  per-character font fallback; TrueType collections supported.
- Windows desktop app: library, built-in reader with side-by-side view, translation queue with cache and retries,
  model connections per job (OpenAI, NVIDIA, OpenRouter, Ollama, LM Studio, any OpenAI-compatible endpoint), book
  recognition, cover and picture redrawing, interface in 17 languages.
- MCP server: translate books from Claude, Codex, Gemini CLI, Cursor, VS Code and other MCP clients; assistants that
  can draw redraw covers themselves.
- Server edition: accounts, two-step sign-in, API keys, audit log, PostgreSQL, web interface, Docker image for amd64
  and arm64, install script.
