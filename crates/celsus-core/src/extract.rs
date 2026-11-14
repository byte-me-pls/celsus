//! Turning the glyphs of a page into translatable blocks.
//!
//! Glyphs are joined into lines and lines into blocks using geometry only. Ruled lines, boxes and images act as
//! walls: text on two sides of a table rule, or inside two different diagram boxes, never ends up in one block.

use std::collections::{BTreeMap, HashMap, HashSet};

use celsus_pdf::interp::{self, PageContent};
use celsus_pdf::{Document, Page};

use crate::layout::{
    Align, BlockId, Initial, PageLayout, Rect, Span, TextBlock, TextLine, TextStyle, join_lines,
};

/// A glyph of the page content: run index and glyph index inside that run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GlyphRef {
    pub run: usize,
    pub glyph: usize,
}

/// A page's blocks plus everything needed to rewrite their text later.
pub struct ExtractedPage {
    pub layout: PageLayout,
    /// The glyphs each block was built from.
    pub glyphs: HashMap<BlockId, Vec<GlyphRef>>,
    /// Drawings and images (top-left coordinates) that new text must not run into.
    pub obstacles: Vec<Rect>,
    /// Blocks read from the text layer of a scanned page. The words to be seen are in the picture under them:
    /// to put a translation there, the picture's words are covered first.
    pub scans: HashSet<BlockId>,
    pub content: PageContent,
}

#[derive(Debug, Clone)]
struct G {
    text: String,
    rect: Rect,
    baseline: f32,
    size: f32,
    at: GlyphRef,
    space: bool,
}

#[derive(Debug, Clone, Copy)]
enum Wall {
    /// Horizontal rule at `y` from `x0` to `x1`.
    H { y: f32, x0: f32, x1: f32 },
    /// Vertical rule at `x` from `y0` to `y1`.
    V { x: f32, y0: f32, y1: f32 },
}

#[derive(Debug, Clone)]
struct Line {
    glyphs: Vec<usize>,
    rect: Rect,
    baseline: f32,
    size: f32,
    container: Option<usize>,
    first_run: usize,
    /// The glyphs of a large initial this line stands beside.
    initial: Vec<usize>,
}

/// A part of the page found by looking at it as a picture: one paragraph, one heading, one list entry.
#[derive(Debug, Clone)]
pub struct Region {
    pub label: String,
    /// How sure the guess is, from 0 to 1.
    pub score: f32,
    pub rect: Rect,
}

/// A guess below this is not followed.
const SURE: f32 = 0.5;

impl Region {
    /// Regions that hold one run of text. A table or a picture holds other things and is not one.
    fn is_text(&self) -> bool {
        matches!(
            self.label.as_str(),
            "text"
                | "list_item"
                | "caption"
                | "footnote"
                | "section_header"
                | "title"
                | "page_header"
                | "page_footer"
                | "code"
                | "formula"
        )
    }
}

/// Extract the blocks of `page` (number is 1-based, for the layout record).
pub fn extract_page(doc: &Document, page: &Page, number: u32) -> celsus_pdf::Result<ExtractedPage> {
    extract_page_in(doc, page, number, &[])
}

/// Extract the blocks of `page`. Where `regions` cover the text, and are sure of it, they decide which lines
/// belong together; everywhere else the lines are grouped by their geometry.
pub fn extract_page_in(
    doc: &Document,
    page: &Page,
    number: u32,
    regions: &[Region],
) -> celsus_pdf::Result<ExtractedPage> {
    let area = |a: &Rect| (a.x1 - a.x0).max(0.0) * (a.y1 - a.y0).max(0.0);
    let shared = |a: &Rect, b: &Rect| {
        area(&Rect {
            x0: a.x0.max(b.x0),
            y0: a.y0.max(b.y0),
            x1: a.x1.min(b.x1),
            y1: a.y1.min(b.y1),
        })
    };
    let guesses: Vec<&Region> = regions.iter().filter(|r| r.is_text()).collect();
    // Of two sure regions lying over each other the first listed stays (they come best first).
    let mut text_regions: Vec<&Region> = Vec::new();
    for r in &guesses {
        if r.score < SURE {
            continue;
        }
        // One box around an area and, with less weight, boxes standing side by side in it: the page was read
        // two ways there, as a paragraph and as the cells of a table. The box would join each row of cells
        // into one line, so it is not followed.
        let inside: Vec<&&Region> = guesses
            .iter()
            .filter(|q| {
                area(&q.rect) < area(&r.rect) * 0.6
                    && shared(&q.rect, &r.rect) > area(&q.rect) * 0.8
            })
            .collect();
        let cells = inside.iter().enumerate().any(|(i, a)| {
            inside[..i].iter().any(|b| {
                let both = a.rect.y1.min(b.rect.y1) - a.rect.y0.max(b.rect.y0);
                let low = (a.rect.y1 - a.rect.y0).min(b.rect.y1 - b.rect.y0);
                both > low * 0.5 && (a.rect.x1 <= b.rect.x0 || b.rect.x1 <= a.rect.x0)
            })
        });
        if cells {
            continue;
        }
        let covered = text_regions
            .iter()
            .any(|k| shared(&r.rect, &k.rect) > area(&r.rect).min(area(&k.rect)) * 0.6);
        if !covered {
            text_regions.push(r);
        }
    }
    let region_of = |r: &Rect| -> Option<usize> {
        let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        text_regions
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                cx >= t.rect.x0 && cx <= t.rect.x1 && cy >= t.rect.y0 && cy <= t.rect.y1
            })
            .min_by(|a, b| {
                let area = |t: &Region| (t.rect.x1 - t.rect.x0) * (t.rect.y1 - t.rect.y0);
                area(a.1).total_cmp(&area(b.1))
            })
            .map(|(i, _)| i)
    };
    let content = interp::interpret(doc, page)?;
    let [cx0, _, _, cy1] = page.crop_box.map(|v| v as f32);
    let (width, height) = (page.width() as f32, page.height() as f32);
    let to_rect = |b: [f32; 4]| Rect {
        x0: b[0] - cx0,
        y0: cy1 - b[3],
        x1: b[2] - cx0,
        y1: cy1 - b[1],
    };

    // Visible, horizontal glyphs. Rotated text is kept as its own protected blocks.
    //
    // A scanned page is a picture of the page with its text laid on it, for searching: some of it invisible,
    // some drawn over the picture's own words. Both kinds are the text of that page. They are read like any
    // other text, and remembered as lying on a scan.
    let on_scan = |b: &[f32; 4]| {
        let (x, y) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        content
            .bitonal
            .iter()
            .any(|s| x >= s[0] && x <= s[2] && y >= s[1] && y <= s[3])
    };
    let mut glyphs = Vec::new();
    let mut scan_glyphs: HashSet<GlyphRef> = HashSet::new();
    let mut rotated_runs = Vec::new();
    for (ri, run) in content.runs.iter().enumerate() {
        if run.render_mode == 7 || run.size < 1.0 {
            continue;
        }
        let hidden = run.render_mode == 3;
        if run.angle.abs() > 0.02 {
            if !hidden {
                rotated_runs.push(ri);
            }
            continue;
        }
        for (gi, g) in run.glyphs.iter().enumerate() {
            if g.text.is_empty() || !g.visible {
                continue;
            }
            let scan = on_scan(&g.bounds);
            // Invisible text anywhere else is not part of what the page shows.
            if hidden && !scan {
                continue;
            }
            if scan {
                scan_glyphs.insert(GlyphRef { run: ri, glyph: gi });
            }
            glyphs.push(G {
                space: g.text.chars().all(char::is_whitespace),
                text: g.text.clone(),
                rect: to_rect(g.bounds),
                baseline: cy1 - g.origin.1,
                size: run.size,
                at: GlyphRef { run: ri, glyph: gi },
            });
        }
    }

    // Walls and containers from vector graphics and images.
    let page_area = width * height;
    let mut walls = Vec::new();
    let mut containers: Vec<Rect> = Vec::new();
    let seen = uncovered(&content.paths);
    let drawn = || {
        content
            .paths
            .iter()
            .zip(&seen)
            .filter_map(|(p, b)| b.map(|b| (p, b)))
    };
    for (p, bounds) in drawn().filter(|(p, _)| p.is_rect) {
        let r = to_rect(bounds);
        let (w, h) = (r.width(), r.height());
        if w <= 2.5 && h > 6.0 {
            walls.push(Wall::V {
                x: (r.x0 + r.x1) / 2.0,
                y0: r.y0,
                y1: r.y1,
            });
        } else if h <= 2.5 && w > 6.0 {
            walls.push(Wall::H {
                y: (r.y0 + r.y1) / 2.0,
                x0: r.x0,
                x1: r.x1,
            });
        } else if w > 6.0 && h > 6.0 && w * h < page_area * 0.7 {
            // A white fill is usually a page-sized or column background, not a box around text.
            let background = p.filled && !p.stroked && p.fill == [255, 255, 255];
            if !background {
                containers.push(r);
            }
            if p.stroked {
                walls.push(Wall::H {
                    y: r.y0,
                    x0: r.x0,
                    x1: r.x1,
                });
                walls.push(Wall::H {
                    y: r.y1,
                    x0: r.x0,
                    x1: r.x1,
                });
                walls.push(Wall::V {
                    x: r.x0,
                    y0: r.y0,
                    y1: r.y1,
                });
                walls.push(Wall::V {
                    x: r.x1,
                    y0: r.y0,
                    y1: r.y1,
                });
            }
        }
    }
    let pictures = containers.len();
    // A scanned picture is the page itself, not something on it that text stands in or runs around.
    let pictures_on_the_page = || {
        content
            .images
            .iter()
            .copied()
            .filter(|b| !content.bitonal.contains(b))
    };
    for b in pictures_on_the_page() {
        let r = to_rect(b);
        if r.width() * r.height() < page_area * 0.7 {
            containers.push(r);
        }
    }
    let mut obstacles: Vec<Rect> = Vec::new();
    for b in drawn()
        .filter(|(p, _)| p.stroked || p.fill != [255, 255, 255])
        .map(|(_, b)| b)
        .chain(pictures_on_the_page())
    {
        let r = to_rect(b);
        if r.width() * r.height() < page_area * 0.7
            && r.x1 > 0.0
            && r.y1 > 0.0
            && r.x0 < width
            && r.y0 < height
        {
            obstacles.push(r);
        }
    }
    // The smallest container that holds a rectangle.
    let container_of = |r: &Rect| -> Option<usize> {
        let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        // The rectangle of a picture is often larger than what it shows (a round picture, a cut-out figure),
        // and text set around it reaches into the corners. Only text well inside counts as part of the picture.
        let inner = r.height();
        containers
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                let m = if *i >= pictures { inner } else { 0.0 };
                cx >= c.x0 + m && cx <= c.x1 - m && cy >= c.y0 + m && cy <= c.y1 - m
            })
            .min_by(|a, b| (a.1.width() * a.1.height()).total_cmp(&(b.1.width() * b.1.height())))
            .map(|(i, _)| i)
    };
    let v_wall_between = |x0: f32, x1: f32, y0: f32, y1: f32| {
        walls.iter().any(|w| match *w {
            Wall::V {
                x,
                y0: wy0,
                y1: wy1,
            } => x > x0 && x < x1 && wy0 < y1 - 1.0 && wy1 > y0 + 1.0,
            Wall::H { .. } => false,
        })
    };
    let h_wall_between = |y0: f32, y1: f32, x0: f32, x1: f32| {
        walls.iter().any(|w| match *w {
            Wall::H {
                y,
                x0: wx0,
                x1: wx1,
            } => y > y0 && y < y1 && wx0 < x1 - 1.0 && wx1 > x0 + 1.0,
            Wall::V { .. } => false,
        })
    };

    // Rows: glyphs sharing a baseline.
    let mut order: Vec<usize> = (0..glyphs.len()).collect();
    order.sort_by(|&a, &b| glyphs[a].baseline.total_cmp(&glyphs[b].baseline));
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for i in order {
        let g = &glyphs[i];
        match rows.last_mut() {
            Some(row)
                if (glyphs[row[0]].baseline - g.baseline).abs()
                    < glyphs[row[0]].size.min(g.size) * 0.3 =>
            {
                row.push(i)
            }
            _ => rows.push(vec![i]),
        }
    }

    // A row is measured from its first glyph, and that can be a line of the next column sitting a little
    // higher. Small capitals then fall just outside the row of the capital they follow. A glyph set right
    // behind the one drawn before it, on nearly the same baseline, belongs to that glyph's row.
    let mut row_of = vec![0usize; glyphs.len()];
    for (ri, row) in rows.iter().enumerate() {
        for &i in row {
            row_of[i] = ri;
        }
    }
    for i in 1..glyphs.len() {
        let (p, g) = (&glyphs[i - 1], &glyphs[i]);
        let size = p.size.min(g.size);
        if row_of[i] != row_of[i - 1]
            && (p.baseline - g.baseline).abs() < size * 0.3
            && g.rect.x0 - p.rect.x1 > -size * 0.3
            && g.rect.x0 - p.rect.x1 < size * 0.6
        {
            row_of[i] = row_of[i - 1];
        }
    }
    let mut rows: Vec<Vec<usize>> = {
        let mut out = vec![Vec::new(); rows.len()];
        for (i, &ri) in row_of.iter().enumerate() {
            out[ri].push(i);
        }
        out.retain(|row| !row.is_empty());
        out
    };
    for row in rows.iter_mut() {
        row.sort_by(|&a, &b| glyphs[a].rect.x0.total_cmp(&glyphs[b].rect.x0));
    }
    let mut rows: Vec<Vec<usize>> = rows
        .into_iter()
        .flat_map(|row| unmix(&glyphs, &content, row))
        .collect();
    attach_scripts(&mut glyphs, &mut rows);
    let gutters = find_gutters(&glyphs, &rows);

    // Lines: split rows at wide gaps, column gutters, walls and container changes.
    let mut lines: Vec<Line> = Vec::new();
    for (ri, row) in rows.into_iter().enumerate() {
        let mut current: Vec<usize> = Vec::new();
        let mut last_ink: Option<usize> = None;
        let mut spaced = false;
        for i in row {
            let g = &glyphs[i];
            if g.space {
                spaced = true;
            } else if let Some(prev) = last_ink.map(|p| &glyphs[p]) {
                let gap = g.rect.x0 - prev.rect.x1;
                let size = prev.size.max(g.size);
                // The rectangle of a drawing is often larger than what it shows (a round picture), and a word
                // set beside it can reach into a corner. A word is never half inside something, and a rule
                // that runs through a word (a connector in a diagram) does not cut it in two.
                let new_word = std::mem::take(&mut spaced) || gap > size * 0.25;
                // The cells of a table or the labels of a drawing are set one by one, and seldom land on
                // exactly the same baseline: where the same type goes on a little higher or lower after a
                // gap, another text begins. (A line that slopes a little moves far less from word to word.)
                let (a, b) = (&content.runs[prev.at.run], &content.runs[g.at.run]);
                let step = (g.baseline - prev.baseline).abs();
                let stepped = gap > size * 0.6
                    && a.font_resource == b.font_resource
                    && (prev.size - g.size).abs() < 0.05
                    && step > 0.3
                    && step > (g.rect.x0 - prev.rect.x0) * 0.04;
                let by_shape = gap > size * 1.6
                    || stepped
                    || gutters.contains(&(ri, i))
                    || (new_word
                        && (v_wall_between(
                            prev.rect.x1 - 0.5,
                            g.rect.x0 + 0.5,
                            g.rect.y0,
                            g.rect.y1,
                        ) || container_of(&prev.rect) != container_of(&g.rect)));
                let split = match (region_of(&prev.rect), region_of(&g.rect)) {
                    (Some(a), Some(b)) if a == b => false,
                    (Some(_), Some(_)) => new_word || by_shape,
                    _ => by_shape,
                };
                if split {
                    lines.extend(make_line(
                        &glyphs,
                        std::mem::take(&mut current),
                        &container_of,
                    ));
                }
            }
            current.push(i);
            if !g.space {
                last_ink = Some(i);
            }
        }
        lines.extend(make_line(&glyphs, current, &container_of));
    }
    find_initials(&glyphs, &mut lines, &container_of);

    // Blocks: stack lines that continue each other.
    lines.sort_by(|a, b| {
        a.rect
            .y0
            .total_cmp(&b.rect.y0)
            .then(a.rect.x0.total_cmp(&b.rect.x0))
    });
    // A bullet or another mark on its own is not a line of the text above or below it.
    let is_mark = |l: &Line| {
        let mut ink = l.glyphs.iter().map(|&i| &glyphs[i]).filter(|g| !g.space);
        ink.clone().count() <= 2 && ink.all(|g| !g.text.chars().any(char::is_alphanumeric))
    };
    // Line spacing: how far each line is from the next line under it, and the spacing most lines of a size
    // have on this page. More than that between two lines is the space between two paragraphs or entries.
    let next_leading: Vec<Option<f32>> = lines
        .iter()
        .map(|a| {
            lines
                .iter()
                .filter(|b| {
                    b.baseline > a.baseline + a.size * 0.3
                        && b.baseline < a.baseline + a.size * 2.5
                        && b.rect.x0 < a.rect.x1
                        && b.rect.x1 > a.rect.x0
                        && (a.size - b.size).abs() <= a.size * 0.15
                })
                .map(|b| b.baseline - a.baseline)
                .min_by(f32::total_cmp)
        })
        .collect();
    // How far the lines that start where a line starts reach to the right.
    let reach: Vec<f32> = lines
        .iter()
        .map(|a| {
            lines
                .iter()
                .filter(|b| {
                    (a.rect.x0 - b.rect.x0).abs() <= a.size * 3.0
                        && (a.size - b.size).abs() <= a.size * 0.15
                        && a.container == b.container
                })
                .map(|b| b.rect.x1)
                .fold(a.rect.x1, f32::max)
        })
        .collect();
    let mut seen: HashMap<(i32, i32), usize> = HashMap::new();
    for (l, lead) in lines.iter().zip(&next_leading) {
        if let Some(lead) = lead {
            *seen
                .entry(((l.size * 2.0).round() as i32, (lead * 4.0).round() as i32))
                .or_default() += 1;
        }
    }
    let usual_leading = |size: f32| {
        let key = (size * 2.0).round() as i32;
        seen.iter()
            .filter(|((s, _), n)| *s == key && **n >= 3)
            .max_by_key(|((_, lead), n)| (**n, -*lead))
            .map(|((_, lead), _)| *lead as f32 / 4.0)
    };
    let leader: Vec<bool> = lines
        .iter()
        .map(|l| crate::layout::leader_span(&line_text(&glyphs, &l.glyphs)).is_some())
        .collect();
    // A page number at the end of a row closes the entry it stands beside: the row of a table of contents, of
    // an index, of a list of figures. Such numbers stand in a column, one under the other.
    let is_number: Vec<bool> = lines
        .iter()
        .map(|l| {
            let text = line_text(&glyphs, &l.glyphs);
            let text = text.trim();
            (1..=6).contains(&text.chars().count())
                && (text.chars().all(|c| c.is_ascii_digit())
                    || text.chars().all(|c| "ivxlcdm".contains(c))
                    || text.chars().all(|c| "IVXLCDM".contains(c)))
        })
        .collect();
    let in_column = |i: usize| {
        let a = &lines[i];
        lines
            .iter()
            .zip(&is_number)
            .filter(|(b, is)| {
                **is && ((a.rect.x1 - b.rect.x1).abs() < 1.5 || (a.rect.x0 - b.rect.x0).abs() < 1.5)
            })
            .count()
            >= 3
    };
    let numbered: Vec<bool> = lines
        .iter()
        .map(|l| {
            (0..lines.len()).any(|i| {
                let n = &lines[i];
                is_number[i]
                    && n.container == l.container
                    && (n.baseline - l.baseline).abs() < l.size.min(n.size) * 0.3
                    && n.rect.x0 > l.rect.x1 + l.size
                    && in_column(i)
            })
        })
        .collect();
    let mut line_region: Vec<Option<usize>> = lines.iter().map(|l| region_of(&l.rect)).collect();
    // A region is one paragraph, set in one size. A line of another size whose letters lie on the letters of
    // its lines is a label that was printed over the text and only looks like part of it. (A mark set small
    // beside a word, or under a sum, touches no other letter.)
    let mut widths: HashMap<(usize, i32), f32> = HashMap::new();
    for (l, r) in lines.iter().zip(&line_region) {
        if let Some(r) = r {
            *widths
                .entry((*r, (l.size * 4.0).round() as i32))
                .or_default() += l.rect.x1 - l.rect.x0;
        }
    }
    let usual_size = |region: usize| {
        widths
            .iter()
            .filter(|((r, _), _)| *r == region)
            .max_by(|a, b| a.1.total_cmp(b.1).then(a.0.1.cmp(&b.0.1)))
            .map(|((_, size), _)| *size as f32 / 4.0)
    };
    let intruders: Vec<usize> = (0..lines.len())
        .filter(|&i| {
            let (Some(region), l) = (line_region[i], &lines[i]) else {
                return false;
            };
            let Some(usual) = usual_size(region) else {
                return false;
            };
            if (l.size - usual).abs() <= usual * 0.15 {
                return false;
            }
            let under: Vec<&G> = lines
                .iter()
                .zip(&line_region)
                .filter(|(o, r)| {
                    **r == Some(region)
                        && (o.size - usual).abs() <= usual * 0.15
                        && o.rect.y0 < l.rect.y1
                        && o.rect.y1 > l.rect.y0
                })
                .flat_map(|(o, _)| o.glyphs.iter().map(|&g| &glyphs[g]))
                .filter(|g| !g.space)
                .collect();
            let ink: Vec<&G> = l
                .glyphs
                .iter()
                .map(|&g| &glyphs[g])
                .filter(|g| !g.space)
                .collect();
            let on_top = ink
                .iter()
                .filter(|g| {
                    under
                        .iter()
                        .any(|u| shared(&g.rect, &u.rect) > area(&g.rect).min(area(&u.rect)) * 0.1)
                })
                .count();
            on_top >= 2 && on_top * 2 >= ink.len()
        })
        .collect();
    for i in intruders {
        line_region[i] = None;
    }
    // Letters or codes set out one by one ("H e l l o", "48 65 6C"): a figure in type, not a sentence. A
    // heading can be spaced out like that too, but it stands alone; these come in rows, one under the other
    // or side by side.
    let spelled: Vec<Spelled> = lines
        .iter()
        .map(|l| spelled_out(&line_text(&glyphs, &l.glyphs)))
        .collect();
    let spelled: Vec<bool> = (0..lines.len())
        .map(|i| match spelled[i] {
            Spelled::No => false,
            Spelled::Codes => true,
            Spelled::Letters => (0..lines.len()).any(|j| {
                j != i
                    && spelled[j] != Spelled::No
                    && (lines[j].baseline - lines[i].baseline).abs() < lines[i].size * 2.0
            }),
        })
        .collect();
    // An index: most lines are a few words, a comma and page numbers. Whatever continues an entry there is
    // set further in than the entry began; a line that is not begins the next one.
    let index = {
        let entries = lines
            .iter()
            .filter(|l| is_index_entry(&line_text(&glyphs, &l.glyphs)))
            .count();
        entries >= 8 && entries * 2 >= lines.len()
    };
    // Where a sentence ends with its line and the next line begins a new one.
    let sentences: Vec<(bool, bool)> = lines
        .iter()
        .map(|l| {
            let text = line_text(&glyphs, &l.glyphs);
            let text = text
                .trim()
                .trim_matches(['"', '\'', '“', '”', '‘', '’', '(', ')']);
            (
                text.chars().next().is_some_and(char::is_uppercase),
                text.ends_with(['.', '!', '?']),
            )
        })
        .collect();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let target = groups.iter().rposition(|g| {
            let prev = &lines[*g.last().unwrap()];
            if spelled[li] != spelled[*g.last().unwrap()] {
                return false;
            }
            let first = &lines[g[0]];
            if index && line.rect.x0 < first.rect.x0 + line.size * 0.3 {
                return false;
            }
            // An entry whose lines hang under its first one ends where a line comes back out to the left:
            // the items of a list, the sources at the end of a chapter.
            if g.len() >= 2
                && g[1..]
                    .iter()
                    .all(|&i| lines[i].rect.x0 > first.rect.x0 + line.size * 0.7)
                && line.rect.x0 < prev.rect.x0 - line.size * 0.7
                && (line.rect.x0 - first.rect.x0).abs() < line.size * 0.3
            {
                return false;
            }
            if numbered[*g.last().unwrap()] && line.baseline > prev.baseline + prev.size * 0.3 {
                return false;
            }
            match (line_region[li], line_region[g[0]]) {
                (Some(a), Some(b)) => return a == b,
                (None, None) => {}
                _ => return false,
            }
            if is_mark(prev) || is_mark(line) {
                return false;
            }
            // A row of dots leads to a page number and ends its entry. An entry is not continued by a line
            // that starts far to the left of it either: that is the number of the entry above.
            let last = *g.last().unwrap();
            if leader[last] || (leader[li] && prev.rect.x0 > line.rect.x0 + prev.size * 2.0) {
                return false;
            }
            let block_x0 = g.iter().map(|&i| lines[i].rect.x0).fold(f32::MAX, f32::min);
            let block_x1 = g.iter().map(|&i| lines[i].rect.x1).fold(f32::MIN, f32::max);
            let size = prev.size.max(line.size);
            let gap = line.rect.y0 - prev.rect.y1;
            let leading = line.baseline - prev.baseline;
            let overlap = line.rect.x0 < prev.rect.x1 && line.rect.x1 > prev.rect.x0;
            let similar = (prev.size - line.size).abs() <= size * 0.15;
            if !(overlap
                && similar
                && gap > -size * 0.4
                && gap < size * 0.9
                && leading < size * 2.0)
            {
                return false;
            }
            if prev.container != line.container
                || h_wall_between(
                    prev.baseline,
                    line.rect.y0 + line.size * 0.2,
                    line.rect.x0,
                    line.rect.x1,
                )
            {
                return false;
            }
            // Paragraph ends: a short line before (unless both are centred lines of one heading),
            // or a first-line indent after a full line.
            let centred =
                ((prev.rect.x0 + prev.rect.x1) - (line.rect.x0 + line.rect.x1)).abs() / 2.0 < 2.0
                    && (prev.rect.x0 - block_x0).abs() > 1.0;
            let right = block_x1.max(line.rect.x1);
            // A line is short when the next line's first word would still have fit on it. Ragged text ends
            // many lines well before the edge, in the middle of a sentence, because the next word is long.
            let first_word = {
                let mut ink = line
                    .glyphs
                    .iter()
                    .map(|&i| &glyphs[i])
                    .skip_while(|g| g.space);
                ink.next().map_or(0.0, |first| {
                    // Not every page draws its spaces: a gap between two letters ends the word as well.
                    let mut end = first;
                    for g in ink {
                        if g.space || g.rect.x0 - end.rect.x1 > end.size.min(g.size) * 0.18 {
                            break;
                        }
                        end = g;
                    }
                    end.rect.x1 - first.rect.x0
                })
            };
            let room = right - prev.rect.x1;
            let usual = usual_leading(prev.size);
            let wider = |by: f32| usual.is_some_and(|u| leading > u * by);
            // A short line whose room the next word would not have filled either: the end of an entry if the
            // next line stands further off than lines usually do.
            // Or a sentence that ends well before the lines around it do, with a new one under it: sentences
            // listed one per line.
            let beyond = reach[*g.last().unwrap()] - prev.rect.x1;
            let short_prev = right - block_x0 > size * 8.0
                && ((room > size * 1.5 && room > first_word + size * 0.3)
                    || (beyond > size * 1.5 && wider(1.08))
                    || (beyond > size * 1.5
                        && beyond > first_word + size * 0.3
                        && sentences[*g.last().unwrap()].1
                        && sentences[li].0));
            // More space than between the lines so far.
            let spaced = match g.len() {
                1 => wider(1.3) && next_leading[li].is_none_or(|next| next < leading * 0.9),
                n => leading > (prev.baseline - lines[g[n - 2]].baseline) * 1.3,
            };
            // A first-line indent only counts when the block so far sits flush left (not a hanging indent).
            // Its own first line may be indented the same way.
            let flush = g[1..]
                .iter()
                .all(|&i| (lines[i].rect.x0 - block_x0).abs() < 1.0);
            let indented = g.len() >= 2 && flush && line.rect.x0 > prev.rect.x0 + size * 0.7;
            !((short_prev && !centred) || indented || spaced)
        });
        // A text that many others were begun after is left alone. Only those that share its width count:
        // the cells of a table in the next column are read in between, and are not in the way.
        let near = |gi: usize| {
            let across = |g: &&Vec<usize>| {
                g.iter()
                    .any(|&i| lines[i].rect.x0 < line.rect.x1 && lines[i].rect.x1 > line.rect.x0)
            };
            groups[gi + 1..].iter().filter(across).count() < 8
        };
        match target {
            Some(gi) if line_region[li].is_some() || near(gi) => groups[gi].push(li),
            _ => groups.push(vec![li]),
        }
    }
    // A table sets short cells in rows and columns: each has a neighbour on its line. A column of such cells
    // is not one text. Every cell is read, translated and set by itself, and a number among them stays as it
    // is. The heading over the column is set apart from them, in bold or without a neighbour, and stays whole.
    let cell: Vec<bool> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            line_text(&glyphs, &l.glyphs).split_whitespace().count() <= 3
                && lines.iter().enumerate().any(|(j, o)| {
                    j != i
                        && o.container == l.container
                        && (o.baseline - l.baseline).abs() < l.size.min(o.size) * 0.3
                        && (o.rect.x1 <= l.rect.x0 || o.rect.x0 >= l.rect.x1)
                })
        })
        .collect();
    // A longer cell has such a short cell beside it on its line.
    let in_row: Vec<bool> = lines
        .iter()
        .map(|l| {
            lines.iter().enumerate().any(|(j, o)| {
                cell[j]
                    && o.container == l.container
                    && (o.baseline - l.baseline).abs() < l.size.min(o.size) * 0.3
                    && (o.rect.x1 <= l.rect.x0 || o.rect.x0 >= l.rect.x1)
            })
        })
        .collect();
    let bold: Vec<bool> = lines
        .iter()
        .map(|l| {
            l.glyphs
                .iter()
                .map(|&g| &glyphs[g])
                .filter(|g| !g.space)
                .all(|g| content.runs[g.at.run].font.bold)
        })
        .collect();
    let mut groups: Vec<Vec<usize>> = groups
        .into_iter()
        .flat_map(|g| {
            if line_region[g[0]].is_some() {
                return vec![g];
            }
            let mut parts: Vec<Vec<usize>> = Vec::new();
            let mut rest: Vec<usize> = Vec::new();
            let mut at = 0;
            while at < g.len() {
                let mut end = at;
                while end < g.len() && cell[g[end]] && bold[g[end]] == bold[g[at]] {
                    end += 1;
                }
                // The heading of a column ends where the weight of the type changes, however few cells follow.
                let headed =
                    at > 0 && end > at && cell[g[at - 1]] && bold[g[at]] != bold[g[at - 1]];
                if end - at >= 3 || headed {
                    if !rest.is_empty() {
                        parts.push(std::mem::take(&mut rest));
                    }
                    parts.extend(g[at..end].iter().map(|&i| vec![i]));
                    at = end;
                } else {
                    rest.push(g[at]);
                    at += 1;
                }
            }
            if !rest.is_empty() {
                parts.push(rest);
            }
            parts
        })
        .collect();

    // Reading order: the order the content stream draws them, which producers almost always write in.
    groups.sort_by_key(|g| {
        g.iter()
            .map(|&i| lines[i].first_run)
            .min()
            .unwrap_or(usize::MAX)
    });

    let mut blocks = Vec::new();
    let mut refs = HashMap::new();
    let mut rows: Vec<bool> = Vec::new();
    for group in &groups {
        // Lines that each stand in a row of a table and each begin anew are its cells, one under the other,
        // and not a paragraph.
        rows.push(
            group.len() >= 2
                && group.iter().all(|&li| in_row[li])
                && group[1..].iter().all(|&li| {
                    line_text(&glyphs, &lines[li].glyphs)
                        .trim_start()
                        .starts_with(|c: char| c.is_uppercase() || c.is_numeric())
                }),
        );
        let id = BlockId(format!("b{:04}", blocks.len() + 1));
        let members: Vec<usize> = group
            .iter()
            .flat_map(|&li| lines[li].initial.iter().chain(&lines[li].glyphs).copied())
            .collect();
        let style = dominant_style(
            &content,
            &glyphs,
            &members,
            group.iter().map(|&li| lines[li].size),
        );
        let (mut inline, looks) = inline_styles(
            &content,
            &glyphs,
            group.iter().flat_map(|&li| &lines[li].glyphs).copied(),
            &style,
        );
        let mut text_lines: Vec<TextLine> = group
            .iter()
            .map(|&li| {
                let l = &lines[li];
                let mut text = line_text(&glyphs, &l.initial);
                let (rest, mut spans) = styled_text(&content, &glyphs, &l.glyphs, &style, &looks);
                for span in &mut spans {
                    span.at += text.len() as u32;
                }
                text.push_str(&rest);
                TextLine {
                    rect: l.rect,
                    text,
                    baseline: l.baseline,
                    spans,
                }
            })
            .collect();
        let rect = text_lines
            .iter()
            .skip(1)
            .fold(text_lines[0].rect, |r, l| r.union(&l.rect));
        let initial = (group.len() >= 2)
            .then(|| &lines[group[0]].initial)
            .and_then(|cap| {
                let ink: Vec<&G> = cap
                    .iter()
                    .map(|&i| &glyphs[i])
                    .filter(|g| !g.space)
                    .collect();
                let first = ink.first()?;
                let rect = ink.iter().fold(first.rect, |r, g| r.union(&g.rect));
                Some(Initial {
                    rect: Rect {
                        y0: (first.baseline - first.size * 0.75).max(rect.y0),
                        y1: first.baseline,
                        ..rect
                    },
                    size: first.size,
                })
            });
        let source = join_lines(text_lines.iter().map(|l| l.text.as_str()));
        let letters = source.chars().filter(|c| c.is_alphabetic()).count();
        let protected = letters < 2
            || style.monospace
            || looks_like_code(&source)
            || looks_like_formula(&source)
            || text_lines.iter().all(|l| is_roman_numeral(l.text.trim()))
            || group.iter().all(|&li| spelled[li]);
        // Text that stays as it is has no use for the styles inside it. Nor has text with such tags of its
        // own: nobody could tell the two apart afterwards.
        if protected
            || crate::inline::has_tags(&source)
            || text_lines.iter().all(|l| l.spans.is_empty())
        {
            inline.clear();
            for line in &mut text_lines {
                line.spans.clear();
            }
        }
        let align = detect_align(&text_lines, &rect, style.font_size);
        let mut at: Vec<GlyphRef> = members.iter().map(|&i| glyphs[i].at).collect();
        at.sort_unstable();
        refs.insert(id.clone(), at);
        blocks.push(TextBlock {
            id,
            rect,
            lines: text_lines,
            style,
            align,
            protected,
            separate_lines: false,
            initial,
            inline,
        });
    }
    // The first of two lines is as long as the block: that alone does not make the block justified. It is
    // when a longer justified text on the page ends at the same edge.
    let edges: Vec<f32> = blocks
        .iter()
        .filter(|b| b.lines.len() >= 3 && b.align == Align::Justify)
        .map(|b| b.rect.x1)
        .collect();
    // Nor do two lines show whether the second hangs under the first or is centred under it. The entries of
    // a list begin at one edge; a label under a drawing shares its edge with nothing.
    let lefts: Vec<f32> = blocks.iter().map(|b| b.rect.x0).collect();
    // Where each line of each block begins: an edge that other text begins at too is the edge of a column.
    let starts: Vec<(usize, f32)> = blocks
        .iter()
        .enumerate()
        .flat_map(|(i, b)| b.lines.iter().map(move |l| (i, l.rect.x0)))
        .collect();
    // And a paragraph of two lines whose first line is indented and stops short of the margin by about as
    // much looks centred, line for line. It is a paragraph when the longer paragraphs of the page begin the
    // same way: their first lines where its first line begins, their other lines where its second does.
    let indented: Vec<(f32, f32)> = blocks
        .iter()
        .filter(|b| b.lines.len() >= 3 && !b.protected)
        .map(|b| (b.lines[0].rect.x0, b.lines[1].rect.x0))
        .collect();
    for (i, b) in blocks.iter_mut().enumerate() {
        if b.lines.len() != 2 || b.protected {
            continue;
        }
        if b.align == Align::Center {
            let (first, second) = (b.lines[0].rect.x0, b.lines[1].rect.x0);
            if first > second + b.style.font_size * 0.4
                && indented
                    .iter()
                    .any(|(f, rest)| (f - first).abs() < 1.0 && (rest - second).abs() < 1.0)
            {
                b.align = Align::Left;
            }
        }
        if b.align == Align::Justify && !edges.iter().any(|x1| (x1 - b.rect.x1).abs() < 1.5) {
            b.align = Align::Left;
        }
        let (first, second) = (&b.lines[0].rect, &b.lines[1].rect);
        let tol = (b.style.font_size * 0.25).max(1.0);
        // Two lines that end at one edge and begin far apart, where no other text begins, are set flush right,
        // as the title of a book is on its title page. Read from the left they look like a paragraph with a deep
        // hanging indent, and the translation would run from where the first line began, its other lines pushed
        // far in. A paragraph, a list entry or an index entry begins where others do; a list with a hanging
        // indent shows itself in its longer entries.
        let alone = |x: f32| !starts.iter().any(|&(j, x0)| j != i && (x0 - x).abs() < 1.0);
        if matches!(b.align, Align::Left | Align::Justify)
            && (first.x1 - second.x1).abs() < tol
            && second.x0 > first.x0 + b.style.font_size
            && alone(first.x0)
            && alone(second.x0)
            && !indented.iter().any(|(f, rest)| {
                (f - first.x0).abs() < 1.0 && (rest - second.x0).abs() < b.style.font_size
            })
        {
            b.align = Align::Right;
        }
        if b.align == Align::Left
            && ((first.x0 + first.x1) - (second.x0 + second.x1)).abs() / 2.0 < tol
            && second.x0 > first.x0 + b.style.font_size * 0.7
            && !lefts
                .iter()
                .enumerate()
                .any(|(j, x0)| j != i && (x0 - b.rect.x0).abs() < 1.0)
        {
            b.align = Align::Center;
        }
    }
    mark_separate_lines(&mut blocks);
    for (b, rows) in blocks.iter_mut().zip(rows) {
        b.separate_lines |= rows && !b.protected;
    }

    // Rotated runs: one protected block each, so they are never touched.
    for &ri in &rotated_runs {
        let run = &content.runs[ri];
        let visible: Vec<usize> = (0..run.glyphs.len())
            .filter(|&i| run.glyphs[i].visible)
            .collect();
        let text: String = visible
            .iter()
            .map(|&i| run.glyphs[i].text.as_str())
            .collect();
        if text.trim().is_empty() {
            continue;
        }
        let rect = visible
            .iter()
            .skip(1)
            .fold(to_rect(run.glyphs[visible[0]].bounds), |r, &i| {
                r.union(&to_rect(run.glyphs[i].bounds))
            });
        let id = BlockId(format!("b{:04}", blocks.len() + 1));
        refs.insert(
            id.clone(),
            visible
                .iter()
                .map(|&glyph| GlyphRef { run: ri, glyph })
                .collect(),
        );
        blocks.push(TextBlock {
            id,
            rect,
            lines: vec![TextLine {
                rect,
                text: text.trim().to_owned(),
                baseline: rect.y1,
                spans: Vec::new(),
            }],
            style: run_style(&content, ri, run.size),
            align: Align::Left,
            protected: true,
            separate_lines: false,
            initial: None,
            inline: Vec::new(),
        });
    }

    Ok(ExtractedPage {
        layout: PageLayout {
            page: number,
            width,
            height,
            ocr: false,
            blocks,
        },
        // A block is of the scan when most of its glyphs lie on it.
        scans: refs
            .iter()
            .filter(|(_, list)| {
                list.iter().filter(|g| scan_glyphs.contains(g)).count() * 2 > list.len()
            })
            .map(|(id, _)| id.clone())
            .collect(),
        glyphs: refs,
        obstacles,
        content,
    })
}

/// A formula set on a line of its own: a mathematical sign and nothing longer than a three-letter name.
fn looks_like_formula(text: &str) -> bool {
    text.contains(|c| "=≤≥≠≈±×÷∑∏∫√∞".contains(c))
        && text
            .split(|c: char| !c.is_alphabetic())
            .all(|word| word.chars().count() <= 3)
}

/// A page number of the front matter, or the number of a part: "xvii", "IV". There is nothing in it to
/// translate, and a column of them has to stay as it is.
fn is_roman_numeral(text: &str) -> bool {
    let lower = text.to_lowercase();
    if text.is_empty() || (text != lower && text != text.to_uppercase()) {
        return false;
    }
    let mut rest = lower.as_str();
    for group in [
        &["mmm", "mm", "m"][..],
        &["cm", "cd", "dccc", "dcc", "dc", "d", "ccc", "cc", "c"],
        &["xc", "xl", "lxxx", "lxx", "lx", "l", "xxx", "xx", "x"],
        &["ix", "iv", "viii", "vii", "vi", "v", "iii", "ii", "i"],
    ] {
        if let Some(part) = group.iter().find(|part| rest.starts_with(**part)) {
            rest = &rest[part.len()..];
        }
    }
    rest.is_empty()
}

#[derive(Clone, Copy, PartialEq)]
enum Spelled {
    No,
    Letters,
    Codes,
}

/// Single letters with spaces between them, or a row of two-digit codes: text that is shown letter by letter
/// or byte by byte.
fn spelled_out(line: &str) -> Spelled {
    let words: Vec<&str> = line.split_whitespace().collect();
    let digits = words
        .iter()
        .filter(|w| w.contains(|c: char| c.is_ascii_digit()))
        .count();
    if words.len() >= 3
        && digits * 2 >= words.len()
        && words
            .iter()
            .all(|w| w.len() == 2 && w.chars().all(|c| c.is_ascii_hexdigit()))
    {
        Spelled::Codes
    } else if words.len() >= 2 && words.iter().all(|w| w.chars().count() == 1) {
        Spelled::Letters
    } else {
        Spelled::No
    }
}

/// A single token such as `getPushesFromPinNumber(int)`, `user_id` or `java.util.List`.
fn looks_like_code(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return false;
    }
    let chars: Vec<char> = t.chars().collect();
    let inner_upper = chars
        .windows(2)
        .any(|w| w[0].is_lowercase() && w[1].is_uppercase());
    let call = t.contains('(') && t.ends_with(')');
    let snake = chars
        .windows(3)
        .any(|w| w[0].is_alphanumeric() && w[1] == '_' && w[2].is_alphanumeric());
    let dotted = chars
        .windows(3)
        .any(|w| w[0].is_alphabetic() && w[1] == '.' && w[2].is_alphabetic());
    call || snake || dotted || (inner_upper && !chars[0].is_uppercase())
}

/// What is left to see of each drawing. A filled rectangle drawn later hides what lies under it: the art of a
/// cover often runs on under the panel that holds the text, and is no picture beside that text.
fn uncovered(paths: &[interp::PathItem]) -> Vec<Option<[f32; 4]>> {
    let covers: Vec<(usize, [f32; 4])> = paths
        .iter()
        .enumerate()
        .filter(|(_, q)| {
            q.filled && q.bounds[2] - q.bounds[0] > 20.0 && q.bounds[3] - q.bounds[1] > 20.0
        })
        .map(|(i, q)| {
            if q.is_rect {
                return (i, q.bounds);
            }
            // A round shape (a speech bubble, a disc, a box with round corners) covers for sure what lies in
            // the box inside its own: for an ellipse that box reaches seven tenths of the way out.
            let [x0, y0, x1, y1] = q.bounds;
            let (dx, dy) = ((x1 - x0) * 0.15, (y1 - y0) * 0.15);
            (i, [x0 + dx, y0 + dy, x1 - dx, y1 - dy])
        })
        .collect();
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut b = p.bounds;
            // The line around a box is as wide outside the box as inside: a fill of the same size leaves it.
            let m = if p.stroked { 1.0 } else { -0.5 };
            for (_, c) in covers.iter().filter(|(j, _)| *j > i) {
                let across = c[0] <= b[0] - m && c[2] >= b[2] + m;
                let along = c[1] <= b[1] - m && c[3] >= b[3] + m;
                if across && along {
                    return None;
                }
                if across && c[1] <= b[1] - m && c[3] > b[1] && c[3] < b[3] {
                    b[1] = c[3];
                } else if across && c[3] >= b[3] + m && c[1] < b[3] && c[1] > b[1] {
                    b[3] = c[1];
                } else if along && c[0] <= b[0] - m && c[2] > b[0] && c[2] < b[2] {
                    b[0] = c[2];
                } else if along && c[2] >= b[2] + m && c[0] < b[2] && c[0] > b[0] {
                    b[2] = c[0];
                }
            }
            Some(b)
        })
        .collect()
}

/// Gaps that separate columns rather than words: `(row, first glyph after the gap)`.
///
/// A gap counts as a gutter when the text before it ends on an edge shared with at least two other rows (the
/// right margin of a justified column) and another row has a gap starting or ending at the same place. Word
/// spaces in justified text are wide too, but they do not line up like that.
fn find_gutters(glyphs: &[G], rows: &[Vec<usize>]) -> std::collections::HashSet<(usize, usize)> {
    struct Gap {
        row: usize,
        x0: f32,
        x1: f32,
        after: usize,
        /// Wider than a word space can be; narrower gaps only count before a column of page numbers.
        wide: bool,
        /// No wider than the spaces of a justified line get.
        wordlike: bool,
    }
    let mut gaps = Vec::new();
    let mut edges = Vec::new();
    // Where rows begin.
    let mut starts = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let mut prev: Option<&G> = None;
        for &i in row {
            let g = &glyphs[i];
            if g.space {
                continue;
            }
            if prev.is_none() {
                starts.push(g.rect.x0);
            }
            if let Some(p) = prev {
                let gap = g.rect.x0 - p.rect.x1;
                let size = p.size.max(g.size);
                if gap > size * 0.3 {
                    let wide = gap > size * 0.6;
                    gaps.push(Gap {
                        row: ri,
                        x0: p.rect.x1,
                        x1: g.rect.x0,
                        after: i,
                        wide,
                        wordlike: gap < size * 1.2,
                    });
                    if wide {
                        edges.push(p.rect.x1);
                    }
                }
            }
            prev = Some(g);
        }
        if let Some(p) = prev {
            edges.push(p.rect.x1);
        }
    }
    // A column of page numbers (a table of contents, an index): a row ends in a run of digits or roman numerals,
    // set apart from what comes before, and its right edge lines up with such runs on at least two other rows. A
    // long entry can come close to its number, so the gap may be narrow (and is often a positioning move, not a
    // space character); it still separates two columns.
    let is_numeral = |t: &str| {
        !t.is_empty()
            && (t.chars().all(|c| c.is_ascii_digit()) || t.chars().all(|c| "ivxlcdm".contains(c)))
    };
    // (row, first glyph of the trailing number, its right edge)
    let mut trailing: Vec<(usize, usize, f32)> = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let ink: Vec<usize> = row.iter().copied().filter(|&i| !glyphs[i].space).collect();
        let Some(&last) = ink.last() else { continue };
        let mut k = ink.len();
        while k > 0 && is_numeral(glyphs[ink[k - 1]].text.trim()) {
            k -= 1;
        }
        if k == ink.len() || k == 0 {
            continue;
        }
        let text: String = ink[k..].iter().map(|&i| glyphs[i].text.as_str()).collect();
        let (before, first) = (&glyphs[ink[k - 1]], &glyphs[ink[k]]);
        if is_numeral(&text) && first.rect.x0 - before.rect.x1 > before.size.max(first.size) * 0.3 {
            trailing.push((ri, ink[k], glyphs[last].rect.x1));
        }
    }
    let mut out = std::collections::HashSet::new();
    for &(ri, first, right) in &trailing {
        if trailing
            .iter()
            .filter(|t| (t.2 - right).abs() < 1.5)
            .count()
            >= 3
        {
            out.insert((ri, first));
        }
    }
    for gap in &gaps {
        if !gap.wide {
            continue;
        }
        let shared_edge = edges.iter().filter(|e| (*e - gap.x0).abs() < 1.0).count() >= 3;
        // Another row breaks at the same place, or other rows begin where the text behind the gap begins (a
        // note in the margin, set on lines of its own).
        let partner = gaps.iter().any(|o| {
            o.wide
                && o.row != gap.row
                && ((o.x0 - gap.x0).abs() < 1.0 || (o.x1 - gap.x1).abs() < 1.0)
        }) || starts.iter().filter(|x| (*x - gap.x1).abs() < 0.5).count() >= 2;
        // Three lines of a paragraph can end a word at the same place by chance. Where the gap could be a
        // word space, the text behind it has to line up as well, as the left edge of a column does.
        let column = !gap.wordlike
            || gaps
                .iter()
                .filter(|o| o.wide && o.row != gap.row)
                .map(|o| o.x1)
                .chain(starts.iter().copied())
                .filter(|x| (x - gap.x1).abs() < 0.5)
                .count()
                >= 2;
        // A note in the margin is set ragged, so its lines share no right edge; the column beside it still
        // begins at one place, line after line. So does the second column of an index, where hardly a row
        // begins there: the rows reach it across a gap of their own.
        let size = glyphs[gap.after].size;
        let column_start = gap.x1 - gap.x0 > size * 0.8
            && starts
                .iter()
                .copied()
                .chain(
                    gaps.iter()
                        .filter(|o| o.row != gap.row && o.x1 - o.x0 > size * 0.8)
                        .map(|o| o.x1),
                )
                .filter(|x| (x - gap.x1).abs() < 0.5)
                .count()
                >= 3;
        if (shared_edge && partner && column) || column_start {
            out.insert((gap.row, gap.after));
        }
    }
    out
}

/// Two texts printed over each other on one baseline (a label under a note, a watermark behind a line) would be
/// read as one line with their letters mixed. They are told apart by how they were drawn (font, size, colour, or
/// a baseline that is a little off), and each becomes a row of its own.
fn unmix(glyphs: &[G], content: &PageContent, row: Vec<usize>) -> Vec<Vec<usize>> {
    let key = |i: usize| {
        let run = &content.runs[glyphs[i].at.run];
        (
            run.font_resource.as_slice(),
            (run.size * 10.0).round() as i32,
            run.color,
            (glyphs[i].baseline * 4.0).round() as i32,
        )
    };
    // Kinds of text whose letters lie on each other.
    let mut clash = Vec::new();
    let mut last: Option<usize> = None;
    for &i in row.iter().filter(|&&i| !glyphs[i].space) {
        if let Some(p) = last {
            let (a, b) = (&glyphs[p].rect, &glyphs[i].rect);
            let over = a.x1.min(b.x1) - b.x0;
            if over > a.width().min(b.width()) * 0.5 && key(p) != key(i) {
                clash.push((key(p), key(i)));
            }
        }
        last = Some(i);
    }
    // Two lines of the same kind that are a fraction of a point apart show it less plainly: an accent set
    // over its letter looks the same, once or twice in a row.
    let plain = clash
        .iter()
        .filter(|(a, b)| (a.0, a.1, a.2) != (b.0, b.1, b.2))
        .count();
    if plain < 2 && clash.len() < 4 {
        return vec![row];
    }
    // Each kind joins a strand it does not lie on. Where there are several (a word in italics lies on neither
    // text), it is the one drawn nearest to it: the words of a line follow each other in the page's content.
    let mut strands: Vec<(Vec<_>, Vec<usize>)> = Vec::new();
    for &i in &row {
        let k = key(i);
        let run = glyphs[i].at.run;
        let fits = |kinds: &Vec<_>| {
            !clash
                .iter()
                .any(|(a, b)| (*a == k && kinds.contains(b)) || (*b == k && kinds.contains(a)))
        };
        let apart = |members: &Vec<usize>| {
            members
                .iter()
                .map(|&m| glyphs[m].at.run.abs_diff(run))
                .min()
                .unwrap_or(usize::MAX)
        };
        let home = strands
            .iter()
            .position(|(kinds, _)| kinds.contains(&k))
            .or_else(|| {
                (0..strands.len())
                    .filter(|&s| fits(&strands[s].0))
                    .min_by_key(|&s| apart(&strands[s].1))
            });
        match home.map(|s| &mut strands[s]) {
            Some((kinds, members)) => {
                if !kinds.contains(&k) {
                    kinds.push(k);
                }
                members.push(i);
            }
            None => strands.push((vec![k], vec![i])),
        }
    }
    strands.into_iter().map(|(_, members)| members).collect()
}

fn make_line(
    glyphs: &[G],
    members: Vec<usize>,
    container_of: &dyn Fn(&Rect) -> Option<usize>,
) -> Option<Line> {
    let ink: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&i| !glyphs[i].space)
        .collect();
    if ink.is_empty() {
        return None;
    }
    let rect = ink
        .iter()
        .skip(1)
        .fold(glyphs[ink[0]].rect, |r, &i| r.union(&glyphs[i].rect));
    let mut sizes: Vec<f32> = ink.iter().map(|&i| glyphs[i].size).collect();
    sizes.sort_by(f32::total_cmp);
    // The largest regular size: small capitals and superscripts are smaller, drop caps far larger.
    let median = sizes[sizes.len() / 2];
    let size = sizes
        .iter()
        .copied()
        .filter(|s| *s <= median * 1.5)
        .fold(median, f32::max);
    // The baseline of the body text, not of superscripts.
    let baseline = ink
        .iter()
        .filter(|&&i| (glyphs[i].size - size).abs() < 0.5)
        .map(|&i| glyphs[i].baseline)
        .fold(f32::MIN, f32::max);
    let first_run = members.iter().map(|&i| glyphs[i].at.run).min().unwrap_or(0);
    Some(Line {
        container: container_of(&rect),
        glyphs: members,
        rect,
        baseline,
        size,
        first_run,
        initial: Vec::new(),
    })
}

/// Large initials: one to three big letters with the first lines of a paragraph standing beside them. The
/// letters are taken out of the lines and handed to the topmost line beside them, so the paragraph reads as
/// one text again and its lines are all of one size.
fn find_initials(
    glyphs: &[G],
    lines: &mut Vec<Line>,
    container_of: &dyn Fn(&Rect) -> Option<usize>,
) {
    struct Found {
        line: usize,
        cap: Vec<usize>,
        rest: Vec<usize>,
        host: usize,
    }
    let mut found: Vec<Found> = Vec::new();
    for (c, line) in lines.iter().enumerate() {
        let ink: Vec<usize> = line
            .glyphs
            .iter()
            .copied()
            .filter(|&i| !glyphs[i].space)
            .collect();
        let big = if ink.len() <= 3 {
            ink.len()
        } else {
            ink.iter()
                .take_while(|&&i| glyphs[i].size >= line.size * 1.8)
                .count()
        };
        if big == 0 || big > 3 {
            continue;
        }
        let first = &glyphs[ink[0]];
        let (cap_size, cap_base) = (first.size, first.baseline);
        if ink[..big]
            .iter()
            .any(|&i| (glyphs[i].size - cap_size).abs() > 0.5)
            || !ink[..big]
                .iter()
                .any(|&i| glyphs[i].text.chars().any(char::is_alphabetic))
        {
            continue;
        }
        let cap_x1 = ink[..big]
            .iter()
            .map(|&i| glyphs[i].rect.x1)
            .fold(f32::MIN, f32::max);
        let end = line.glyphs.iter().position(|&i| i == ink[big - 1]).unwrap() + 1;
        let (cap, rest) = (line.glyphs[..end].to_vec(), line.glyphs[end..].to_vec());
        let own = rest.iter().any(|&i| !glyphs[i].space);
        // The lines standing beside it, from the top.
        let mut beside: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|&(o, l)| {
                o != c
                    && l.size * 1.8 <= cap_size
                    && l.rect.x0 >= cap_x1 - l.size * 0.5
                    && l.rect.x0 <= cap_x1 + l.size * 2.0
                    && l.baseline > cap_base - cap_size * 0.85
                    && l.baseline <= cap_base + l.size * 0.4
            })
            .map(|(o, _)| o)
            .collect();
        if beside.len() + usize::from(own) < 2 {
            continue;
        }
        beside.sort_by(|&a, &b| lines[a].baseline.total_cmp(&lines[b].baseline));
        let host = match beside.first() {
            Some(&top) if !own || lines[top].baseline < cap_base - 0.5 => top,
            _ => c,
        };
        found.push(Found {
            line: c,
            cap,
            rest,
            host,
        });
    }
    let mut gone = vec![false; lines.len()];
    for f in found {
        match make_line(glyphs, f.rest, container_of) {
            Some(rest) => lines[f.line] = rest,
            None => gone[f.line] = true,
        }
        if !gone[f.host] {
            lines[f.host].initial = f.cap;
        }
    }
    let mut gone = gone.into_iter();
    lines.retain(|_| !gone.next().unwrap());
}

/// The superscript or subscript form of a character, where Unicode has one.
const PLAIN_RAISED: &str = "0123456789+-=()abcdefghijklmnoprstuvwxyzABDEGHIJKLMNOPRTUVW";
const RAISED: &str = "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ᵃᵇᶜᵈᵉᶠᵍʰⁱʲᵏˡᵐⁿᵒᵖʳˢᵗᵘᵛʷˣʸᶻᴬᴮᴰᴱᴳᴴᴵᴶᴷᴸᴹᴺᴼᴾᴿᵀᵁⱽᵂ";
const PLAIN_LOWERED: &str = "0123456789+-=()aehijklmnoprstuvx";
const LOWERED: &str = "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₕᵢⱼₖₗₘₙₒₚᵣₛₜᵤᵥₓ";

fn script_form(c: char, raised: bool) -> Option<char> {
    let c = if matches!(c, '−' | '–') { '-' } else { c };
    let (plain, set) = if raised {
        (PLAIN_RAISED, RAISED)
    } else {
        (PLAIN_LOWERED, LOWERED)
    };
    set.chars().nth(plain.chars().position(|p| p == c)?)
}

/// The ordinary character behind a superscript or subscript one, for a font that does not have the small form.
pub(crate) fn plain_form(c: char) -> Option<char> {
    [(PLAIN_RAISED, RAISED), (PLAIN_LOWERED, LOWERED)]
        .into_iter()
        .find_map(|(plain, set)| plain.chars().nth(set.chars().position(|p| p == c)?))
}

/// Superscripts and subscripts (exponents, footnote marks, the 2 of H₂O) stand on a baseline of their own, so
/// they come out as rows of their own. Move each into the line it is set against and write it with the
/// superscript or subscript character, so it stays with its word through translation.
fn attach_scripts(glyphs: &mut [G], rows: &mut Vec<Vec<usize>>) {
    // (from row, to row, glyph, its new text)
    let mut moves: Vec<(usize, usize, usize, String)> = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        // Candidates are short groups of characters standing on their own: a small letter inside a word of
        // small letters is just text.
        let mut groups: Vec<Vec<usize>> = Vec::new();
        // An expression such as N − 1 is one group: it moves as a whole or stays as a whole.
        for &i in row.iter().filter(|&&i| !glyphs[i].space) {
            let g = &glyphs[i];
            match groups.last_mut() {
                Some(group)
                    if g.rect.x0 - glyphs[*group.last().unwrap()].rect.x1 < g.size * 1.2 =>
                {
                    group.push(i)
                }
                _ => groups.push(vec![i]),
            }
        }
        for group in groups.iter().filter(|g| g.len() <= 6) {
            let (first, last) = (&glyphs[group[0]], &glyphs[*group.last().unwrap()]);
            // Rows are in baseline order, so the line is among the rows next to this one.
            let host = (ri.saturating_sub(4)..(ri + 5).min(rows.len()))
                .filter(|&hi| hi != ri)
                .find(|&hi| {
                    rows[hi].iter().map(|&i| &glyphs[i]).any(|h| {
                        let rise = h.baseline - first.baseline;
                        !h.space
                            && h.size >= first.size * 1.2
                            && h.size <= first.size * 2.2
                            && rise > -h.size * 0.4
                            && rise < h.size * 0.75
                            // Right behind a letter of the line, or right before one.
                            && ((first.rect.x0 - h.rect.x1).abs() < h.size * 0.3
                                || (h.rect.x0 - last.rect.x1).abs() < h.size * 0.3)
                    })
                });
            let Some(host) = host else { continue };
            let raised = glyphs[rows[host][0]].baseline > first.baseline;
            let texts: Option<Vec<String>> = group
                .iter()
                .map(|&i| {
                    glyphs[i]
                        .text
                        .chars()
                        .map(|c| script_form(c, raised))
                        .collect()
                })
                .collect();
            if let Some(texts) = texts {
                moves.extend(group.iter().zip(texts).map(|(&i, t)| (ri, host, i, t)));
            }
        }
    }
    if moves.is_empty() {
        return;
    }
    for (from, to, glyph, text) in moves {
        rows[from].retain(|&i| i != glyph);
        rows[to].push(glyph);
        glyphs[glyph].text = text;
    }
    rows.retain(|row| !row.is_empty());
    for row in rows.iter_mut() {
        row.sort_by(|&a, &b| glyphs[a].rect.x0.total_cmp(&glyphs[b].rect.x0));
    }
}

fn line_text(glyphs: &[G], members: &[usize]) -> String {
    let mut out = String::new();
    let mut prev: Option<&G> = None;
    for &i in members {
        let g = &glyphs[i];
        if g.space {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        if let Some(p) = prev {
            let gap = g.rect.x0 - p.rect.x1;
            if gap > p.size.min(g.size) * 0.18 && !out.ends_with(' ') {
                out.push(' ');
            }
        }
        out.push_str(&g.text);
        prev = Some(g);
    }
    out.trim().to_owned()
}

/// What sets words apart inside a paragraph: bold, italic, a fixed pitch, the colour.
type Look = (bool, bool, bool, u32);

fn own_look(base: &TextStyle) -> Look {
    (base.bold, base.italic, base.monospace, base.color)
}

/// The look of a run of text inside a block. A colour close to the block's own is the block's own: black is
/// written in more than one way.
fn look(content: &PageContent, run: usize, base: &TextStyle) -> Look {
    let r = &content.runs[run];
    let far = (0..3).any(|k| {
        let own = (base.color >> (16 - 8 * k)) & 0xFF;
        u32::from(r.color[k]).abs_diff(own) > 40
    });
    let color =
        (u32::from(r.color[0]) << 16) | (u32::from(r.color[1]) << 8) | u32::from(r.color[2]);
    (
        r.font.bold,
        r.font.italic,
        r.font.fixed_pitch,
        if far { color } else { base.color },
    )
}

/// The styles of the words set apart inside a block, in the order they come, and the look each is known by.
fn inline_styles(
    content: &PageContent,
    glyphs: &[G],
    members: impl Iterator<Item = usize>,
    base: &TextStyle,
) -> (Vec<TextStyle>, Vec<Look>) {
    let own = own_look(base);
    let (mut styles, mut looks) = (Vec::new(), Vec::new());
    for i in members {
        let g = &glyphs[i];
        if g.space || !g.text.chars().any(char::is_alphanumeric) {
            continue;
        }
        let look = look(content, g.at.run, base);
        if look == own || looks.contains(&look) {
            continue;
        }
        if looks.len() == crate::inline::MAX_STYLES {
            break;
        }
        looks.push(look);
        styles.push(TextStyle {
            color: look.3,
            ..run_style(content, g.at.run, base.font_size)
        });
    }
    (styles, looks)
}

/// [`line_text`], and where in it the style changes.
fn styled_text(
    content: &PageContent,
    glyphs: &[G],
    members: &[usize],
    base: &TextStyle,
    looks: &[Look],
) -> (String, Vec<Span>) {
    if looks.is_empty() {
        return (line_text(glyphs, members), Vec::new());
    }
    let own = own_look(base);
    let ink: Vec<&G> = members
        .iter()
        .map(|&i| &glyphs[i])
        .filter(|g| !g.space)
        .collect();
    // The style each glyph is printed in, if it is one of the block's.
    let printed: Vec<Option<usize>> = ink
        .iter()
        .map(|g| {
            let look = look(content, g.at.run, base);
            if look == own {
                Some(0)
            } else {
                looks.iter().position(|l| *l == look).map(|k| k + 1)
            }
        })
        .collect();
    // Punctuation goes with the words beside it: a comma that happens to be in italics after an upright word
    // is not worth a mention. Only in a fixed-width face does it stand for itself.
    let mut styles: Vec<usize> = Vec::with_capacity(ink.len());
    for (n, g) in ink.iter().enumerate() {
        let before = styles.last().copied().unwrap_or(0);
        let word = g.text.chars().any(char::is_alphanumeric);
        styles.push(match printed[n] {
            None if word => 0,
            None => before,
            Some(k) if k == 0 || word => k,
            Some(k) if looks[k - 1].2 && !base.monospace => k,
            Some(k) if before == k || printed.get(n + 1) == Some(&Some(k)) => k,
            Some(_) => 0,
        });
    }
    let mut out = String::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut prev: Option<&G> = None;
    let (mut n, mut current) = (0, 0);
    for &i in members {
        let g = &glyphs[i];
        if g.space {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        if let Some(p) = prev {
            let gap = g.rect.x0 - p.rect.x1;
            if gap > p.size.min(g.size) * 0.18 && !out.ends_with(' ') {
                out.push(' ');
            }
        }
        if styles[n] != current {
            current = styles[n];
            spans.push(Span {
                at: out.len() as u32,
                style: current as u8,
            });
        }
        n += 1;
        out.push_str(&g.text);
        prev = Some(g);
    }
    let lead = (out.len() - out.trim_start().len()) as u32;
    let text = out.trim().to_owned();
    for span in &mut spans {
        span.at = span.at.saturating_sub(lead);
    }
    spans.retain(|s| (s.at as usize) < text.len());
    (text, spans)
}

fn run_style(content: &PageContent, run: usize, size: f32) -> TextStyle {
    let r = &content.runs[run];
    let f = &r.font;
    TextStyle {
        font_name: (!f.name.is_empty()).then(|| f.name.clone()),
        font_size: size,
        color: (u32::from(r.color[0]) << 16) | (u32::from(r.color[1]) << 8) | u32::from(r.color[2]),
        bold: f.bold,
        italic: f.italic,
        serif: f.serif,
        monospace: f.fixed_pitch,
    }
}

/// Bold, italic, serif, monospace, colour and font name.
type StyleKey = (bool, bool, bool, bool, [u8; 3], String);

/// The style used by most characters of a block, with the median line size.
fn dominant_style(
    content: &PageContent,
    glyphs: &[G],
    members: &[usize],
    sizes: impl Iterator<Item = f32>,
) -> TextStyle {
    // The letters decide, not what stands between them: the row of dots that leads to a page number is
    // longer than the title before it and often printed lighter, and an entry of an index may have more
    // digits, in the colour of a link, than letters.
    let count = |wanted: fn(&char) -> bool| {
        let mut votes: BTreeMap<usize, usize> = BTreeMap::new();
        for &i in members.iter().filter(|&&i| !glyphs[i].space) {
            let n = glyphs[i].text.chars().filter(wanted).count();
            if n > 0 {
                *votes.entry(glyphs[i].at.run).or_default() += n;
            }
        }
        votes
    };
    let mut votes = count(|c| c.is_alphabetic());
    if votes.is_empty() {
        votes = count(|c| c.is_alphanumeric());
    }
    if votes.is_empty() {
        votes = count(|_| true);
    }
    // Votes per distinct style rather than per run.
    let mut by_style: HashMap<StyleKey, (usize, usize)> = HashMap::new();
    for (&run, &count) in &votes {
        let r = &content.runs[run];
        let key = (
            r.font.bold,
            r.font.italic,
            r.font.serif,
            r.font.fixed_pitch,
            r.color,
            r.font.name.clone(),
        );
        let e = by_style.entry(key).or_insert((0, run));
        e.0 += count;
    }
    // Two styles with as many letters each: the one that comes first, so that a page is read the same way
    // every time.
    let run = by_style
        .values()
        .max_by_key(|(count, run)| (*count, std::cmp::Reverse(*run)))
        .map_or(0, |(_, run)| *run);
    let mut sizes: Vec<f32> = sizes.collect();
    sizes.sort_by(f32::total_cmp);
    run_style(
        content,
        run,
        sizes.get(sizes.len() / 2).copied().unwrap_or(10.0),
    )
}

fn detect_align(lines: &[TextLine], block: &Rect, size: f32) -> Align {
    if lines.len() < 2 {
        return Align::Left;
    }
    let tol = (size * 0.25).max(1.0);
    let body = &lines[..lines.len() - 1];
    // The lines after the first share a left edge (the first may be indented, or hang out to the left).
    let rest_left = lines[1].rect.x0;
    let lefts = lines
        .iter()
        .skip(1)
        .all(|l| (l.rect.x0 - rest_left).abs() < tol);
    let rights = body.iter().all(|l| (l.rect.x1 - block.x1).abs() < tol);
    let centers = lines
        .iter()
        .all(|l| ((l.rect.x0 + l.rect.x1) - (block.x0 + block.x1)).abs() / 2.0 < tol);
    let all_rights = lines.iter().all(|l| (l.rect.x1 - block.x1).abs() < tol);
    let hanging = lines
        .iter()
        .skip(1)
        .any(|l| l.rect.x0 > lines[0].rect.x0 + size * 0.7)
        && (lines[0].rect.x0 - block.x0).abs() < tol;
    // Lines of nearly one length that begin at exactly one place are set from the left.
    let flush = lines
        .iter()
        .all(|l| (l.rect.x0 - lines[0].rect.x0).abs() < 0.3);
    // Lines that fill the block from edge to edge are centred too, but only by being as wide as it is.
    let full = lefts && all_rights;
    if centers && !hanging && !flush && !full {
        Align::Center
    } else if lefts && rights && lines.len() >= 2 {
        Align::Justify
    } else if all_rights && !lefts {
        Align::Right
    } else {
        Align::Left
    }
}

/// "example, 359", "merging, 391–393", or nothing but numbers: the way an entry of an index ends.
fn is_index_entry(line: &str) -> bool {
    let number = |w: &str| {
        let w = w.trim_end_matches(['.', ',', ';']);
        w.starts_with(|c: char| c.is_ascii_digit())
            && w.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | '–' | '—'))
    };
    let words: Vec<&str> = line.split_whitespace().collect();
    let numbers = words.iter().rev().take_while(|w| number(w)).count();
    numbers >= 1
        && (numbers == words.len()
            || (words[words.len() - numbers - 1].ends_with(',') && words.len() <= 12))
}

fn ends_in_page_number(line: &str) -> bool {
    let number = |w: &str| {
        let w = w.trim_end_matches(['.', ',', ';']);
        w.starts_with(|c: char| c.is_ascii_digit())
            && w.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | '–' | '—'))
    };
    let mut words = line.split_whitespace().rev();
    match (words.next(), words.next()) {
        (Some(last), Some(before)) => number(last) && (before.ends_with(',') || number(before)),
        (Some(last), None) => number(last) && last.contains(['-', '–', '—']),
        _ => false,
    }
}

/// Find blocks whose lines are separate entries. A line is compared with the right edge of its column (the
/// widest text block it shares horizontal space with), not of its own block: two table-of-contents entries grouped
/// into one block have a "longest" line that still stops far short of where running text would end.
fn mark_separate_lines(blocks: &mut [TextBlock]) {
    // Each text block with whether it reads as running text: three lines or more, of some length.
    let text: Vec<(f32, f32, bool)> = blocks
        .iter()
        .filter(|b| !b.protected)
        .map(|b| {
            let chars: usize = b.lines.iter().map(|l| l.text.trim().chars().count()).sum();
            (
                b.rect.x0,
                b.rect.x1,
                b.lines.len() >= 3 && chars >= 30 * b.lines.len(),
            )
        })
        .collect();
    for b in blocks
        .iter_mut()
        .filter(|b| !b.protected && b.lines.len() >= 2)
    {
        let (x0, x1) = (b.rect.x0, b.rect.x1);
        // A headline, a deck or a footnote set across two columns is not the column of the text under it.
        // It shows by reaching well into running text that stands wholly beside this block.
        let beside = |c: &&(f32, f32, bool)| {
            c.2 && (c.0 >= x1 - 1.0 || c.1 <= x0 + 1.0) && c.1 - c.0 >= 0.5 * (x1 - x0)
        };
        let across = |a0: f32, a1: f32| {
            text.iter()
                .filter(beside)
                .any(|c| a1.min(c.1) - a0.max(c.0) >= 0.25 * (c.1 - c.0))
        };
        let column = text
            .iter()
            .filter(|(a0, a1, _)| *a0 < x1 && *a1 > x0 && !across(*a0, *a1))
            .fold((x0, x1), |(l, r), (a0, a1, _)| (l.min(*a0), r.max(*a1)));
        // Entries can fill their lines as well: those of an index end in page numbers, and a list of
        // sentences ends each line with a full stop.
        let ending = |test: fn(&str) -> bool| {
            let n = b.lines.iter().filter(|l| test(l.text.trim())).count();
            (n, b.lines.len())
        };
        let (numbers, of) = ending(ends_in_page_number);
        let (stops, _) = ending(|t| t.ends_with(['.', ';', '!', '?']));
        b.separate_lines = crate::layout::lines_end_short(&b.lines, column.0, column.1)
            || numbers * 2 >= of
            || (of >= 3 && stops == of);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of text starting at `x`, one 5pt-wide glyph per character, then `number` with its right edge at
    /// `number_right`.
    fn row_glyphs(
        glyphs: &mut Vec<G>,
        y: f32,
        text: &str,
        number: &str,
        number_right: f32,
    ) -> Vec<usize> {
        let mut row = Vec::new();
        let mut push = |glyphs: &mut Vec<G>, c: char, x0: f32| {
            row.push(glyphs.len());
            let n = glyphs.len();
            glyphs.push(G {
                text: c.to_string(),
                rect: Rect {
                    x0,
                    y0: y,
                    x1: x0 + 5.0,
                    y1: y + 10.0,
                },
                baseline: y + 8.0,
                size: 10.0,
                at: GlyphRef { run: 0, glyph: n },
                space: c == ' ',
            });
        };
        for (k, c) in text.chars().enumerate() {
            push(glyphs, c, 5.0 * k as f32);
        }
        let start = number_right - 5.0 * number.chars().count() as f32;
        for (k, c) in number.chars().enumerate() {
            push(glyphs, c, start + 5.0 * k as f32);
        }
        row
    }

    #[test]
    fn page_number_columns_split_even_when_the_gap_is_narrow() {
        let mut g = Vec::new();
        let rows = vec![
            row_glyphs(&mut g, 0.0, "Short entry", "331", 300.0),
            row_glyphs(&mut g, 14.0, "Another entry", "335", 300.0),
            // 56 characters end at 280 and the number starts at 285: half an em, narrower than a column gap.
            row_glyphs(
                &mut g,
                28.0,
                "A very long entry that nearly reaches its number columnX",
                "366",
                300.0,
            ),
            row_glyphs(&mut g, 42.0, "Fourth", "339", 300.0),
            // A word ending in a letter that is also a roman numeral is not a page number.
            row_glyphs(&mut g, 56.0, "Mimari", "", 300.0),
        ];
        let split = find_gutters(&g, &rows);
        let first_number_glyph =
            |r: usize| *rows[r].iter().find(|&&i| g[i].rect.x0 >= 284.0).unwrap();
        assert!(split.contains(&(0, first_number_glyph(0))));
        assert!(
            split.contains(&(2, first_number_glyph(2))),
            "the long entry is cut from its number"
        );
        assert!(split.contains(&(3, first_number_glyph(3))));
        assert!(
            !split.iter().any(|(r, _)| *r == 4),
            "no split inside a word"
        );
    }

    #[test]
    fn index_entries() {
        assert!(is_index_entry("example, 359"));
        assert!(is_index_entry("merging, 391–393"));
        assert!(is_index_entry("of a shared kernel, 354, 356–358,"));
        assert!(is_index_entry("412, 415"));
        // A sentence that happens to end in a number, and a year after a comma far into a long line.
        assert!(!is_index_entry("The first edition appeared in 1974"));
        assert!(!is_index_entry(
            "It was introduced by the company in the spring of that year and sold well until the end of the decade, 1979"
        ));
    }

    #[test]
    fn a_drawing_under_a_later_fill_is_not_seen() {
        let path = |bounds: [f32; 4], is_rect, stroked, filled| interp::PathItem {
            bounds,
            is_rect,
            stroked,
            filled,
            fill: [255, 255, 255],
            after: 0,
        };
        let art = path([100.0, 100.0, 300.0, 300.0], false, false, true);
        // Wholly under a panel drawn after it: gone. Under one drawn before it: all of it shows.
        let panel = path([50.0, 50.0, 400.0, 400.0], true, false, true);
        assert_eq!(uncovered(&[art, panel])[0], None);
        assert_eq!(uncovered(&[panel, art])[1], Some(art.bounds));
        // A panel over its lower half leaves the upper half.
        let half = path([50.0, 50.0, 400.0, 200.0], true, false, true);
        assert_eq!(
            uncovered(&[art, half])[0],
            Some([100.0, 200.0, 300.0, 300.0])
        );
        // The line around a box stays when the box is filled again at the same size.
        let frame = path([100.0, 100.0, 300.0, 300.0], true, true, false);
        let fill = path([100.0, 100.0, 300.0, 300.0], true, false, true);
        assert_eq!(uncovered(&[frame, fill])[0], Some(frame.bounds));
    }

    #[test]
    fn a_headline_across_two_columns_is_not_the_column_of_the_text_under_it() {
        let block = |id: &str, x0: f32, x1: f32, y: f32, lines: usize| {
            let line = |i: usize| TextLine {
                rect: Rect {
                    x0,
                    y0: y + 12.0 * i as f32,
                    x1,
                    y1: y + 12.0 * i as f32 + 10.0,
                },
                text: "running text that fills the measure of its column".into(),
                baseline: y + 12.0 * i as f32 + 8.0,
                spans: Vec::new(),
            };
            TextBlock {
                id: BlockId(id.into()),
                rect: Rect {
                    x0,
                    y0: y,
                    x1,
                    y1: y + 12.0 * lines as f32,
                },
                lines: (0..lines).map(line).collect(),
                style: TextStyle {
                    font_name: None,
                    font_size: 10.0,
                    color: 0,
                    bold: false,
                    italic: false,
                    serif: true,
                    monospace: false,
                },
                align: Align::Justify,
                protected: false,
                separate_lines: false,
                initial: None,
                inline: Vec::new(),
            }
        };
        // A deck over both columns, then two columns of running text.
        let mut page = vec![
            block("deck", 40.0, 430.0, 60.0, 1),
            block("left", 40.0, 260.0, 100.0, 5),
            block("right", 280.0, 500.0, 100.0, 5),
        ];
        mark_separate_lines(&mut page);
        assert!(!page[1].separate_lines, "the left column is running text");
        assert!(!page[2].separate_lines);

        // Without a second column the wide block is the measure, and lines that stop halfway are entries.
        let mut page = vec![
            block("wide", 40.0, 430.0, 60.0, 1),
            block("entries", 40.0, 220.0, 100.0, 5),
        ];
        mark_separate_lines(&mut page);
        assert!(page[1].separate_lines);
    }

    #[test]
    fn code_tokens() {
        assert!(looks_like_code("getPushesFromPinNumber(int)"));
        assert!(looks_like_code("anIterator.hasNext()"));
        assert!(looks_like_code("user_id"));
        assert!(!looks_like_code("Component Type"));
        assert!(!looks_like_code("Developer:"));
        assert!(!looks_like_code("JavaScript"));
    }
}
