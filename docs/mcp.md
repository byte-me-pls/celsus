# Translating from a chat (MCP)

Celsus ships an MCP server, `celsus-mcp`, next to the desktop app. Chat and coding apps that support the Model
Context Protocol start it and get tools to work with your library: Celsus hands out the text block by block, the
assistant in the chat translates it with your subscription, and Celsus checks each answer and writes the finished
pages into the PDF. No API key is needed for this.

In short: connect your chat app once (one click in the desktop app), then ask it in plain words to translate a
book. The assistant does the translating; Celsus keeps the layout.

| You use | Where it connects | How |
| --- | --- | --- |
| Claude Desktop, Claude Code, Codex, Gemini CLI, Cursor, VS Code and others on your computer | the desktop app's library | [Setting it up](#setting-it-up) |
| ChatGPT or Claude on the web or a phone | a Celsus server | [On a Celsus server](#on-a-celsus-server), sign-in |
| A coding app, with a shared library | a Celsus server | [On a Celsus server](#on-a-celsus-server), API key |

## Setting it up

In the desktop app open **Settings → Connections → Translate from a chat**. It lists the apps it found on this
computer (Claude Desktop, Claude Code, OpenAI Codex, Gemini CLI, Antigravity, VS Code, Cursor, Windsurf, Cline,
LM Studio) and adds Celsus to their settings with one click. Restart the app afterwards.

For an app that is not listed, add this to its MCP settings (most apps use this format), with the path to
`celsus-mcp.exe` in the folder Celsus is installed in. The same page of the app shows the path to copy. On macOS it
is `/Applications/Celsus.app/Contents/MacOS/celsus-mcp` (move Celsus to Applications first: an app started from the
disk image or the Downloads folder has no lasting place), on Linux `/usr/bin/celsus-mcp`. The AppImage keeps a copy
of the server in `~/.local/share/io.github.byte-me-pls.celsus/bin/` and points other apps at that.

```json
{
  "mcpServers": {
    "celsus": { "command": "C:\\Users\\you\\AppData\\Local\\Celsus\\celsus-mcp.exe", "args": [] }
  }
}
```

Claude Code: `claude mcp add celsus -- "C:\path\to\celsus-mcp.exe"`. Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.celsus]
command = 'C:\path\to\celsus-mcp.exe'
args = []
```

ChatGPT on the web only accepts MCP servers reachable over the internet, so it cannot use the local server; on the
OpenAI side use Codex.

The server works on the same library as the desktop app (`CELSUS_DATA_DIR` if set). It speaks JSON-RPC over
standard input and output and never opens a network port.

## On a Celsus server

The server edition offers the same tools over the network at `https://your-server/mcp` (Streamable HTTP). In the
web interface, **Chat apps** shows the exact address, ready-made settings and the apps you have connected.

- **ChatGPT and Claude** (web, desktop and mobile): add a custom connector with the address. The app opens a Celsus
  page where you sign in and allow it; no key is needed. These apps reach the server over the internet, so it needs
  an https address (see [server-setup.md](server-setup.md)).
- **Codex, Claude Code, Cursor, VS Code and others**: use the address with an API key that may read and translate
  (**Account → API keys**), sent as `Authorization: Bearer <key>`:

```sh
claude mcp add --transport http celsus https://your-server/mcp --header "Authorization: Bearer celsus_..."
```

```toml
# ~/.codex/config.toml
[mcp_servers.celsus]
url = "https://your-server/mcp"
bearer_token_env_var = "CELSUS_API_KEY"
```

On the server the assistant works as you: it sees the books you can see, and what it translates counts for you.
`add_book` takes the PDF as base64, `replace_picture` the picture as base64, and `get_output` gives the address the
translated PDF can be downloaded from with the same credentials.

## Translating text

Ask in plain words, for example *"Translate pages 10–12 of Domain-Driven Design in my Celsus library into
Turkish."* The assistant then:

1. finds the book with `list_books` (or adds a PDF with `add_book`),
2. calls `get_text_to_translate` for the next blocks, with instructions and the text around them,
3. sends its translations with `submit_translations`. Celsus checks each one (numbers missing, text left untranslated,
   much shorter than the source, an added explanation or ellipsis, a list with the wrong number of lines) and returns the ones it rejects with the reason; the assistant
   fixes and resends them,
4. repeats until no blocks are left. `get_output` tells where the translated PDF is.

## Covers and pictures

Covers are redrawn, not typed over. An assistant that can generate images (OpenAI Codex can) does it itself:

> Translate the cover of Code in my Celsus library into Turkish. Draw it yourself.

1. `list_pictures` finds the pictures on a page (page 1, the cover, by default).
2. `get_picture` gives the assistant the image itself, so it can see the artwork and read the text.
3. The assistant draws the same picture with every text translated and saves it as PNG or JPEG.
4. `replace_picture` puts it into the translated PDF in place of the original (`file_path`, or `image_base64`).
5. `get_picture` with `redrawn: true` shows the result; `restore_picture` brings the original back.

An assistant that cannot draw (Claude, for instance) can call `redraw_picture` instead: Celsus then asks the picture
model chosen in the desktop app under **Settings → Models** to draw it.

## Tools

| Tool | Changes the library | What it does |
| --- | --- | --- |
| `list_books` | no | Books with page counts and how many pages are translated into each language |
| `add_book` | yes | Adds a PDF from this computer |
| `book_info` | no | One book's details; with a language, which pages are done |
| `get_text_to_translate` | no | The next blocks to translate, with instructions and context |
| `submit_translations` | yes | Checks and stores translations, writes finished pages |
| `get_output` | no | Where the translated PDF is |
| `list_pictures` | no | Pictures on the given pages, and which are redrawn; the picture model Celsus would use |
| `get_picture` | no | The picture itself (original, or redrawn in a language) |
| `replace_picture` | yes | Puts a picture the assistant made into the translated PDF |
| `redraw_picture` | yes | Has Celsus redraw a picture with the picture model chosen in the app |
| `restore_picture` | yes | Back to the original picture |

Tools that only read are marked read-only, so apps that honour the hint can run them without asking each time.
Languages are codes such as `tr`, `de`, `ar`, `hi`, `ja`, `zh-hans` or `pt-br`.
