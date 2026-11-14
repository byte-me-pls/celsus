# Changelog

Versions are 0.YYMM.N: year and month of the release, then a counter for further releases in that month.

## Unreleased

- **A night light for reading.** The reader has a reading light beside its zoom: pages can stay as printed, take a warm evening light, or be shown light on dark. Warmth and brightness are set with two sliders, and N turns the night light on and off. The choice is kept for every book.
- **Colours of your own.** Settings has a page for colours: whole palettes to start from (four for each theme),
  the accent at a click, and every colour of the interface on its own: background, sidebar, surfaces, fields,
  lines, text, and the colours of success, errors, warnings and notes. Each theme keeps its own, and colours that
  must be read are made to read on the background chosen.
- **Contents and search in the reader.** A panel beside the pages lists the contents the PDF carries, as a
  browser shows them, with the titles in the language being read where the book's translation has them; the part
  being read is marked, and a click goes there. The same panel searches the book (Ctrl+F): every place is listed
  with its page and the words around it, and marked on the page; F3 goes to the next one. A word is found in
  capitals or small, in small capitals, with or without its accents, and broken over two lines.
- **Closing the window keeps Celsus at work.** Celsus stays beside the clock (in the menu bar on a Mac) and goes on
  with the translations in its queue; a click on its icon opens the window again, and its menu quits. A setting in
  General turns this off. On a system with no place for such icons, closing quits as before.
- **A job that fails for a passing reason goes on by itself.** A model that stays silent, a connection that
  drops or a rate limit that is used up used to end the job, and it waited for someone to press Continue. Now it
  goes back to the queue and is tried again after two, five and fifteen minutes, and for as long as each try gets
  some pages further. The list says when. A key that is refused, a model that is gone or answers that cannot be
  used still end the job, since waiting does not help there.
- **Which model translates.** Each job in the list says the model it uses, marked while it is at work. The version
  of the app stands at the top of the window.
- **What a new version brings is shown.** After an update Celsus says once, in short, what the new version
  brought, with a link to all its notes. The notice that a newer version is out lists it too.
- **The brand at the head of the sidebar.** The facade of the Library of Celsus and its name now stand large at
  the top of the sidebar; a click on it goes to the library. On the web too.
- **A few translated pages do not make a translated book.** A book counted as translated, and showed the badge of
  the language, as soon as one of its pages was. Now only a book translated from its first page to its last does;
  one that is partly translated has a dashed badge with how far it got (TR %9), and a filter of its own.
- **Jobs can be taken off the list.** Every job that has ended has a Remove button, and the list one to remove all
  that have ended. Their translated pages stay, and what they used still counts in the totals. A stopped or failed
  job whose pages have all been translated since, by a later job, leaves the list by itself.
- **One Celsus at a time.** Starting Celsus while it is open brings its window forward instead of opening a second
  app on the same library. And the queue of a library is worked by one app only, even when an older copy is left
  open: two copies used to run the same job side by side.
- **Translations no longer run over one another.** Two blocks whose boxes overlapped were never checked against
  each other, though their lines did not: a paragraph cut in two by a small picture in one of its lines, an index
  entry that began on the line of the one before. When the first came out longer, the two were written over one
  another. Now the second begins on the next line, and where nothing else helps one of them keeps its original.
- **A speech bubble drawn over a drawing is read as one.** Lines drawn under the white of a bubble or of another
  round shape no longer cut the text in it in two.
- **Long author names no longer push a book's card over the next one** in the library.

## 0.2611.6 (2026-11-13)

- **Titles set flush right stay flush right.** A title or subtitle of two lines, set against the right edge as on
  the title page of a book, was taken for a paragraph with a deep hanging indent: the translation began where the
  first line began and its other lines were pushed far in, so far that a word had to be broken in two.
- **Words are not broken while a smaller size keeps them whole.** A word wider than its line was broken
  between two letters. Now the text first gets a little smaller, as far as the usual limits go; only a word that
  would not fit at any size, such as a long address, is still broken.
- **Heavy faces stay bold.** A title set in a heavy display face whose font calls itself regular, as many do,
  came back in a regular weight. The page is looked at now: text of 14 points and up whose strokes are as thick
  as those of a black or heavy face is set in bold. Smaller text is left to what its font says, since at that
  size the strokes cannot be told apart on the page.
- **Condensed faces stay narrow.** Text set in a condensed face (titles, running heads, the code of some
  publishers) was written in a font of ordinary width and took more room than it had. Its letters are now drawn
  as narrow as the original's.

## 0.2611.5 (2026-11-13)

- **Finding working models says what is wrong.** When not one model answered, the result was "0 of 60" and
  nothing more. Now it shows what most of them said, which is most often something about the connection, such as
  a key that was refused.
- **A key the system will not give is said to be so.** On a Mac the keychain asks again, after an update, before
  the new version may read the key the old one stored; until it is allowed, every request went out without a key
  and every model seemed not to answer. Celsus now says that the key could not be read, and asks the keychain
  once a run instead of once for every model tried.

## 0.2611.4 (2026-11-13)

- **Translated pages show while the job runs.** The translated book used to be written only when a job ended or
  was stopped, so a page marked as translated still showed the original until then. It is written every few
  seconds now, with every page finished so far, and the reader loads the new pages by itself: at once for the
  pages in view, otherwise when you turn the page. On the server too.
- **Pages are looked at.** Before a translation is placed, the page is drawn the way a reader sees it, and
  what is on it besides the text to translate is found by looking: figures, rules, listings, the drawings of a
  scanned page. It takes about a fiftieth of a second a page. A page that cannot be drawn is placed by what its
  content says, as before.
- **Figures make room.** A translation that needs more lines than its original used to get smaller as soon as
  a figure, a listing or a rule stood under it. Now what stands under it goes down as it is, with its caption
  and the text inside it, as far as the page has room, and only then does the text get smaller. The old place is
  painted over with the paper around it. Not yet on pages whose columns change places for a language read from
  right to left.
- **Text stays together.** Paragraphs that follow one another without a blank line are one run of text, and a
  listing or a figure set into it is part of the run. When a translation comes out shorter than its original,
  what follows it closes up, so that no hole opens in the middle of the text; a page ends a little earlier
  instead. And a block that is short of room goes up into room left above it before it is made smaller. Before
  this, one paragraph that needed a line more could make all the text of a page smaller while other paragraphs
  left white space behind them.
- **A short paragraph is not centred by mistake.** Two lines, the first indented and ending short of the margin,
  were taken for centred lines and their translation was centred.
- **Smaller text stays readable.** Text that had to be set smaller than its original is drawn with slightly
  heavier strokes and, where it was not black, a little darker, the more so the smaller it got.
- **Scanned books that carry their text.** A book scanned in black and white, with the text laid on its pages
  for searching, can be translated. The text is read from that layer, visible or not; the words in the picture
  are covered with the white of the paper, exactly where their ink is seen, and the translation is written over
  them. Figures are found by looking at the page, so text grows into free room and moves figures as it does in
  any other book; only text inside a figure stays where it was. Code and whatever else is not translated stays
  as the picture shows it. Before this, such a translation was written on top of the picture's own words.
  Scans in grey or colour, and scans without a text layer, are still not read.
- **Questions are asked before, not after.** Deleting a book, removing a connection, clearing the cache and
  disconnecting a chat app went ahead without waiting for the answer to "are you sure?". They wait now.
- **Settings that cannot break a job.** The number settings stay within sensible limits whatever is typed, and
  a connection without a model is not saved as a choice.

## 0.2611.3 (2026-11-12)

- **Starting out.** Translating without a model no longer answers with a code: the app says that a model is
  needed, takes you to where one is chosen, and shows a note on the library page until one is.
- **A connection is tried before it is saved.** Its key is checked without spending anything. A connection that
  turns the key away, or whose address is no address, is not saved and says why; a list of models that cannot be
  read says why too.
- **Contents pages.** A row of leader dots is cut to five before it goes to a model and filled out again when the
  page is written, for single entries and for lists of them. A long row could keep a model writing dots until
  its answer was cut off, so that the job stood on that page for minutes each time it was run.
- **Continue.** A job that was stopped goes on from where it was: its pages that are translated already count as
  done from the start and nothing is asked for them again. The button says *Continue*.
- **A job says what it is doing.** While it works on a page for more than a few seconds, the job shows the page
  and how long it has been on it, so that waiting for a slow model does not look like standing still.
- **A model whose answers cannot be used ends the job.** After eight unusable answers in a row the job stops and
  says to choose another model, in place of asking for every block of every page twice. A connection that keeps
  refusing for its rate limit ends the job at once, not block by block.
- **Pages without text.** A scanned book has only pictures of its pages. Queueing such pages is refused with an
  explanation, and a job that finds nothing to translate no longer ends as done.

## 0.2611.2 (2026-11-12)

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
- **Chinese, Japanese and Korean.** A complete translation into these scripts is no longer taken for one that
  was cut short because it has far fewer characters than its source.
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
