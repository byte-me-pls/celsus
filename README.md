<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="branding/logo-dark.png">
    <img src="branding/logo-light.png" alt="Celsus" width="320">
  </picture>
</p>

<p align="center"><b>A library that translates books without breaking their layout.</b></p>

Celsus opens a PDF, finds every block of text on every page, has a language model translate it, and writes the
translation back into the same place: the same fonts, sizes, columns, tables, captions and page numbers. Diagrams,
code and formulas are left alone. Covers and pictures with text in them are redrawn in the target language.

It is named after the Library of Celsus in Ephesus.

## What it can do

- **Keeps the page.** Text is fitted into the box it came from, wrapping and shrinking within readable limits.
  Blocks that still do not fit keep the original text and are reported, never cut off.
- **About 130 target languages.** Text is shaped with rustybuzz (HarfBuzz in Rust): Arabic letters join, Persian,
  Urdu and Hebrew run right to left, Indian scripts form their conjuncts, Thai marks sit in place, and Chinese,
  Japanese and Korean break between characters. Each character falls back to an installed font that has it.
- **Any OpenAI-compatible model.** OpenAI, NVIDIA, OpenRouter, a local Ollama or LM Studio, or anything else that
  speaks the same API. Translation, book recognition and picture redrawing can each use a different model.
- **Translate from a chat.** Claude, OpenAI Codex, Gemini CLI, Cursor, VS Code and other MCP clients can translate
  books in your library with your subscription, and an assistant that can draw (such as Codex) redraws covers
  itself.
- **Covers and pictures.** A picture model, or the assistant in your chat, redraws a cover with its text translated
  and the artwork, colours and lettering kept.
- **Reader.** A fast built-in reader with scrolling and page modes and a side-by-side view of original and
  translation.
- **Interface in 17 languages:** English, Türkçe, Deutsch, Français, Español, Italiano, Português, Nederlands,
  Polski, Русский, Українська, العربية, हिन्दी, Bahasa Indonesia, 简体中文, 日本語, 한국어.
- **Server edition.** A shared library on a home server or Raspberry Pi, with everything the desktop app has, plus
  accounts, two-step sign-in, API keys, an audit log, and MCP over the network for ChatGPT, Claude and coding apps,
  in a Docker image.

## Get started

### Desktop app (Windows, macOS, Linux)

Download the file for your system from [Releases](https://github.com/byte-me-pls/celsus/releases):

| System | File | Install |
| --- | --- | --- |
| Windows 10, 11 | `Celsus_x.y.z_x64-setup.exe` | Run it. It installs for your user only; no administrator rights are needed. |
| macOS (Apple silicon and Intel) | `Celsus_x.y.z_universal.dmg` | Open it and drag Celsus to Applications. |
| Debian, Ubuntu, Mint | `Celsus_x.y.z_amd64.deb` | `sudo apt install ./Celsus_x.y.z_amd64.deb` |
| Fedora, openSUSE | `Celsus-x.y.z-1.x86_64.rpm` | `sudo dnf install ./Celsus-x.y.z-1.x86_64.rpm` |
| Other Linux | `Celsus_x.y.z_amd64.AppImage` | `chmod +x` it and run it. |

The files are not signed yet, so the system asks before the first start. On Windows choose *More info → Run
anyway*. On macOS open the app once, then allow it under *System Settings → Privacy & Security → Open Anyway*.
On Linux, translations are written with the Noto, Liberation or DejaVu fonts; most systems have one of them, and
`fonts-noto-core` covers the most languages.

Then:

1. **Settings → Connections:** add a model connection (the guide on each one tells you where to get a key), or
   connect a chat app under *Translate from a chat* and skip keys entirely.
2. **Library → Add books:** drop in PDFs. With a model chosen for book details, title, author and subject are
   filled in for you.
3. Open a book, pick the target language and the pages, and press **Add to queue**. Translated pages appear in the
   reader as they finish.

More in [docs/desktop.md](docs/desktop.md).

### Translate from a chat (MCP)

The desktop app adds Celsus to the MCP settings of the chat and coding apps it finds on your computer
(**Settings → Connections → Translate from a chat**). Then ask in a chat, for example:

> Translate pages 10–12 of Domain-Driven Design in my Celsus library into Turkish.
>
> Translate the cover of that book into Turkish. Draw it yourself.

On a server, the same tools are at `https://your-server/mcp`: ChatGPT and Claude connect with a sign-in (OAuth),
Codex, Claude Code and Cursor with an API key; the web interface's **Chat apps** page shows how.

Tools, setup by hand and how covers work: [docs/mcp.md](docs/mcp.md).

### Server edition

```sh
curl -fsSL https://raw.githubusercontent.com/byte-me-pls/celsus/main/install.sh | bash
```

Requirements, HTTPS, backups and settings: [docs/server-setup.md](docs/server-setup.md). How it is secured:
[docs/server-security.md](docs/server-security.md).

## Build from source

You need Rust 1.94 or newer. On Windows the desktop app also needs the WebView2 runtime (included in Windows 10
and 11); on Linux, the WebKitGTK and D-Bus development packages:

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libdbus-1-dev pkg-config patchelf
```

```sh
cargo build --release -p celsus-desktop -p celsus-mcp   # desktop app and MCP server
cargo build --release -p celsus-server                  # server edition
cargo test --workspace                                  # tests (server tests need CELSUS_TEST_DATABASE_URL)
```

The installers are made with the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`), each on the system
it is for: the Windows setup program on Windows, the `.dmg` on macOS, the `.deb`, `.rpm` and AppImage on Linux.

```sh
bash scripts/bundle.sh
```

The server image builds with plain `docker build -t celsus .`.

Tests for writing every script use the fonts installed on the system (`C:\Windows\Fonts`,
`/System/Library/Fonts`, `/usr/share/fonts`, or `CELSUS_TEST_FONTS_DIR`); on Linux install `fonts-noto-core` and
`fonts-wqy-zenhei`.

## How it is put together

| Crate | What it does |
| --- | --- |
| `celsus-pdf` | Reads PDFs (objects, streams, fonts, content), and writes changes back with embedded TrueType subsets |
| `celsus-core` | Finds text blocks, talks to models, fits and shapes text in every script, writes translated pages |
| `celsus-store` | The desktop library on disk: SQLite, book folders, translation cache, redrawn pictures |
| `celsus-desktop` | The Tauri app and its interface (`ui/`, shared with the web edition) |
| `celsus-mcp` | The MCP server chat apps start to work with the library |
| `celsus-server` | The server edition: HTTP API, accounts and security, PostgreSQL, web interface |

## Licence

Celsus is released under the [MIT licence](LICENSE). It bundles pdf.js (Apache-2.0) and the DM Sans and Instrument
Serif fonts (SIL Open Font License 1.1); their licences are next to them in `crates/celsus-desktop/ui/`.
