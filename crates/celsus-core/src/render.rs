//! Writing translations into a PDF.
//!
//! For every translated block the original glyphs are taken out of the content stream (nothing else on the page
//! is touched) and the translation is drawn in its place with the closest bundled font, at the original size and
//! colour. A translation that is longer than the original is wrapped and, if needed, set slightly smaller, but
//! never below a readable size: such a block keeps its original text and is reported instead.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use celsus_pdf::edit::{Insertion, edit};
use celsus_pdf::embed::FontEmbedder;
use celsus_pdf::interp::{Matrix, PageContent, Source, TextRun, invert};
use celsus_pdf::write::{Saver, real};
use celsus_pdf::{Dict, Document, ObjRef, Object, Page, Stream};
use serde::{Deserialize, Serialize};

use crate::extract::{ExtractedPage, GlyphRef};
use crate::fit::{self, FitOptions, Measure};
use crate::fonts::FontSet;
use crate::layout::{Align, BlockId, PageLayout, Rect, TextBlock};
use crate::shape::Shaper;

/// Translations may shrink to 70% of the original size, and never below 6 pt.
pub const READABLE: FitOptions = FitOptions {
    min_scale: 0.7,
    min_size: 6.0,
    line_height: 1.15,
    step: 0.25,
};

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
    fonts: &'d FontSet,
    saver: Saver<'d>,
    embedders: HashMap<usize, (FontEmbedder, ObjRef)>,
    /// Form XObjects already rewritten (shared forms are edited once).
    forms_done: BTreeSet<ObjRef>,
}

struct Plan<'b> {
    block: &'b TextBlock,
    /// Fonts for this block, best first (see [`FontSet::chain_for`]).
    chain: Vec<usize>,
    fitted: fit::Fitted,
}

impl<'d> BookWriter<'d> {
    pub fn new(doc: &'d Document, fonts: &'d FontSet) -> Self {
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
        let mut report = PageReport {
            page: layout.page,
            ..Default::default()
        };
        let mut plans = Vec::new();
        for (id, text) in translations {
            let text = text.trim();
            let Some(block) = layout.blocks.iter().find(|b| &b.id == id) else {
                report.kept.push(Issue::UnknownBlock { id: id.clone() });
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let chain = self.fonts.chain_for(&block.style, script);
            if chain.is_empty() {
                report.kept.push(Issue::MissingGlyphs {
                    id: id.clone(),
                    chars: text.into(),
                });
                continue;
            }
            let shaper = Shaper::new(self.fonts, chain.clone(), rtl);
            // No font for some character: the original stays, and the report says which characters were missing,
            // rather than printing gaps.
            let missing = shaper.missing(text);
            if !missing.is_empty() {
                report.kept.push(Issue::MissingGlyphs {
                    id: id.clone(),
                    chars: missing.into_iter().collect(),
                });
                continue;
            }
            let leading = (block.lines.len() > 1).then(|| {
                (block.lines.last().unwrap().baseline - block.lines[0].baseline)
                    / (block.lines.len() - 1) as f32
            });
            // Free space below the block is used before the text is made smaller, keeping some paragraph spacing.
            let room = (next_block_gap(layout, &extracted.obstacles, block)
                - block.style.font_size * 0.5)
                .max(0.0);
            let indent = indent_of(block);
            let width =
                available_width(layout, &extracted.obstacles, block).max(block.style.font_size);
            let measure: &dyn Measure = &shaper;
            let line_box = (block.lines[0].rect.height() / block.style.font_size).clamp(0.6, 1.5);
            let area = fit::Area {
                width,
                height: block.rect.height() + room,
                indent,
                line_box: Some(line_box),
            };
            // A one-line heading, label or running head stays on one line if a readable size allows it.
            let single = (block.lines.len() == 1)
                .then(|| {
                    let one_line = fit::Area {
                        height: block.rect.height(),
                        ..area
                    };
                    fit::fit(
                        text,
                        one_line,
                        block.style.font_size,
                        leading,
                        measure,
                        READABLE,
                    )
                })
                .flatten();
            match single.or_else(|| {
                fit::fit(
                    text,
                    area,
                    block.style.font_size,
                    leading,
                    measure,
                    READABLE,
                )
            }) {
                Some(fitted) => plans.push(Plan {
                    block,
                    chain,
                    fitted,
                }),
                None => report.kept.push(Issue::DoesNotFit { id: id.clone() }),
            }
        }

        // Glyphs to take out, per run.
        let mut removals: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        for plan in &plans {
            for g in extracted.glyphs.get(&plan.block.id).into_iter().flatten() {
                removals.entry(g.run).or_default().insert(g.glyph);
            }
        }
        // New text. Each block is drawn where its original text was drawn (right after the text object that held
        // it), so it keeps its depth: text that a later drawing covered stays covered.
        let [cx0, _, _, cy1] = page.crop_box;
        let content = &extracted.content;
        let mut drawn_last = String::new();
        let mut insertions: Vec<Insertion> = Vec::new();
        let mut used_faces = BTreeSet::new();
        for plan in &plans {
            let refs = extracted
                .glyphs
                .get(&plan.block.id)
                .map_or(&[][..], Vec::as_slice);
            let place = insertion_point(content, refs);
            let mut draw = String::new();
            let shaper = Shaper::new(self.fonts, plan.chain.clone(), rtl);
            let b = plan.block;
            let size = plan.fitted.size;
            let align = match b.align {
                Align::Left | Align::Justify if rtl => Align::Right,
                // Separate entries are never stretched to the full measure.
                Align::Justify if b.keeps_lines() => Align::Left,
                a => a,
            };
            // Entries of a table of contents keep each line's own indent when the line count is unchanged.
            let own_indents = !rtl && b.keeps_lines() && plan.fitted.lines.len() == b.lines.len();
            let c = b.style.color;
            let rgb =
                [(c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF].map(|v| real(f64::from(v) / 255.0));
            let width = available_width(layout, &extracted.obstacles, b).max(b.style.font_size);
            // Keep the first baseline where it was, scaled with the size.
            let first = b.rect.y0 + (b.lines[0].baseline - b.rect.y0) * size / b.style.font_size;
            // Inserted right after an operator: start on a new line so the two never run together.
            draw.push_str("\nq ");
            if let Some((_, ctm)) = &place
                && let Some(inv) = invert(ctm)
            {
                writeln!(draw, "{} cm", inv.map(real).join(" ")).unwrap();
            }
            // Text state is part of the graphics state the original may have left set.
            writeln!(
                draw,
                "{} {} {} rg 0 Tc 0 Tw 100 Tz 0 Ts 0 Tr BT",
                rgb[0], rgb[1], rgb[2]
            )
            .unwrap();
            let last = plan.fitted.lines.len() - 1;
            for (n, line) in plan.fitted.lines.iter().enumerate() {
                let shaped = shaper.shape_line(line, size);
                let w = shaped.width;
                let indent = if own_indents {
                    b.lines[n].rect.x0 - b.rect.x0
                } else if rtl {
                    0.0
                } else {
                    indent_of(b).of_line(n)
                };
                let x = match align {
                    Align::Left | Align::Justify => b.rect.x0 + indent,
                    Align::Center => (b.rect.x0 + b.rect.x1 - w) / 2.0,
                    Align::Right => b.rect.x1 - w,
                };
                let baseline = first + plan.fitted.leading * n as f32;
                let (px, py) = (cx0 + f64::from(x), cy1 - f64::from(baseline));
                writeln!(draw, "1 0 0 1 {} {} Tm", real(px), real(py)).unwrap();
                // Justified lines spread the free space over their spaces.
                let spaces = shaped
                    .runs
                    .iter()
                    .flat_map(|r| &r.glyphs)
                    .filter(|g| g.text == " ")
                    .count();
                let stretch = width - indent - w;
                let per_space = if align == Align::Justify
                    && n < last
                    && spaces > 0
                    && stretch > 0.0
                    && stretch < width * 0.25
                {
                    stretch / spaces as f32
                } else {
                    0.0
                };
                for run in &shaped.runs {
                    self.font_ref(run.face);
                    used_faces.insert(run.face);
                    let font = &self.fonts.faces()[run.face].font;
                    let embedder = &mut self.embedders.get_mut(&run.face).unwrap().0;
                    write_run(&mut draw, run, font, embedder, size, per_space);
                }
            }
            draw.push_str("ET Q\n");
            report.replaced.push(b.id.clone());
            match place {
                Some((at, _)) => insertions.push(Insertion {
                    source: Source::Page,
                    at,
                    data: draw.into_bytes(),
                }),
                None => drawn_last.push_str(&draw),
            }
        }
        let mut edited = edit(content, &removals, &insertions);

        if plans.is_empty() {
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
            let name = std::path::Path::new(&self.fonts.faces()[face].name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("CelsusFont{face}"));
            embedder.write(&mut self.saver, &name, *target);
        }
        self.saver.save()
    }
}

/// First-line and hanging indents of a block, measured from its left edge.
fn indent_of(block: &TextBlock) -> fit::Indent {
    if block.lines.len() < 2 || !matches!(block.align, Align::Left | Align::Justify) {
        return fit::Indent::default();
    }
    let size = block.style.font_size;
    let first = block.lines[0].rect.x0 - block.rect.x0;
    let mut rest: Vec<f32> = block.lines[1..]
        .iter()
        .map(|l| l.rect.x0 - block.rect.x0)
        .collect();
    rest.sort_by(f32::total_cmp);
    let rest = rest[rest.len() / 2];
    let keep = |v: f32| {
        if v > size * 0.4 && v < size * 6.0 {
            v
        } else {
            0.0
        }
    };
    fit::Indent {
        first: keep(first),
        rest: keep(rest),
    }
}

/// Where to draw a block's translation: just after the text object (`ET`) that drew its last glyph in the page's
/// own content, with the transformation matrix in effect there. `None` (text inside a form XObject) means
/// drawing at the end of the page instead.
fn insertion_point(content: &PageContent, glyphs: &[GlyphRef]) -> Option<(usize, Matrix)> {
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
    Some((ops[et].end, last.ctm))
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

/// How wide the translation of `block` may be. A line can grow into the empty space beside it, in the direction
/// its alignment allows, up to the nearest text at the same height or the edge of the page's text area.
pub fn available_width(layout: &PageLayout, obstacles: &[Rect], block: &TextBlock) -> f32 {
    let r = &block.rect;
    let gap = block.style.font_size * 0.5;
    let content_left = layout.blocks.iter().map(|b| b.rect.x0).fold(r.x0, f32::min);
    let content_right = layout.blocks.iter().map(|b| b.rect.x1).fold(r.x1, f32::max);
    let others = layout
        .blocks
        .iter()
        .filter(|o| o.id != block.id)
        .map(|o| o.rect);
    let beside: Vec<Rect> = others
        .chain(obstacles.iter().copied())
        .filter(|o| o.y0 < r.y1 && o.y1 > r.y0)
        .collect();
    // A box drawn around the block limits it at its inner edges.
    let enclosing = obstacles.iter().filter(|o| {
        o.x0 <= r.x0 + 0.5 && o.x1 >= r.x1 - 0.5 && o.y0 <= r.y0 + 0.5 && o.y1 >= r.y1 - 0.5
    });
    let (mut right_limit, mut left_limit) = (content_right, content_left);
    for o in enclosing {
        right_limit = right_limit.min(o.x1 - 2.0);
        left_limit = left_limit.max(o.x0 + 2.0);
    }
    for o in &beside {
        if o.x0 >= r.x1 - 0.5 {
            right_limit = right_limit.min(o.x0 - gap);
        } else if o.x1 <= r.x0 + 0.5 {
            left_limit = left_limit.max(o.x1 + gap);
        }
    }
    let (right_limit, left_limit) = (right_limit.max(r.x1), left_limit.min(r.x0));
    match block.align {
        // Justified paragraphs keep their measure; other text may grow into the free space beside it, up to the
        // next text or drawing (in a table of contents, the page numbers).
        Align::Justify => r.width(),
        Align::Left => right_limit - r.x0,
        Align::Right => r.x1 - left_limit,
        Align::Center => r.width() + 2.0 * (r.x0 - left_limit).min(right_limit - r.x1),
    }
}

/// Free vertical space below `block` before the next text, drawing or image that overlaps it horizontally.
pub fn next_block_gap(layout: &PageLayout, obstacles: &[Rect], block: &TextBlock) -> f32 {
    let r = &block.rect;
    let below = |o: &Rect| o.y0 >= r.y1 - 0.5 && o.x0 < r.x1 && o.x1 > r.x0;
    // A drawing that already encloses the block (a box around a label) limits it at its own bottom edge.
    let enclosing = |o: &Rect| {
        o.x0 <= r.x0 + 0.5 && o.x1 >= r.x1 - 0.5 && o.y0 <= r.y0 + 0.5 && o.y1 >= r.y1 - 0.5
    };
    let texts = layout
        .blocks
        .iter()
        .filter(|o| o.id != block.id)
        .map(|o| o.rect)
        .filter(below)
        .map(|o| o.y0);
    let drawings = obstacles.iter().filter(|o| below(o)).map(|o| o.y0);
    let boxes = obstacles.iter().filter(|o| enclosing(o)).map(|o| o.y1);
    texts
        .chain(drawings)
        .chain(boxes)
        .map(|y| y - r.y1)
        .fold(layout.height - r.y1, f32::min)
        .max(0.0)
}
