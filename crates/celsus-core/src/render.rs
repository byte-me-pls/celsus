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
    face: usize,
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
        rtl: bool,
    ) -> celsus_pdf::Result<PageReport> {
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
            let Some(best) = self.fonts.best_for(&block.style) else {
                report.kept.push(Issue::MissingGlyphs {
                    id: id.clone(),
                    chars: text.into(),
                });
                continue;
            };
            let faces = self.fonts.faces();
            let Some(face) = std::iter::once(best)
                .chain(0..faces.len())
                .find(|&f| faces[f].missing_chars(text).is_empty())
            else {
                let chars = faces[best].missing_chars(text).into_iter().collect();
                report.kept.push(Issue::MissingGlyphs {
                    id: id.clone(),
                    chars,
                });
                continue;
            };
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
            let measure: &dyn Measure = &faces[face];
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
                    face,
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
            self.font_ref(plan.face);
            used_faces.insert(plan.face);
            let face = &self.fonts.faces()[plan.face];
            let embedder = &mut self.embedders.get_mut(&plan.face).unwrap().0;
            let b = plan.block;
            let size = plan.fitted.size;
            let align = match b.align {
                Align::Left | Align::Justify if rtl => Align::Right,
                a => a,
            };
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
                "{} {} {} rg 0 Tc 0 Tw 100 Tz 0 Ts 0 Tr BT /CelsusF{} {} Tf",
                rgb[0],
                rgb[1],
                rgb[2],
                plan.face,
                real(f64::from(size))
            )
            .unwrap();
            let last = plan.fitted.lines.len() - 1;
            for (n, line) in plan.fitted.lines.iter().enumerate() {
                let w = face.width(line, size);
                let indent = indent_of(b).of_line(n);
                let x = match align {
                    Align::Left | Align::Justify => b.rect.x0 + indent,
                    Align::Center => (b.rect.x0 + b.rect.x1 - w) / 2.0,
                    Align::Right => b.rect.x1 - w,
                };
                let baseline = first + plan.fitted.leading * n as f32;
                let (px, py) = (cx0 + f64::from(x), cy1 - f64::from(baseline));
                write!(draw, "1 0 0 1 {} {} Tm ", real(px), real(py)).unwrap();
                let spaces = line.matches(' ').count();
                let stretch = width - indent - w;
                if align == Align::Justify
                    && n < last
                    && spaces > 0
                    && stretch > 0.0
                    && stretch < width * 0.25
                {
                    // Justify with TJ adjustments after each space (Tw does not apply to two-byte codes).
                    let per_space = -(stretch / spaces as f32) / size * 1000.0;
                    draw.push('[');
                    for (i, word) in line.split(' ').enumerate() {
                        if i > 0 {
                            write!(
                                draw,
                                "<{}> {} ",
                                hex(&embedder.encode(" ")),
                                real(f64::from(per_space))
                            )
                            .unwrap();
                        }
                        write!(draw, "<{}>", hex(&embedder.encode(word))).unwrap();
                    }
                    draw.push_str("] TJ\n");
                } else {
                    writeln!(draw, "<{}> Tj", hex(&embedder.encode(line))).unwrap();
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
        // Multi-line paragraphs keep their measure; single lines may grow into free space.
        Align::Justify => r.width(),
        Align::Left if block.lines.len() > 1 => r.width(),
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
