# Using the desktop app

In short: add a model under **Settings → Connections**, drop a PDF into the **Library**, open it, choose a language
and press **Add to queue**. The rest of this page explains each step and what happens behind it.

- [Connections and models](#connections-and-models)
- [Translating a book](#translating-a-book)
- [Covers and pictures](#covers-and-pictures)
- [Reader](#reader)
- [Interface language](#interface-language)
- [Where things are kept](#where-things-are-kept)

## Connections and models

Celsus talks to language models through any OpenAI-compatible API. Under **Settings → Connections** add one or more:

| Connection | Key | Good for |
| --- | --- | --- |
| OpenAI | from platform.openai.com | translation (`gpt-…-mini` is fast and cheap), covers (`gpt-image` models) |
| NVIDIA | free at build.nvidia.com (`nvapi-…`) | translation with free credits (nemotron, llama, qwen) |
| OpenRouter | from openrouter.ai | hundreds of models with one key; `google/gemini-…-image` for covers |
| Ollama, LM Studio | none | everything runs on your computer |
| Custom | if the server needs one | any other `/v1` endpoint |

Keys are kept in the system credential store (Windows Credential Manager, macOS Keychain, Secret Service on Linux),
never in a file or the database.

Under **Settings → Models** choose, for each job, a connection and a model:

- **Translation**: the book text.
- **Book details**: recognises a newly added book (title, author, subject, language). Falls back to the translation
  model.
- **Pictures and covers**: redraws pictures with their text translated. Needs an image-editing model.

You can also skip keys entirely and translate from a chat app with its own subscription; see [mcp.md](mcp.md).

## Translating a book

Open a book from the library. On its page:

1. Set the **target language**. Type a code or a name; about 130 languages are listed (for Chinese choose
   `zh-hans` or `zh-hant`, for Brazilian Portuguese `pt-br`).
2. Choose the pages: a **range**, only the **missing** ones, or **all**. The estimate shows characters, tokens and
   time.
3. **Add to queue.** Jobs run one after another in the background; **Translations** shows their progress, cost in
   tokens, and lets you stop or retry them. A job you stop says *Stopping…* and ends after the request it is
   waiting for; the pages it finished are kept.

Translations are cached: translating the same text again costs nothing, and if a job stops halfway, retrying picks
up where it left off. A page where the model gave no usable answer for part of the text is not counted as done, so
choosing the **missing** pages picks it up again.

The page map on the book page shows which pages are translated, which have blocks that kept the original (orange),
and which are not done yet. **Read** opens the reader; **Save PDF** writes the translated book wherever you want.

### What happens on each page

**Before translating.** Code, formulas, URLs and numbers-only blocks are never sent for translation. Words inside
a paragraph that are printed in another style (italics, bold, a fixed-pitch face, another colour) are marked, so
the translation comes back with the same words in the same style. For right-to-left languages the whole block keeps
its own style.

**Checking.** Every answer is checked before it is used: numbers missing, text left untranslated, a translation
much shorter than the source, an added explanation, or a list with the wrong number of lines. A rejected answer is
asked for again with the reason.

**Placing.** The blocks of a page are placed together, not one by one. Each block knows its column, what it must
stay above, and the pictures its lines run around. A translation that is longer than the original first takes the
room the page has, then tighter lines, then smaller type down to 70% of the original and never below 6 pt. Lists
and tables of contents keep one line per entry.

**What does not fit.** A block that still does not fit, or whose translation has characters no installed font can
draw, keeps the original text and is listed in the job's report. Nothing is cut off.

### Fonts

Translations are written with TrueType fonts from **Settings → Fonts** (Windows: `C:\Windows\Fonts`, macOS:
`/System/Library/Fonts`, Linux: `/usr/share/fonts`). For each typeface a book uses, Celsus picks the installed
family nearest to it in letter widths, in regular, bold, italic and bold italic; when the book's own typeface is
installed, that one is used. This matching is done for languages written in the Latin, Cyrillic and Greek
alphabets. For other scripts it adds the system's fonts for that script: on Windows these are Segoe UI and Tahoma
(Arabic, Hebrew), Nirmala UI (Indian scripts), Leelawadee UI (Thai, Lao, Khmer), Microsoft YaHei and JhengHei
(Chinese), Yu Gothic and Meiryo (Japanese), Malgun Gothic (Korean) and others. On macOS the text is set in Arial,
Times New Roman and Courier New, with Arial Unicode, PingFang, Songti and Apple SD Gothic Neo for other scripts. On
Linux it is set in Noto, Liberation or DejaVu; for other scripts install the Noto fonts of your system
(`fonts-noto-core`) and, for Chinese, Japanese and Korean, `fonts-wqy-zenhei`. Noto fonts are used when present on
any system. To use other fonts, point the setting at a folder of `.ttf`/`.ttc` files; subfolders are searched too.

## Covers and pictures

In the library, a book's cover is its first page, drawn the first time the book is shown.

On the book page, **Cover and pictures** shows the pictures on a page (page 1 is usually the cover). **Redraw**
sends the picture to the picture model with instructions to translate every text in it and keep everything else:
artwork, colours, lettering style, names, logos and barcodes. An optional note adds wishes ("split the title over two
lines"). The redrawn picture replaces the original in the translated PDF and stays there when more pages are
translated later; **Back to original** undoes it.

Each drawing is billed to the picture model's account. Without a picture model, ask an assistant that can draw to
do it through MCP instead ([mcp.md](mcp.md#covers-and-pictures)).

## Reader

Click a book to read it. The reader draws only the pages near the screen, so long books scroll smoothly.

- **Scroll** or **page** mode, zoom, fit to width, jump to a page.
- **Compare side by side** shows the original and the translation next to each other.
- On a page that is not translated yet, **Translate from here** queues it (or the next ten pages); the page updates
  by itself when the job finishes.

## Interface language

**Settings → General → Interface language**: English, Türkçe, Deutsch, Français, Español, Italiano, Português,
Nederlands, Polski, Русский, Українська, العربية, हिन्दी, Bahasa Indonesia, 简体中文, 日本語 or 한국어. *System*
follows the language of the system. Arabic switches the interface to right to left.

## Where things are kept

Everything is in one folder, `io.github.byte-me-pls.celsus`, under `%APPDATA%` on Windows,
`~/Library/Application Support` on macOS and `~/.local/share` on Linux:

| What | Where |
| --- | --- |
| Library database | `celsus.db` |
| Books, translated PDFs, redrawn pictures | `library/books/<id>/` |
| API keys | not in the folder: Windows Credential Manager, macOS Keychain, or the Secret Service on Linux (GNOME Keyring, KWallet), entries named `Celsus` |

Uninstalling the app leaves the library in place. `CELSUS_DATA_DIR` moves it elsewhere.
