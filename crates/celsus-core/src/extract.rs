//! Turning the glyphs of a page into translatable blocks.
//!
//! Glyphs are joined into lines and lines into blocks using geometry only. Ruled lines, boxes and images act as
//! walls: text on two sides of a table rule, or inside two different diagram boxes, never ends up in one block.

use std::collections::{BTreeMap, HashMap};

use celsus_pdf::interp::{self, PageContent};
use celsus_pdf::{Document, Page};

use crate::layout::{Align, BlockId, PageLayout, Rect, TextBlock, TextLine, TextStyle, join_lines};

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
}

/// Extract the blocks of `page` (number is 1-based, for the layout record).
pub fn extract_page(doc: &Document, page: &Page, number: u32) -> celsus_pdf::Result<ExtractedPage> {
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
    let mut glyphs = Vec::new();
    let mut rotated_runs = Vec::new();
    for (ri, run) in content.runs.iter().enumerate() {
        if matches!(run.render_mode, 3 | 7) || run.size < 1.0 {
            continue;
        }
        if run.angle.abs() > 0.02 {
            rotated_runs.push(ri);
            continue;
        }
        for (gi, g) in run.glyphs.iter().enumerate() {
            if g.text.is_empty() || !g.visible {
                continue;
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
    for p in content.paths.iter().filter(|p| p.is_rect) {
        let r = to_rect(p.bounds);
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
    for b in &content.images {
        let r = to_rect(*b);
        if r.width() * r.height() < page_area * 0.7 {
            containers.push(r);
        }
    }
    let mut obstacles: Vec<Rect> = Vec::new();
    for b in content
        .paths
        .iter()
        .filter(|p| p.stroked || p.fill != [255, 255, 255])
        .map(|p| p.bounds)
        .chain(content.images.iter().copied())
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
        containers
            .iter()
            .enumerate()
            .filter(|(_, c)| cx >= c.x0 && cx <= c.x1 && cy >= c.y0 && cy <= c.y1)
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

    for row in rows.iter_mut() {
        row.sort_by(|&a, &b| glyphs[a].rect.x0.total_cmp(&glyphs[b].rect.x0));
    }
    let gutters = find_gutters(&glyphs, &rows);

    // Lines: split rows at wide gaps, column gutters, walls and container changes.
    let mut lines: Vec<Line> = Vec::new();
    for (ri, row) in rows.into_iter().enumerate() {
        let mut current: Vec<usize> = Vec::new();
        let mut last_ink: Option<usize> = None;
        for i in row {
            let g = &glyphs[i];
            if let Some(prev) = last_ink.map(|p| &glyphs[p]) {
                let gap = g.rect.x0 - prev.rect.x1;
                let size = prev.size.max(g.size);
                let split = gap > size * 1.6
                    || gutters.contains(&(ri, i))
                    || (!g.space
                        && v_wall_between(
                            prev.rect.x1 - 0.5,
                            g.rect.x0 + 0.5,
                            g.rect.y0,
                            g.rect.y1,
                        ))
                    || (!g.space && container_of(&prev.rect) != container_of(&g.rect));
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

    // Blocks: stack lines that continue each other.
    lines.sort_by(|a, b| {
        a.rect
            .y0
            .total_cmp(&b.rect.y0)
            .then(a.rect.x0.total_cmp(&b.rect.x0))
    });
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let target = groups.iter().rposition(|g| {
            let prev = &lines[*g.last().unwrap()];
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
            let short_prev = prev.rect.x1 < right - size * 1.5 && right - block_x0 > size * 8.0;
            // A first-line indent only counts when the block so far sits flush left (not a hanging indent).
            let flush = g.iter().all(|&i| (lines[i].rect.x0 - block_x0).abs() < 1.0);
            let indented = g.len() >= 2 && flush && line.rect.x0 > prev.rect.x0 + size * 0.7;
            !((short_prev && !centred) || indented)
        });
        match target {
            Some(gi) if gi + 8 >= groups.len() => groups[gi].push(li),
            _ => groups.push(vec![li]),
        }
    }

    // Reading order: the order the content stream draws them, which producers almost always write in.
    groups.sort_by_key(|g| {
        g.iter()
            .map(|&i| lines[i].first_run)
            .min()
            .unwrap_or(usize::MAX)
    });

    let mut blocks = Vec::new();
    let mut refs = HashMap::new();
    for group in &groups {
        let id = BlockId(format!("b{:04}", blocks.len() + 1));
        let text_lines: Vec<TextLine> = group
            .iter()
            .map(|&li| {
                let l = &lines[li];
                TextLine {
                    rect: l.rect,
                    text: line_text(&glyphs, &l.glyphs),
                    baseline: l.baseline,
                }
            })
            .collect();
        let rect = text_lines
            .iter()
            .skip(1)
            .fold(text_lines[0].rect, |r, l| r.union(&l.rect));
        let members: Vec<usize> = group
            .iter()
            .flat_map(|&li| lines[li].glyphs.iter().copied())
            .collect();
        let style = dominant_style(
            &content,
            &glyphs,
            &members,
            group.iter().map(|&li| lines[li].size),
        );
        let source = join_lines(text_lines.iter().map(|l| l.text.as_str()));
        let letters = source.chars().filter(|c| c.is_alphabetic()).count();
        let protected = letters < 2 || style.monospace || looks_like_code(&source);
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
        });
    }
    mark_separate_lines(&mut blocks);

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
            }],
            style: run_style(&content, ri, run.size),
            align: Align::Left,
            protected: true,
            separate_lines: false,
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
        glyphs: refs,
        obstacles,
        content,
    })
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
    }
    let mut gaps = Vec::new();
    let mut edges = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let mut prev: Option<&G> = None;
        for &i in row {
            let g = &glyphs[i];
            if g.space {
                continue;
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
        let partner = gaps.iter().any(|o| {
            o.wide
                && o.row != gap.row
                && ((o.x0 - gap.x0).abs() < 1.0 || (o.x1 - gap.x1).abs() < 1.0)
        });
        if shared_edge && partner {
            out.insert((gap.row, gap.after));
        }
    }
    out
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
    })
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
    let mut votes: BTreeMap<usize, usize> = BTreeMap::new();
    for &i in members {
        if !glyphs[i].space {
            *votes.entry(glyphs[i].at.run).or_default() += glyphs[i].text.chars().count();
        }
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
    let run = by_style
        .values()
        .max_by_key(|(count, _)| *count)
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
    if centers && !hanging && !(lefts && all_rights) {
        Align::Center
    } else if lefts && rights && lines.len() >= 2 {
        Align::Justify
    } else if all_rights && !lefts {
        Align::Right
    } else {
        Align::Left
    }
}

/// Find blocks whose lines are separate entries. A line is compared with the right edge of its column (the
/// widest text block it shares horizontal space with), not of its own block: two table-of-contents entries grouped
/// into one block have a "longest" line that still stops far short of where running text would end.
fn mark_separate_lines(blocks: &mut [TextBlock]) {
    let text: Vec<(f32, f32)> = blocks
        .iter()
        .filter(|b| !b.protected)
        .map(|b| (b.rect.x0, b.rect.x1))
        .collect();
    for b in blocks
        .iter_mut()
        .filter(|b| !b.protected && b.lines.len() >= 2)
    {
        let (x0, x1) = (b.rect.x0, b.rect.x1);
        let column = text
            .iter()
            .filter(|(a0, a1)| *a0 < x1 && *a1 > x0)
            .fold((x0, x1), |(l, r), (a0, a1)| (l.min(*a0), r.max(*a1)));
        b.separate_lines = crate::layout::lines_end_short(&b.lines, column.0, column.1);
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
    fn code_tokens() {
        assert!(looks_like_code("getPushesFromPinNumber(int)"));
        assert!(looks_like_code("anIterator.hasNext()"));
        assert!(looks_like_code("user_id"));
        assert!(!looks_like_code("Component Type"));
        assert!(!looks_like_code("Developer:"));
        assert!(!looks_like_code("JavaScript"));
    }
}
