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
use crate::layout::{BlockId, Rect, TextBlock, TextStyle};
use crate::mirror;
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
    /// Looks at the pages, from the first page that is written on.
    eye: std::cell::OnceCell<crate::see::Eye>,
}

/// On a page with more bodies than this, none is moved: it is a page of many small things, not of figures.
const MOST_BODIES: usize = 80;

/// Strokes at least this thick, against the size of the text, are of a heavy face, whatever the font says of
/// itself. Measured on thirty-four faces at sizes from 11 to 30 points, the regular ones stay under 0.16 and
/// the black and heavy ones over 0.2; bold ones fall between, and say that they are bold.
const HEAVY: f32 = 0.185;
/// Text smaller than this, in points, is not judged by its strokes.
const HEAVY_FROM: f32 = 14.0;

/// On a page whose content is longer than this, nothing is moved.
const HEAVY_CONTENT: usize = 2_000_000;

/// A page with a picture of more pixels than this is not looked at: drawing the page means unpacking the
/// picture whole, which takes four bytes a pixel. A page scanned at 600 dots to the inch has fewer.
const MOST_PICTURE: i64 = 40_000_000;

/// Text set smaller than it was is drawn with a pen around its letters, this wide for every point of size it
/// lost. A typeface cut for small sizes is heavier by about as much.
const PEN: f32 = 0.04;

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
            eye: std::cell::OnceCell::new(),
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
        // A page of columns, set for a language read from right to left, has its columns change places. The
        // translations are placed on the page as it will be; the columns themselves are moved further down.
        let plan = (rtl && page.rotate % 360 == 0)
            .then(|| mirror::plan(&extracted.layout, &extracted.obstacles))
            .flatten();
        let mirrored = plan.as_ref().map(|plan| {
            (
                plan.moved(&extracted.layout),
                plan.moved_rects(&extracted.obstacles),
            )
        });
        let (layout, obstacles) = match &mirrored {
            Some((layout, obstacles)) => (layout, obstacles.as_slice()),
            None => (&extracted.layout, extracted.obstacles.as_slice()),
        };
        let in_column: BTreeSet<&BlockId> = plan.as_ref().map_or_else(BTreeSet::new, |plan| {
            extracted
                .layout
                .blocks
                .iter()
                .filter(|b| plan.shift(&b.rect).is_some())
                .map(|b| &b.id)
                .collect()
        });
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
            let text = if missing
                .iter()
                .any(|c| crate::extract::plain_form(*c).is_some())
            {
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
            // A contents entry has its row of dots counted again for the room it has: on its own, or as one of the
            // entries of a list.
            let refilled = (block.lines.len() == 1 || block.keeps_lines())
                .then(|| {
                    let measure = Styled::new(fonts, &chains, rtl);
                    let width = block.rect.x1 - block.rect.x0;
                    let mut any = false;
                    let lines: Vec<String> = rest
                        .split('\n')
                        .map(|line| {
                            let filled = refill_leader(line, width, |t| {
                                measure.width(t, block.style.font_size)
                            });
                            any |= filled.is_some();
                            filled.unwrap_or_else(|| line.to_owned())
                        })
                        .collect();
                    any.then(|| lines.join("\n"))
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

        let mut shapers: Vec<Styled> = entries
            .iter()
            .map(|e| {
                let mut shaper = Styled::new(fonts, &e.chains, rtl && !e.companion);
                // A title set in a condensed face is drawn narrow again, so that it takes the room it took.
                // Not for text from a scan: what the scan's text layer says of the widths is rough.
                if !e.companion && !rtl && !extracted.scans.contains(&e.block.id) {
                    shaper.narrow = shaper.narrowness(e.block).unwrap_or(1.0);
                }
                shaper
            })
            .collect();
        // The page as a reader sees it: what is on it besides the text to place. A page whose columns change
        // places, or that is shown turned, is taken the way its content describes it.
        let lines: Vec<Rect> = entries
            .iter()
            .flat_map(|e| e.block.lines.iter().map(|l| l.rect))
            .collect();
        let pictures_fit = || {
            let Some(objects) = self.doc.get_in(&page.resources, "XObject") else {
                return true;
            };
            let Some(objects) = objects.as_dict() else {
                return true;
            };
            objects.values().all(|o| {
                let Ok(o) = self.doc.resolve(o) else {
                    return true;
                };
                let Object::Stream(s) = &*o else {
                    return true;
                };
                let side = |key: &str| {
                    self.doc
                        .get_in(&s.dict, key)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                        .max(0)
                };
                side("Width").saturating_mul(side("Height")) <= MOST_PICTURE
            })
        };
        let picture =
            (plan.is_none() && page.rotate % 360 == 0 && !lines.is_empty() && pictures_fit())
                .then(|| {
                    let eye = self.eye.get_or_init(|| {
                        let pages = self.doc.pages().map_or(0, |p| p.len());
                        crate::see::Eye::open(&self.doc.data, pages)
                    });
                    eye.look(layout.page as usize - 1, layout.width, layout.height)
                })
                .flatten();
        let staying: Vec<Rect> = layout
            .blocks
            .iter()
            .filter(|b| !entries.iter().any(|e| e.block.id == b.id))
            .map(|b| b.rect)
            .collect();
        // What it shows: where the ink of each line really is, the rows of dots that lead on from lines, and
        // the bodies. A picture that does not show the text where the content has it is not a picture of this
        // page as it will be read, and is not gone by.
        let seen = picture
            .map(|p| (p.find(&lines, &staying), p))
            .filter(|(seen, _)| seen.inks.iter().flatten().count() * 2 >= lines.len());
        let (picture, inks, leaders, found) = match seen {
            Some((seen, p)) => (Some(p), seen.inks, seen.leaders, seen.bodies),
            None => (None, Vec::new(), Vec::new(), Vec::new()),
        };
        // A face can look heavy and say it is regular: a display face drawn heavy is made that way and named
        // for nothing else. What the picture shows decides, and the translation is set in bold.
        if let Some(picture) = &picture {
            let mut at = 0;
            for (k, e) in entries.iter_mut().enumerate() {
                let lines = at..at + e.block.lines.len();
                at = lines.end;
                let style = &e.block.style;
                // Small text is drawn too few pixels high for its strokes to be told apart: a regular face can
                // look as thick as a heavy one there. Titles and headings, where heavy faces are used, are
                // large enough.
                if e.companion
                    || style.bold
                    || style.monospace
                    || style.font_size < HEAVY_FROM
                    || e.chains.is_empty()
                {
                    continue;
                }
                let mut strokes: Vec<f32> = inks[lines]
                    .iter()
                    .flatten()
                    .filter_map(|ink| picture.stroke(ink))
                    .collect();
                if strokes.is_empty() {
                    continue;
                }
                strokes.sort_by(f32::total_cmp);
                if strokes[strokes.len() / 2] < style.font_size * HEAVY {
                    continue;
                }
                let bold = TextStyle {
                    bold: true,
                    ..style.clone()
                };
                let chain = fonts.chain_for(&bold, script);
                let text = inline::strip_marks(&e.text);
                if chain.is_empty()
                    || !Shaper::new(fonts, chain.clone(), rtl)
                        .missing(&text)
                        .is_empty()
                {
                    continue;
                }
                e.chains[0] = chain;
                let mut shaper = Styled::new(fonts, &e.chains, rtl);
                if !rtl && !extracted.scans.contains(&e.block.id) {
                    shaper.narrow = shaper.narrowness(e.block).unwrap_or(1.0);
                }
                shapers[k] = shaper;
            }
        }
        let papers: Vec<Option<[u8; 3]>> = found
            .iter()
            .map(|r| picture.as_ref().and_then(|p| p.paper_around(r)))
            .collect();
        // The content of the page is written once more for each distance something moves down.
        let light = extracted
            .content
            .data
            .get(&Source::Page)
            .is_none_or(|d| d.len() <= HEAVY_CONTENT);
        let bodies: Vec<place::Body> = found
            .iter()
            .zip(&papers)
            .map(|(rect, paper)| place::Body {
                rect: *rect,
                movable: paper.is_some() && found.len() <= MOST_BODIES && light,
            })
            .collect();
        // On a scanned page nothing but the picture says where the figures are.
        let seen_obstacles: Vec<Rect>;
        let obstacles = if extracted.scans.is_empty() || picture.is_none() {
            obstacles
        } else {
            seen_obstacles = obstacles.iter().chain(&found).copied().collect();
            seen_obstacles.as_slice()
        };
        let items: Vec<place::Item> = entries
            .iter()
            .zip(&shapers)
            .map(|(e, shaper)| place::Item {
                block: e.block,
                text: &e.text,
                measure: shaper,
                companion: e.companion,
                initial: e
                    .block
                    .initial
                    .filter(|_| !e.initial.is_empty())
                    .map(|cap| {
                        let width = shaper.width(&e.initial, cap.size);
                        crate::layout::Rect {
                            x1: cap.rect.x0 + width,
                            ..cap.rect
                        }
                    }),
                // What a scanned figure holds inside its box is not known: text in there stays in its place.
                confined: extracted.scans.contains(&e.block.id)
                    && (picture.is_none()
                        || bodies
                            .iter()
                            .any(|body| place::within(&body.rect, &e.block.rect))),
            })
            .collect();
        let solved = place::solve_among(layout, obstacles, &bodies, &items, rtl);
        drop(items);
        let placed = solved.placed;
        // The bodies that moved: where each was, with a point of the paper around it, how far down it went (or
        // up, counted below nothing), and the colour of the paper it leaves behind.
        let lowered = |r: &Rect, by: f32| Rect {
            y0: r.y0 + by,
            y1: r.y1 + by,
            ..*r
        };
        let moves: Vec<(Rect, f32, [u8; 3])> = bodies
            .iter()
            .zip(&solved.moved)
            .zip(&papers)
            .filter(|((_, by), _)| by.abs() > 0.3)
            .map(|((body, by), paper)| {
                let r = &body.rect;
                let was = Rect {
                    x0: r.x0 - 1.0,
                    y0: r.y0 - 1.0,
                    x1: r.x1 + 1.0,
                    y1: r.y1 + 1.0,
                };
                (was, *by, paper.unwrap_or([255; 3]))
            })
            .collect();

        // New text. Each block is drawn where its original text was drawn (right after the text object that held
        // it), so it keeps its depth: text that a later drawing covered stays covered.
        let [cx0, _, _, cy1] = page.crop_box;
        let content = &extracted.content;
        let mut removals: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        let mut drawn_last = String::new();
        // The white that covers the words of a scanned picture where translations go. All of it is laid down
        // before any of the new text, so that no cover falls on a translation written just before it.
        let mut whites = String::new();
        // The same for text that stands inside a body that moved: laid down once the body is in its new place.
        let mut whites_after = String::new();
        let mut insertions: Vec<Insertion> = Vec::new();
        let mut used_faces = BTreeSet::new();
        let mut drawn = 0usize;
        let mut first_line = 0usize;
        for (k, ((entry, shaper), placed)) in entries.iter().zip(&shapers).zip(&placed).enumerate()
        {
            let b = entry.block;
            let line0 = first_line;
            first_line += b.lines.len();
            let carried = solved.carried[k];
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
            // The letters are drawn as narrow as the placement measured them.
            let (size, squeeze) = (placed.size, placed.squeeze * shaper.narrow);
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
            // Text of a column that moved is drawn after the page's own content as well: the place its original
            // was drawn in belongs to the column, and that is drawn somewhere else.
            // So is text read from a scanned page: it goes on top of the picture, over the picture's own words.
            let scan = extracted.scans.contains(&b.id);
            // And text in or near the old or the new place of a body that moved: the paper painted over the old
            // place, and the body drawn in its new one, come after the page's own content.
            let reach = placed.lines.iter().fold(b.rect, |r, l| {
                r.union(&Rect {
                    x0: l.x,
                    y0: l.baseline - size,
                    x1: l.x + l.width,
                    y1: l.baseline + size * 0.3,
                })
            });
            let by_a_move = carried > 0.3
                || moves
                    .iter()
                    .any(|(was, by, _)| was.union(&lowered(was, *by)).overlaps(&reach, -2.0));
            let at = if in_column.contains(&b.id) || scan || by_a_move {
                None
            } else {
                insertion_point(content, refs)
            };
            let at = at.filter(|(_, _, clip)| {
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
            // Smaller letters have thinner strokes and look paler than the text around them. They are drawn a
            // little heavier and a little darker, the more so the smaller they got, so that they read as well as
            // they did.
            let lost = (b.style.font_size - size).max(0.0);
            let pen = if lost > b.style.font_size * 0.02 {
                PEN * lost
            } else {
                0.0
            };
            let darker = if pen > 0.0 {
                f64::from(size / b.style.font_size)
            } else {
                1.0
            };
            let ink = |c: u32| {
                [(c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF]
                    .map(|v| real(f64::from(v) / 255.0 * darker))
            };
            let rgb = ink(b.style.color);
            // Inserted right after an operator: start on a new line so the two never run together.
            let mut draw = String::from("\nq ");
            if let Some((_, ctm, _)) = &at
                && let Some(inv) = invert(ctm)
            {
                writeln!(draw, "{} cm", inv.map(real).join(" ")).unwrap();
            }
            if scan {
                // The words of the original are in the picture and cannot be taken out of it: they are covered
                // with the white of the paper, each line with a little room around it for the strokes that reach
                // above and below. A page scanned in one bit a pixel has no other white.
                // Where the page was looked at, the cover is the box of the ink that was seen there.
                let (dx, dy) = (b.style.font_size * 0.3, b.style.font_size * 0.22);
                let out = if carried > 0.3 {
                    &mut whites_after
                } else {
                    &mut whites
                };
                for (n, line) in b.lines.iter().enumerate() {
                    let r = match inks.get(line0 + n) {
                        Some(Some(ink)) => Rect {
                            x0: ink.x0.min(line.rect.x0) - 1.0,
                            y0: ink.y0.min(line.rect.y0) - 1.0,
                            x1: ink.x1.max(line.rect.x1) + 1.0,
                            y1: ink.y1.max(line.rect.y1) + 1.0,
                        },
                        Some(None) => line.rect,
                        None => Rect {
                            x0: line.rect.x0 - dx,
                            y0: line.rect.y0 - dy,
                            x1: line.rect.x1 + dx,
                            y1: line.rect.y1 + dy,
                        },
                    };
                    let mut cover = |r: &Rect| {
                        writeln!(
                            out,
                            "{} {} {} {} re",
                            real(cx0 + f64::from(r.x0)),
                            real(cy1 - f64::from(r.y1 + carried)),
                            real(f64::from(r.x1 - r.x0)),
                            real(f64::from(r.y1 - r.y0))
                        )
                        .unwrap();
                    };
                    cover(&r);
                    // A row of dots that led on from the line gives way to a longer line: as much of it is
                    // covered as the new text takes, and a little more, so that the dots begin after a space.
                    if let Some(Some(dots)) = leaders.get(line0 + n) {
                        let reach = placed
                            .lines
                            .iter()
                            .filter(|l| {
                                let foot = l.baseline - placed.shift;
                                foot > dots.y0 - size && foot < dots.y1 + size * 0.5
                            })
                            .map(|l| l.x + l.width + size * 0.4)
                            .fold(0.0, f32::max);
                        if reach > dots.x0 {
                            cover(&Rect {
                                x0: dots.x0 - 1.0,
                                y0: dots.y0 - 1.0,
                                x1: reach.min(dots.x1 + 1.0),
                                y1: dots.y1 + 1.0,
                            });
                        }
                    }
                }
            }
            // Text state is part of the graphics state the original may have left set.
            writeln!(
                draw,
                "{} {} {} rg 0 Tc 0 Tw {} Tz 0 Ts 0 Tr",
                rgb[0],
                rgb[1],
                rgb[2],
                real(f64::from(squeeze * 100.0))
            )
            .unwrap();
            if pen > 0.0 {
                writeln!(
                    draw,
                    "{} RG {} w 1 j [] 0 d",
                    rgb.join(" "),
                    real(f64::from(pen))
                )
                .unwrap();
            }
            draw.push_str("BT\n");
            if let Some(cap) = b.initial.filter(|_| !entry.initial.is_empty()) {
                let shaped = shaper.of(0).shape_line(&entry.initial, cap.size);
                let (px, py) = (
                    cx0 + f64::from(cap.rect.x0),
                    cy1 - f64::from(cap.rect.y1 + placed.shift),
                );
                writeln!(
                    draw,
                    "{} Tz 1 0 0 1 {} {} Tm",
                    real(f64::from(shaper.narrow * 100.0)),
                    real(px),
                    real(py)
                )
                .unwrap();
                for run in &shaped.runs {
                    self.font_ref(run.face);
                    used_faces.insert(run.face);
                    let font = &fonts.faces()[run.face].font;
                    let embedder = &mut self.embedders.get_mut(&run.face).unwrap().0;
                    write_run(&mut draw, run, font, embedder, cap.size, 0.0);
                }
                writeln!(draw, "{} Tz", real(f64::from(squeeze * 100.0))).unwrap();
            }
            // The pen is for the text that got smaller, not for an initial that kept its size.
            if pen > 0.0 {
                draw.push_str("2 Tr\n");
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
                        let c = ink(colour).join(" ");
                        if pen > 0.0 {
                            writeln!(draw, "{c} rg {c} RG").unwrap();
                        } else {
                            writeln!(draw, "{c} rg").unwrap();
                        }
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
        // On a mirrored page the page's own content is drawn once for what stays and once for each column, and
        // each time without the text of the others, so that no text is in the file twice.
        let without = |plan: &mirror::Plan, column: Option<usize>| {
            let mut out = removals.clone();
            for (r, run) in content.runs.iter().enumerate() {
                if run.source != Source::Page {
                    continue;
                }
                for (g, glyph) in run.glyphs.iter().enumerate() {
                    let [x0, y0, x1, y1] = glyph.bounds;
                    let (x, y) = ((x0 + x1) / 2.0 - cx0 as f32, cy1 as f32 - (y0 + y1) / 2.0);
                    if plan.column_at(x, y) != column {
                        out.entry(r).or_default().insert(g);
                    }
                }
            }
            out
        };
        // On a page where bodies moved, the page's own content is drawn once for what stays and once for each
        // distance something went down, and here too no text is in the file twice. `step` is that distance, or
        // `None` for what stays.
        let steps: Vec<f32> = {
            let mut steps: Vec<f32> = moves.iter().map(|(_, by, _)| *by).collect();
            steps.sort_by(f32::total_cmp);
            steps.dedup_by(|a, b| (*a - *b).abs() < 0.01);
            steps
        };
        let step_of = |by: f32| steps.iter().position(|s| (s - by).abs() < 0.01);
        let apart = |step: Option<usize>| {
            let mut out = removals.clone();
            for (r, run) in content.runs.iter().enumerate() {
                if run.source != Source::Page {
                    continue;
                }
                for (g, glyph) in run.glyphs.iter().enumerate() {
                    let [x0, y0, x1, y1] = glyph.bounds;
                    let (x, y) = ((x0 + x1) / 2.0 - cx0 as f32, cy1 as f32 - (y0 + y1) / 2.0);
                    let of = moves
                        .iter()
                        .find(|(was, _, _)| {
                            x >= was.x0 && x <= was.x1 && y >= was.y0 && y <= was.y1
                        })
                        .and_then(|(_, by, _)| step_of(*by));
                    if of != step {
                        out.entry(r).or_default().insert(g);
                    }
                }
            }
            out
        };
        let mut edited = match &plan {
            Some(plan) => edit(content, &without(plan, None), &insertions),
            None if !moves.is_empty() => edit(content, &apart(None), &insertions),
            None => edit(content, &removals, &insertions),
        };

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
        let wrapped = |out: &mut Vec<u8>, data: &[u8]| {
            let open = celsus_pdf::content::parse(data)
                .iter()
                .fold(0usize, |depth, op| match op.operator.as_slice() {
                    b"q" => depth + 1,
                    b"Q" => depth.saturating_sub(1),
                    _ => depth,
                });
            out.extend_from_slice(b"q\n");
            out.extend_from_slice(data);
            out.push(b'\n');
            out.extend_from_slice(&b"Q\n".repeat(open + 1));
        };
        let mut stream = Vec::new();
        match &plan {
            None => wrapped(&mut stream, &original),
            Some(plan) => {
                // The strip of a column in each stretch of the page where the columns stand, moved by `dx`.
                let strips = |column: &mirror::Column, dx: f32| -> String {
                    plan.bands
                        .iter()
                        .map(|&(y0, y1)| {
                            format!(
                                "{} {} {} {} re\n",
                                real(cx0 + f64::from(column.x0 + dx)),
                                real(cy1 - f64::from(y1)),
                                real(f64::from(column.x1 - column.x0)),
                                real(f64::from(y1 - y0))
                            )
                        })
                        .collect()
                };
                // What stays: everything but the columns.
                let mut rest = String::from("q\n-32768 -32768 65536 65536 re\n");
                for column in &plan.columns {
                    rest.push_str(&strips(column, 0.0));
                }
                rest.push_str("W* n\n");
                stream.extend_from_slice(rest.as_bytes());
                wrapped(&mut stream, &original);
                stream.extend_from_slice(b"Q\n");
                // Each column, in its new place.
                for (i, column) in plan.columns.iter().enumerate() {
                    let own = edit(content, &without(plan, Some(i)), &[])
                        .remove(&Source::Page)
                        .unwrap_or_default();
                    let lead = format!(
                        "q\n{}W n\n1 0 0 1 {} 0 cm\n",
                        strips(column, column.dx),
                        real(f64::from(column.dx))
                    );
                    stream.extend_from_slice(lead.as_bytes());
                    wrapped(&mut stream, &own);
                    stream.extend_from_slice(b"Q\n");
                }
            }
        }
        // The old place of each body that moved is painted over with the paper around it.
        for (was, _, paper) in &moves {
            let colour = paper.map(|v| real(f64::from(v) / 255.0)).join(" ");
            let fill = format!(
                "q {colour} rg {} {} {} {} re f Q\n",
                real(cx0 + f64::from(was.x0)),
                real(cy1 - f64::from(was.y1)),
                real(f64::from(was.x1 - was.x0)),
                real(f64::from(was.y1 - was.y0))
            );
            stream.extend_from_slice(fill.as_bytes());
        }
        if !whites.is_empty() {
            stream.extend_from_slice(format!("q 1 1 1 rg\n{whites}f Q\n").as_bytes());
        }
        // Then each body in its new place: the page's content once more, moved down, and of it only what shows
        // through the new places of the bodies that went this far.
        for (n, step) in steps.iter().enumerate() {
            let mut lead = String::from("q\n");
            for (was, _, _) in moves.iter().filter(|(_, by, _)| step_of(*by) == Some(n)) {
                writeln!(
                    lead,
                    "{} {} {} {} re",
                    real(cx0 + f64::from(was.x0)),
                    real(cy1 - f64::from(was.y1 + step)),
                    real(f64::from(was.x1 - was.x0)),
                    real(f64::from(was.y1 - was.y0))
                )
                .unwrap();
            }
            writeln!(lead, "W n\n1 0 0 1 0 {} cm", real(f64::from(-step))).unwrap();
            let own = edit(content, &apart(Some(n)), &[])
                .remove(&Source::Page)
                .or_else(|| extracted.content.data.get(&Source::Page).cloned())
                .unwrap_or_default();
            stream.extend_from_slice(lead.as_bytes());
            wrapped(&mut stream, &own);
            stream.extend_from_slice(b"Q\n");
        }
        if !whites_after.is_empty() {
            stream.extend_from_slice(format!("q 1 1 1 rg\n{whites_after}f Q\n").as_bytes());
        }
        stream.extend_from_slice(drawn_last.as_bytes());
        let contents = self.saver.add(Object::Stream(Stream {
            dict: [(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()))]
                .into_iter()
                .collect(),
            data: celsus_pdf::filters::deflate(&stream),
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
        if let Some(plan) = &plan {
            self.move_annotations(&mut page_dict, plan, cx0, cy1)?;
        }
        self.saver.set(page.id, Object::Dict(page_dict));
        Ok(report)
    }

    /// Links and notes that stand in a column go with it.
    fn move_annotations(
        &mut self,
        page: &mut Dict,
        plan: &mirror::Plan,
        cx0: f64,
        cy1: f64,
    ) -> celsus_pdf::Result<()> {
        let mut list = match page.get(b"Annots".as_slice()) {
            Some(Object::Array(list)) => list.clone(),
            Some(Object::Ref(id)) => match self.saver.get(*id)? {
                Object::Array(list) => list,
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        let slide = |annotation: &mut Dict| -> bool {
            let numbers = |key: &str| {
                celsus_pdf::object::get(annotation, key).and_then(celsus_pdf::object::numbers)
            };
            let Some(rect) = numbers("Rect").filter(|r| r.len() == 4) else {
                return false;
            };
            let (x, y) = (
                (rect[0] + rect[2]) / 2.0 - cx0,
                cy1 - (rect[1] + rect[3]) / 2.0,
            );
            let Some(column) = plan.column_at(x as f32, y as f32) else {
                return false;
            };
            let dx = f64::from(plan.columns[column].dx);
            // Every other number is a distance from the left.
            let slid = |values: Vec<f64>| {
                Object::Array(
                    values
                        .iter()
                        .enumerate()
                        .map(|(i, v)| Object::Real(if i % 2 == 0 { v + dx } else { *v }))
                        .collect(),
                )
            };
            let quads = numbers("QuadPoints");
            annotation.insert(b"Rect".to_vec(), slid(rect));
            if let Some(quads) = quads {
                annotation.insert(b"QuadPoints".to_vec(), slid(quads));
            }
            true
        };
        let mut direct = false;
        for entry in &mut list {
            match entry {
                Object::Ref(id) => {
                    if let Object::Dict(mut annotation) = self.saver.get(*id)?
                        && slide(&mut annotation)
                    {
                        self.saver.set(*id, Object::Dict(annotation));
                    }
                }
                Object::Dict(annotation) => direct |= slide(annotation),
                _ => {}
            }
        }
        if direct {
            page.insert(b"Annots".to_vec(), Object::Array(list));
        }
        Ok(())
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
        let mut saver = std::mem::replace(&mut self.saver, Saver::new(self.doc));
        self.embed(&mut saver);
        saver.save()
    }

    /// The file as it stands, with the pages translated so far, while the writer goes on. For showing a long
    /// translation as it grows.
    pub fn snapshot(&self) -> celsus_pdf::Result<Vec<u8>> {
        // The fonts go into a copy: they are written again, with more glyphs, the next time.
        let mut saver = self.saver.clone();
        self.embed(&mut saver);
        saver.save()
    }

    /// Put the fonts used so far, each cut down to the glyphs it needed, into `saver`.
    fn embed(&self, saver: &mut Saver) {
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
            embedder.write(saver, &name, *target);
        }
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
    /// How much narrower the letters are drawn than the fonts have them: the original was set in a condensed
    /// face, and none of the fonts is one.
    narrow: f32,
}

/// Original letters narrower than this, against the letters of the font that stands in for them, are of a
/// condensed face. Less than that is the ordinary difference between two faces of one kind.
const CONDENSED: f32 = 0.85;
/// Letters are never drawn narrower than this.
const NARROWEST: f32 = 0.72;

impl<'f> Styled<'f> {
    fn new(fonts: &'f FontSet, chains: &[Vec<usize>], rtl: bool) -> Self {
        Styled {
            shapers: chains
                .iter()
                .map(|chain| Shaper::new(fonts, chain.clone(), rtl))
                .collect(),
            narrow: 1.0,
        }
    }

    /// How wide the original letters of `block` are against these fonts' letters for the same words, when the
    /// original face is clearly the narrower: the middle of what its lines say. `None` when they are about as
    /// wide, or the fonts lack some of the original's letters.
    fn narrowness(&self, block: &TextBlock) -> Option<f32> {
        let size = block.style.font_size;
        let mut ratios: Vec<f32> = block
            .lines
            .iter()
            // Lines of words: a row of leader dots, or letters spaced out, say nothing of the face.
            .filter(|l| {
                let all = l.text.trim().chars().count();
                let letters = l.text.chars().filter(|c| c.is_alphabetic()).count();
                all >= 4 && letters * 10 >= all * 7
            })
            .filter(|l| self.shapers[0].missing(&l.text).is_empty())
            .filter_map(|l| {
                let wide = self.shapers[0].width(l.text.trim(), size);
                (wide > 0.0).then(|| l.rect.width() / wide)
            })
            .collect();
        if ratios.is_empty() {
            return None;
        }
        ratios.sort_by(f32::total_cmp);
        let ratio = ratios[ratios.len() / 2];
        // Much narrower than that is not a face but something else: letters drawn in another size than the
        // one read, or squeezed by the document itself.
        (ratio < CONDENSED && ratio > NARROWEST * 0.85).then_some(ratio.max(NARROWEST))
    }

    /// The fonts of a style; the block's own where the text uses none of the others.
    fn of(&self, style: usize) -> &Shaper<'f> {
        self.shapers.get(style).unwrap_or(&self.shapers[0])
    }
}

impl Measure for Styled<'_> {
    fn width(&self, text: &str, size: f32) -> f32 {
        if self.shapers.len() == 1 {
            return self.shapers[0].width(text, size) * self.narrow;
        }
        inline::pieces(text)
            .into_iter()
            .map(|(style, piece)| self.of(style).width(piece, size))
            .sum::<f32>()
            * self.narrow
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
    let unit = if text[span.clone()].contains(char::is_whitespace) {
        " ."
    } else {
        "."
    };
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
