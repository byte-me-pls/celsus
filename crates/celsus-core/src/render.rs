//! Writing translations into a PDF.
//!
//! For every translated block the original glyphs are taken out of the content stream (nothing else on the page
//! is touched) and the translation is drawn in its place with the closest bundled font, at the original size and
//! colour. Where each line goes is worked out for the page as a whole (see [`crate::place`]); a block that
//! cannot be set at a readable size keeps its original text and is reported instead.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::rc::Rc;

use celsus_pdf::edit::{Insertion, edit};
use celsus_pdf::embed::FontEmbedder;
use celsus_pdf::font::Font;
use celsus_pdf::interp::{Bounds, Matrix, PageContent, Source, TextRun, invert};
use celsus_pdf::write::{Saver, real};
use celsus_pdf::{Dict, Document, ObjRef, Object, Page, Stream};
use serde::{Deserialize, Serialize};

use crate::extract::{ExtractedPage, GlyphRef};
use crate::fit::Measure;
use crate::fonts::FontSet;
use crate::inline;
use crate::layout::{BlockId, TextBlock};
use crate::place;
use crate::shape::Shaper;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum Issue {
    /// Even at the smallest readable size the translation does not fit; the original text was kept.
    DoesNotFit { id: BlockId },
    /// No bundled font has these characters; the original text was kept.
    MissingGlyphs { id: BlockId, chars: String },
    /// The id does not exist on this page.
    UnknownBlock { id: BlockId },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageReport {
    pub page: u32,
    pub replaced: Vec<BlockId>,
    pub kept: Vec<Issue>,
}

/// Collects translated pages of one document and writes the result.
pub struct BookWriter<'d> {
    doc: &'d Document,
    /// The faces to write with: the general ones, and those that stand in for the document's own families.
    fonts: Rc<FontSet>,
    saver: Saver<'d>,
    embedders: HashMap<usize, (FontEmbedder, ObjRef)>,
    /// Form XObjects already rewritten (shared forms are edited once).
    forms_done: BTreeSet<ObjRef>,
}

impl<'d> BookWriter<'d> {
    pub fn new(doc: &'d Document, fonts: &FontSet) -> Self {
        // Settled here for the whole document, whichever of its pages are written and in whatever order.
        let fonts = Rc::new(fonts.matched(&document_fonts(doc)));
        BookWriter {
            doc,
            fonts,
            saver: Saver::new(doc),
            embedders: HashMap::new(),
            forms_done: BTreeSet::new(),
        }
    }

    fn font_ref(&mut self, face: usize) -> ObjRef {
        if let Some((_, r)) = self.embedders.get(&face) {
            return *r;
        }
        let r = self.saver.reserve();
        let font = self.fonts.faces()[face].font.clone();
        self.embedders.insert(face, (FontEmbedder::new(font), r));
        r
    }

    /// Replace the given blocks of one page with their translations.
    pub fn translate_page(
        &mut self,
        page: &Page,
        extracted: &ExtractedPage,
        translations: &BTreeMap<BlockId, String>,
        to: &crate::Language,
    ) -> celsus_pdf::Result<PageReport> {
        let rtl = to.is_rtl();
        let script = crate::languages::info(to).map(|i| i.script);
        let layout = &extracted.layout;
        let fonts = Rc::clone(&self.fonts);
        let fonts: &FontSet = &fonts;
        let mut report = PageReport {
            page: layout.page,
            ..Default::default()
        };
        // What to place: the translations, and with them short pieces of text that are not translated but stand
        // on the same line as one (a bullet, the number of a list entry, the page number in a table of
        // contents). Those move with their line when the page needs it.
        struct Entry<'b> {
            block: &'b TextBlock,
            /// With marks where the words set apart inside the block begin and end.
            text: String,
            /// The fonts of the block's own style, then of each style inside it that the text uses.
            chains: Vec<Vec<usize>>,
            companion: bool,
            /// The opening letters, set as a large initial.
            initial: String,
        }
        let mut entries: Vec<Entry> = Vec::new();
        for (id, text) in translations {
            let text = text.trim();
            let Some(block) = layout.blocks.iter().find(|b| &b.id == id) else {
                report.kept.push(Issue::UnknownBlock { id: id.clone() });
                continue;
            };
            // The words set apart inside the block come back with tags around them. Tags that are out of order
            // are left out and the text is set in the block's own style, as text that runs right to left is.
            let tagged: String;
            let text = if !block.inline.is_empty() {
                let names = inline::names(&block.style, &block.inline);
                let bare = inline::strip_marks(text);
                tagged = (!rtl)
                    .then(|| inline::marked(&bare, &names))
                    .flatten()
                    .unwrap_or_else(|| inline::without_tags(&bare));
                tagged.trim()
            } else if inline::has_marks(text) {
                tagged = inline::strip_marks(text);
                tagged.trim()
            } else {
                text
            };
            if text.is_empty() {
                continue;
            }
            let chain = fonts.chain_for(&block.style, script);
            if chain.is_empty() {
                report.kept.push(Issue::MissingGlyphs {
                    id: id.clone(),
                    chars: text.into(),
                });
                continue;
            }
            // No font for some character: the original stays, and the report says which characters were missing,
            // rather than printing gaps.
            let shaper = Shaper::new(fonts, chain.clone(), rtl);
            let mut missing = shaper.missing(&inline::strip_marks(text));
            // Small raised letters are rare in fonts. Written as ordinary letters the text is still all there.
            let plain: String;
            let text = if missing.iter().any(|c| crate::extract::plain_form(*c).is_some()) {
                plain = text
                    .chars()
                    .map(|c| match crate::extract::plain_form(c) {
                        Some(p) if missing.contains(&c) => p,
                        _ => c,
                    })
                    .collect();
                missing = shaper.missing(&inline::strip_marks(&plain));
                plain.as_str()
            } else {
                text
            };
            if !missing.is_empty() {
                report.kept.push(Issue::MissingGlyphs {
                    id: id.clone(),
                    chars: missing.into_iter().collect(),
                });
                continue;
            }
            // A paragraph that opened with a large initial opens with one again: the first letter of the
            // translation, with whatever stands before it.
            let cut = (!rtl && place::takes_initial(block))
                .then(|| {
                    text.char_indices()
                        .filter(|(_, c)| inline::style_of(*c).is_none())
                        .take(3)
                        .find(|(_, c)| c.is_alphanumeric())
                })
                .flatten()
                .map_or(0, |(at, c)| at + c.len_utf8());
            let initial = inline::strip_marks(&text[..cut]);
            // What follows the initial goes on in the style the initial stood in.
            let mut rest = String::with_capacity(text.len());
            rest.extend(inline::last_mark(&text[..cut]));
            rest.push_str(text[cut..].trim_start());
            let mut chains = vec![chain];
            if inline::has_marks(&rest) {
                chains.extend(block.inline.iter().map(|s| fonts.chain_for(s, script)));
            }
            let refilled = (block.lines.len() == 1)
                .then(|| {
                    let measure = Styled::new(fonts, &chains, rtl);
                    refill_leader(&rest, block.rect.x1 - block.rect.x0, |t| {
                        measure.width(t, block.style.font_size)
                    })
                })
                .flatten();
            entries.push(Entry {
                block,
                text: refilled.unwrap_or(rest),
                chains,
                companion: false,
                initial,
            });
        }
        let translated = entries.len();
        for b in &layout.blocks {
            if !b.protected || b.lines.len() != 1 || translations.contains_key(&b.id) {
                continue;
            }
            let text = b.lines[0].text.trim();
            if text.is_empty() || text.chars().count() > 6 {
                continue;
            }
            let beside_translation = entries[..translated].iter().any(|e| {
                let t = e.block;
                t.lines.len() <= 3
                    && (t.lines[0].baseline - b.lines[0].baseline).abs() <= 1.0
                    && (t.rect.x1 <= b.rect.x0 + 1.0 || b.rect.x1 <= t.rect.x0 + 1.0)
            });
            if !beside_translation {
                continue;
            }
            // It has to be drawn again if it moves, which is only safe for upright text in an ordinary font: a
            // bullet from a symbol font is some other character in any other font.
            let refs = extracted.glyphs.get(&b.id).map_or(&[][..], Vec::as_slice);
            let plain = !refs.is_empty()
                && refs.iter().all(|g| {
                    let run = &extracted.content.runs[g.run];
                    let name = run.font.name.to_ascii_lowercase();
                    run.angle.abs() < 0.01
                        && !["symbol", "dingbat", "wingding", "webding", "math", "extra"]
                            .iter()
                            .any(|s| name.contains(s))
                });
            let chain = fonts.chain_for(&b.style, script);
            if !plain
                || chain.is_empty()
                || !Shaper::new(fonts, chain.clone(), false)
                    .missing(text)
                    .is_empty()
            {
                continue;
            }
            entries.push(Entry {
                block: b,
                text: text.into(),
                chains: vec![chain],
                companion: true,
                initial: String::new(),
            });
        }

        let shapers: Vec<Styled> = entries
            .iter()
            .map(|e| Styled::new(fonts, &e.chains, rtl && !e.companion))
            .collect();
        let items: Vec<place::Item> = entries
            .iter()
            .zip(&shapers)
            .map(|(e, shaper)| place::Item {
                block: e.block,
                text: &e.text,
                measure: shaper,
                companion: e.companion,
                initial: e.block.initial.filter(|_| !e.initial.is_empty()).map(|cap| {
                    let width = shaper.width(&e.initial, cap.size);
                    crate::layout::Rect {
                        x1: cap.rect.x0 + width,
                        ..cap.rect
                    }
                }),
            })
            .collect();
        let placed = place::solve(layout, &extracted.obstacles, &items, rtl);
        drop(items);

        // New text. Each block is drawn where its original text was drawn (right after the text object that held
        // it), so it keeps its depth: text that a later drawing covered stays covered.
        let [cx0, _, _, cy1] = page.crop_box;
        let content = &extracted.content;
        let mut removals: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        let mut drawn_last = String::new();
        let mut insertions: Vec<Insertion> = Vec::new();
        let mut used_faces = BTreeSet::new();
        let mut drawn = 0usize;
        for ((entry, shaper), placed) in entries.iter().zip(&shapers).zip(&placed) {
            let b = entry.block;
            let Some(placed) = placed else {
                if !entry.companion {
                    report.kept.push(Issue::DoesNotFit { id: b.id.clone() });
                }
                continue;
            };
            // A companion whose line stayed where it was is left alone.
            if entry.companion && placed.shift.abs() < 0.3 {
                continue;
            }
            let refs = extracted.glyphs.get(&b.id).map_or(&[][..], Vec::as_slice);
            for g in refs {
                removals.entry(g.run).or_default().insert(g.glyph);
            }
            let (size, squeeze) = (placed.size, placed.squeeze);
            // The original may have been drawn under a clipping path (a printable area, a frame). A translation
            // that reaches past it would be cut there, so it is drawn after the page's own content instead.
            // The same goes for a background painted later (the white ground of a figure further down): it left
            // the original alone, but would cover a translation that grew onto it.
            let last_run = refs.iter().map(|g| g.run).max().unwrap_or(0);
            let was = [
                cx0 as f32 + b.rect.x0,
                cy1 as f32 - b.rect.y1,
                cx0 as f32 + b.rect.x1,
                cy1 as f32 - b.rect.y0,
            ];
            let touches = |p: &[f32; 4], r: &[f32; 4]| {
                p[0] < r[2] - 1.0 && p[2] > r[0] + 1.0 && p[1] < r[3] - 1.0 && p[3] > r[1] + 1.0
            };
            let covers: Vec<[f32; 4]> = content
                .paths
                .iter()
                .filter(|p| p.filled && p.after > last_run && !touches(&p.bounds, &was))
                .map(|p| p.bounds)
                .collect();
            let at = insertion_point(content, refs).filter(|(_, _, clip)| {
                placed.lines.iter().all(|line| {
                    let (x, y) = (cx0 as f32 + line.x, cy1 as f32 - line.baseline);
                    let ink = [x, y - size * 0.25, x + line.width, y + size * 0.75];
                    x >= clip[0] - 1.0
                        && x + line.width <= clip[2] + 1.0
                        && y - size * 0.25 >= clip[1] - 1.0
                        && y + size * 0.75 <= clip[3] + 1.0
                        && !covers.iter().any(|p| touches(p, &ink))
                })
            });
            let ink = |c: u32| {
                [(c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF].map(|v| real(f64::from(v) / 255.0))
            };
            let rgb = ink(b.style.color);
            // Inserted right after an operator: start on a new line so the two never run together.
            let mut draw = String::from("\nq ");
            if let Some((_, ctm, _)) = &at
                && let Some(inv) = invert(ctm)
            {
                writeln!(draw, "{} cm", inv.map(real).join(" ")).unwrap();
            }
            // Text state is part of the graphics state the original may have left set.
            writeln!(
                draw,
                "{} {} {} rg 0 Tc 0 Tw {} Tz 0 Ts 0 Tr BT",
                rgb[0],
                rgb[1],
                rgb[2],
                real(f64::from(squeeze * 100.0))
            )
            .unwrap();
            if let Some(cap) = b.initial.filter(|_| !entry.initial.is_empty()) {
                let shaped = shaper.of(0).shape_line(&entry.initial, cap.size);
                let (px, py) = (
                    cx0 + f64::from(cap.rect.x0),
                    cy1 - f64::from(cap.rect.y1 + placed.shift),
                );
                writeln!(draw, "100 Tz 1 0 0 1 {} {} Tm", real(px), real(py)).unwrap();
                for run in &shaped.runs {
                    self.font_ref(run.face);
                    used_faces.insert(run.face);
                    let font = &fonts.faces()[run.face].font;
                    let embedder = &mut self.embedders.get_mut(&run.face).unwrap().0;
                    write_run(&mut draw, run, font, embedder, cap.size, 0.0);
                }
                writeln!(draw, "{} Tz", real(f64::from(squeeze * 100.0))).unwrap();
            }
            // Words set apart inside the block may have a colour of their own.
            let mut colour = b.style.color;
            for line in &placed.lines {
                // Each stretch of the line in the fonts of its style.
                let shaped: Vec<_> = inline::pieces(&line.text)
                    .into_iter()
                    .map(|(style, text)| (style, shaper.of(style).shape_line(text, size)))
                    .collect();
                let (px, py) = (cx0 + f64::from(line.x), cy1 - f64::from(line.baseline));
                writeln!(draw, "1 0 0 1 {} {} Tm", real(px), real(py)).unwrap();
                // Justified lines spread the free space over their spaces. A line far too short for that (the
                // words would drift apart) is left ragged.
                let spaces = shaped
                    .iter()
                    .flat_map(|(_, piece)| &piece.runs)
                    .flat_map(|r| &r.glyphs)
                    .filter(|g| g.text == " ")
                    .count();
                let per_space = match line.fill {
                    Some(fill)
                        if spaces > 0 && fill > line.width && fill - line.width < fill * 0.25 =>
                    {
                        // The scaling of the letters applies to this too, so it is taken out again.
                        (fill - line.width) / spaces as f32 / squeeze
                    }
                    _ => 0.0,
                };
                for (style, piece) in &shaped {
                    let wanted = style
                        .checked_sub(1)
                        .and_then(|k| b.inline.get(k))
                        .map_or(b.style.color, |s| s.color);
                    if wanted != colour && !piece.runs.is_empty() {
                        colour = wanted;
                        writeln!(draw, "{} rg", ink(colour).join(" ")).unwrap();
                    }
                    for run in &piece.runs {
                        self.font_ref(run.face);
                        used_faces.insert(run.face);
                        let font = &fonts.faces()[run.face].font;
                        let embedder = &mut self.embedders.get_mut(&run.face).unwrap().0;
                        write_run(&mut draw, run, font, embedder, size, per_space);
                    }
                }
            }
            draw.push_str("ET Q\n");
            drawn += 1;
            if !entry.companion {
                report.replaced.push(b.id.clone());
            }
            match at {
                Some((at, _, _)) => insertions.push(Insertion {
                    source: Source::Page,
                    at,
                    data: draw.into_bytes(),
                }),
                None => drawn_last.push_str(&draw),
            }
        }
        let mut edited = edit(content, &removals, &insertions);

        if drawn == 0 {
            return Ok(report);
        }

        // Form XObjects that lost text.
        for (source, data) in edited.iter() {
            if let Source::Form(id) = source
                && self.forms_done.insert(*id)
            {
                let Object::Stream(original) = self.saver.get(*id)? else {
                    continue;
                };
                let mut dict = original.dict.clone();
                for key in ["Filter", "DecodeParms", "Length"] {
                    dict.remove(key.as_bytes());
                }
                dict.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
                self.saver.set(
                    *id,
                    Object::Stream(Stream {
                        dict,
                        data: celsus_pdf::filters::deflate(data),
                    }),
                );
            }
        }

        // The page: original content (edited) wrapped in q/Q so its graphics state cannot leak, then the new text.
        let original = match edited.remove(&Source::Page) {
            Some(d) => d,
            None => extracted
                .content
                .data
                .get(&Source::Page)
                .cloned()
                .unwrap_or_default(),
        };
        // Close any graphics states the original leaves open (and with them its clipping paths), then our own.
        let open = celsus_pdf::content::parse(&original)
            .iter()
            .fold(0usize, |depth, op| match op.operator.as_slice() {
                b"q" => depth + 1,
                b"Q" => depth.saturating_sub(1),
                _ => depth,
            });
        let mut content = b"q\n".to_vec();
        content.extend_from_slice(&original);
        content.push(b'\n');
        content.extend_from_slice(&b"Q\n".repeat(open + 1));
        content.extend_from_slice(drawn_last.as_bytes());
        let contents = self.saver.add(Object::Stream(Stream {
            dict: [(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()))]
                .into_iter()
                .collect(),
            data: celsus_pdf::filters::deflate(&content),
        }));

        let mut resources = page.resources.clone();
        let mut font_dict: Dict = self
            .doc
            .get_in(&resources, "Font")
            .and_then(|f| f.as_dict().cloned())
            .unwrap_or_default();
        for face in used_faces {
            let r = self.font_ref(face);
            font_dict.insert(format!("CelsusF{face}").into_bytes(), Object::Ref(r));
        }
        resources.insert(b"Font".to_vec(), Object::Dict(font_dict));
        let Object::Dict(mut page_dict) = self.saver.get(page.id)? else {
            return Err(celsus_pdf::Error::Corrupt(
                "page is not a dictionary".into(),
            ));
        };
        page_dict.insert(b"Resources".to_vec(), Object::Dict(resources));
        page_dict.insert(b"Contents".to_vec(), Object::Ref(contents));
        self.saver.set(page.id, Object::Dict(page_dict));
        Ok(report)
    }

    /// Put a redrawn picture (a JPEG or PNG) in place of the image XObject `id`. The page keeps drawing it in the same
    /// place and size, so nothing else changes.
    pub fn replace_image(&mut self, id: ObjRef, jpeg: &[u8]) -> celsus_pdf::Result<()> {
        let stream = celsus_pdf::image::xobject(jpeg).ok_or_else(|| {
            celsus_pdf::Error::Corrupt("redrawn picture is not a JPEG or PNG".into())
        })?;
        self.saver.set(id, Object::Stream(stream));
        Ok(())
    }

    /// Embed the fonts and produce the new file.
    pub fn finish(mut self) -> celsus_pdf::Result<Vec<u8>> {
        let mut faces: Vec<usize> = self.embedders.keys().copied().collect();
        faces.sort_unstable();
        for face in faces {
            let (embedder, target) = &self.embedders[&face];
            let written = &self.fonts.faces()[face];
            let name = if written.postscript.is_empty() {
                std::path::Path::new(&written.name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("CelsusFont{face}"))
            } else {
                written.postscript.clone()
            };
            embedder.write(&mut self.saver, &name, *target);
        }
        self.saver.save()
    }
}


/// A contents entry keeps its row of dots as long as the room allows: the translated title is longer or shorter
/// than the original one, so the dots between it and the page number are counted again for `width`.
/// The fonts the pages of a document name in their resources.
fn document_fonts(doc: &Document) -> Vec<Font> {
    // More than any book has; a file made to waste time stops here.
    const MOST: usize = 4096;
    let mut seen = BTreeSet::new();
    let mut fonts = Vec::new();
    for page in doc.pages().unwrap_or_default() {
        let Some(named) = doc.get_in(&page.resources, "Font") else {
            continue;
        };
        let Some(named) = named.as_dict() else {
            continue;
        };
        for entry in named.values() {
            let id = entry.as_ref();
            if fonts.len() >= MOST || id.is_some_and(|id| !seen.insert(id)) {
                continue;
            }
            if let Some(dict) = doc.resolve(entry).ok().as_deref().and_then(Object::as_dict) {
                fonts.push(Font::load(doc, dict, id));
            }
        }
    }
    fonts
}

/// The fonts of a block: of its own style, and of each style of the words set apart inside it. Text measured
/// with it says with marks where each style begins (see [`crate::inline`]).
struct Styled<'f> {
    shapers: Vec<Shaper<'f>>,
}

impl<'f> Styled<'f> {
    fn new(fonts: &'f FontSet, chains: &[Vec<usize>], rtl: bool) -> Self {
        Styled {
            shapers: chains
                .iter()
                .map(|chain| Shaper::new(fonts, chain.clone(), rtl))
                .collect(),
        }
    }

    /// The fonts of a style; the block's own where the text uses none of the others.
    fn of(&self, style: usize) -> &Shaper<'f> {
        self.shapers.get(style).unwrap_or(&self.shapers[0])
    }
}

impl Measure for Styled<'_> {
    fn width(&self, text: &str, size: f32) -> f32 {
        if self.shapers.len() == 1 {
            return self.shapers[0].width(text, size);
        }
        inline::pieces(text)
            .into_iter()
            .map(|(style, piece)| self.of(style).width(piece, size))
            .sum()
    }

    fn ascent(&self) -> f32 {
        self.shapers[0].ascent()
    }

    fn descent(&self) -> f32 {
        self.shapers[0].descent()
    }
}

fn refill_leader(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> Option<String> {
    let span = crate::layout::leader_span(text)?;
    let unit = if text[span.clone()].contains(char::is_whitespace) { " ." } else { "." };
    let head = text[..span.start].trim_end();
    let tail = text[span.end..].trim();
    let free = width - measure(head) - measure(tail) - measure(" ") * 2.0;
    let dots = ((free / measure(unit).max(0.1)) as usize).clamp(3, 400);
    let mut out = String::with_capacity(text.len());
    out.push_str(head);
    if unit == "." && !head.is_empty() {
        out.push(' ');
    }
    out.push_str(&unit.repeat(dots));
    if !tail.is_empty() {
        out.push(' ');
        out.push_str(tail);
    }
    Some(out)
}

/// Where to draw a block's translation: just after the text object (`ET`) that drew its last glyph in the page's
/// own content, with the transformation matrix and the clipping area in effect there. `None` (text inside a form
/// XObject) means drawing at the end of the page instead.
fn insertion_point(content: &PageContent, glyphs: &[GlyphRef]) -> Option<(usize, Matrix, Bounds)> {
    let runs: Vec<&TextRun> = glyphs.iter().map(|g| &content.runs[g.run]).collect();
    if runs.is_empty() || runs.iter().any(|r| r.source != Source::Page) {
        return None;
    }
    let last = runs.iter().max_by_key(|r| r.op)?;
    let ops = content.ops.get(&Source::Page)?;
    let et = ops
        .iter()
        .enumerate()
        .skip(last.op)
        .find(|(_, o)| o.operator == b"ET")?
        .0;
    // The matrix cannot change inside a text object, so the run's matrix still holds after its ET.
    Some((ops[et].end, last.ctm, last.clip))
}

/// Draw one shaped run: glyph ids in a `TJ` array, with adjustments wherever the shaped position differs from the
/// font's own advance (kerning, joined Arabic letters, marks), and marks raised or lowered with `Ts`.
fn write_run(
    draw: &mut String,
    run: &crate::shape::Run,
    font: &celsus_pdf::ttf::TrueType,
    embedder: &mut FontEmbedder,
    size: f32,
    per_space: f32,
) {
    let upem = f32::from(font.units_per_em);
    let thousandths = |pt: f32| -> String { real(f64::from(pt / size * 1000.0)) };
    writeln!(draw, "/CelsusF{} {} Tf", run.face, real(f64::from(size))).unwrap();
    let mut open = false;
    // Movement still owed before the next glyph, in points (positive moves right).
    let mut owed = 0.0f32;
    let flush_owed = |draw: &mut String, owed: &mut f32| {
        if owed.abs() > 0.001 {
            write!(draw, " {} ", thousandths(-*owed)).unwrap();
        }
        *owed = 0.0;
    };
    for g in &run.glyphs {
        let code = hex(&embedder.encode_glyph(g.gid, &g.text));
        let natural = f32::from(font.advance(g.gid)) * size / upem;
        owed += g.dx;
        if g.dy.abs() > 0.001 {
            // A raised or lowered mark: leave the array, set the rise, draw it, and come back.
            if open {
                draw.push_str("] TJ\n");
                open = false;
            }
            if owed.abs() > 0.001 {
                write!(draw, "[{}] TJ ", thousandths(-owed)).unwrap();
                owed = 0.0;
            }
            writeln!(draw, "{} Ts [<{code}>] TJ 0 Ts", real(f64::from(g.dy))).unwrap();
        } else {
            if !open {
                draw.push('[');
                open = true;
            }
            flush_owed(draw, &mut owed);
            write!(draw, "<{code}>").unwrap();
        }
        let extra = if g.text == " " { per_space } else { 0.0 };
        // After the glyph the pen sits at dx + natural; it should sit at advance (+ any justification).
        owed += g.advance + extra - natural - g.dx;
    }
    if open {
        flush_owed(draw, &mut owed);
        draw.push_str("] TJ\n");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}
