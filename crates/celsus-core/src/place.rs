//! Placing the translations of one page.
//!
//! A translation is rarely as long as the text it replaces, so the blocks of a page cannot be fitted one by one:
//! a paragraph that grows by a line has to move the paragraphs under it, and it must stop at a picture, a table
//! rule or text that stays where it is. The page is solved as a whole:
//!
//! - Every block gets a frame: the horizontal span its lines may use. Running text keeps its measure. A heading,
//!   a label or a table cell may grow into the empty space beside it, up to the nearest text or drawing.
//! - Blocks under one another form chains. A block that grows pushes the blocks below it down, never up, and
//!   never past its floor: the next thing that cannot move, or the end of the text area.
//! - When a chain does not fit, its blocks go down a ladder of ever stronger measures: slightly narrower
//!   letters, tighter line spacing, less space between paragraphs, then smaller sizes down to a readable floor.
//!   Paragraphs of the same size on a page end up at the same step, so the page keeps an even look.
//! - The result is checked line by line. Text that would touch other text or a drawing is confined to the place
//!   of its original, and if it still does not fit there, the original stays and is reported.

use std::collections::HashMap;

use crate::fit::{self, Indent, Measure};
use crate::layout::{Align, BlockId, PageLayout, Rect, TextBlock};

/// Translations may shrink to this fraction of the original size,
pub const MIN_SCALE: f32 = 0.7;
/// and never below this size in points.
pub const MIN_SIZE: f32 = 6.0;

/// A translated block to place.
pub struct Item<'a> {
    pub block: &'a TextBlock,
    pub text: &'a str,
    pub measure: &'a dyn Measure,
    /// Text that is not translated (a bullet, a page number) but sits on the same row as translated text, and is
    /// drawn again only if that row has to move.
    pub companion: bool,
    /// The room a large initial takes, drawn by the caller: `text` is what follows it, and runs around it.
    pub initial: Option<Rect>,
    /// The translation must not go lower than its original did: what lies under it on the page is not known.
    /// So it is with text on a scanned page, where a figure is part of the picture and nothing says where.
    pub confined: bool,
}

/// Whether the translation of `block` opens with a large initial again.
pub fn takes_initial(block: &TextBlock) -> bool {
    block.initial.is_some()
        && is_running(block)
        && matches!(block.align, Align::Left | Align::Justify)
}

/// Something on the page that is not text to translate: a drawing, a rule, a listing that stays as it is. It
/// keeps its shape. Text that grows above it may move it down as it is, with what stands inside it, as far as
/// there is room under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    pub rect: Rect,
    /// Whether it can be taken out of its place at all.
    pub movable: bool,
}

/// Where everything ended up.
#[derive(Debug, Clone, PartialEq)]
pub struct Solved {
    /// One entry per item, `None` where the translation cannot be placed legibly and the original has to stay.
    pub placed: Vec<Option<Placed>>,
    /// How far down each body moved; less than nothing for one that went up.
    pub moved: Vec<f32>,
    /// For each item, how far the body it stands in took it down.
    pub carried: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    /// Left edge and baseline, in points from the top-left of the page.
    pub x: f32,
    pub baseline: f32,
    /// Width as drawn.
    pub width: f32,
    /// The width a justified line is stretched to.
    pub fill: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub size: f32,
    /// Horizontal scaling of the letters, 1.0 for none.
    pub squeeze: f32,
    pub lines: Vec<Line>,
    /// How far the block moved down from where its original stood.
    pub shift: f32,
}

/// One rung of the ladder. Every rung takes at most as much room as the one before.
struct Step {
    squeeze: f32,
    /// Line spacing, as a fraction of the original's.
    lead: f32,
    scale: f32,
    /// Space kept between paragraphs, as a fraction of the original's.
    gap: f32,
}

const fn step(squeeze: f32, lead: f32, scale: f32, gap: f32) -> Step {
    Step {
        squeeze,
        lead,
        scale,
        gap,
    }
}

const LADDER: &[Step] = &[
    step(1.0, 1.0, 1.0, 1.0),
    step(0.97, 1.0, 1.0, 1.0),
    step(0.95, 0.97, 1.0, 0.9),
    step(0.93, 0.95, 1.0, 0.8),
    step(0.93, 0.95, 0.96, 0.7),
    step(0.93, 0.94, 0.92, 0.6),
    step(0.93, 0.93, 0.88, 0.5),
    step(0.93, 0.93, 0.84, 0.5),
    step(0.93, 0.93, 0.80, 0.5),
    step(0.93, 0.93, 0.76, 0.5),
    step(0.93, 0.93, 0.72, 0.5),
    step(0.93, 0.93, MIN_SCALE, 0.5),
];

/// What is tried to keep a single line on one line, and the entries of a list on one line each, before the text
/// is allowed to wrap: (squeeze, scale).
const ONE_LINE: &[(f32, f32)] = &[
    (1.0, 1.0),
    (0.97, 1.0),
    (0.94, 1.0),
    (0.91, 1.0),
    (0.91, 0.95),
    (0.91, 0.9),
    (0.91, 0.85),
];
const PER_ENTRY: &[(f32, f32)] = &[
    (1.0, 1.0),
    (0.97, 1.0),
    (0.94, 1.0),
    (0.92, 1.0),
    (0.92, 0.95),
    (0.92, 0.9),
    (0.92, 0.85),
    (0.92, 0.8),
    (0.92, 0.75),
    (0.92, MIN_SCALE),
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// One line: a heading, a label, a table cell.
    Single,
    /// Lines that are separate entries.
    Entries,
    /// A paragraph.
    Running,
    /// Not text: a body of the page, which only ever moves.
    Body,
}

#[derive(Clone)]
struct Shape {
    size: f32,
    squeeze: f32,
    leading: f32,
    /// Each line: its text, its indent, and how far it stays clear of the right edge.
    lines: Vec<(String, f32, f32)>,
    height: f32,
    /// The lines stand on the baselines of the original lines.
    own: bool,
    gap: f32,
    /// A word had to be broken in two to fit its line, and would fit whole at a size further down the ladder.
    cramped: bool,
}

struct Node {
    r: Rect,
    size0: f32,
    /// Baseline distance as a multiple of the size.
    ratio: f32,
    /// From the top of the block to its first baseline, and from its last baseline to its bottom.
    base_off: f32,
    desc_off: f32,
    indent: Indent,
    align: Align,
    kind: Kind,
    /// The span the lines may use.
    left: f32,
    right: f32,
    floor: f32,
    /// The floor is the end of the page's text area rather than something on the page.
    page_floor: bool,
    /// How high the block may go, when the text above it came out shorter than it was and the block is short
    /// of room: up to the nearest thing above it that stays, with the space that was between the two.
    ceiling: f32,
    /// A paragraph that follows another with less than a blank line between them: the two are one run of
    /// text. It keeps its distance to the paragraph above, wherever that ends, so that a paragraph that came
    /// out shorter leaves no hole in the middle of the text.
    follows: bool,
    /// Tied to something that stays in place.
    anchored: bool,
    /// Blocks directly above, with the space that was between the two.
    above: Vec<(usize, f32)>,
    row: usize,
    fixed: Option<Shape>,
    /// Things the text runs around: a picture or a large initial that reaches into the block from a side.
    around: Vec<Rect>,
}

impl Node {
    fn width(&self) -> f32 {
        (self.right - self.left).max(self.size0)
    }

    /// How far line `n` has to stay clear of the left and the right edge of the frame, for lines of `size`
    /// that are `leading` apart.
    fn insets(&self, n: usize, size: f32, leading: f32) -> (f32, f32) {
        if self.around.is_empty() {
            return (0.0, 0.0);
        }
        let baseline = self.r.y0 + self.base_off * size / self.size0 + leading * n as f32;
        let (y0, y1) = (baseline - size * 0.75, baseline + size * 0.25);
        let middle = (self.left + self.right) / 2.0;
        let gap = size * 0.4;
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for o in self
            .around
            .iter()
            .filter(|o| o.y0 < y1 - 0.5 && o.y1 > y0 + 0.5)
        {
            if (o.x0 + o.x1) / 2.0 < middle {
                left = left.max(o.x1 + gap - self.left);
            } else {
                right = right.max(self.right - (o.x0 - gap));
            }
        }
        // Never so much that no room is left for words.
        let most = (self.width() - size * 4.0).max(0.0);
        (left.min(most), right.min((most - left).max(0.0)))
    }

    /// The horizontal span the block can ever cover.
    fn span(&self) -> (f32, f32) {
        (self.left.min(self.r.x0), self.right.max(self.r.x1))
    }
}

struct State {
    /// 0: free. 1: no wider than the original. 2: also no lower.
    restrict: Vec<u8>,
    kept: Vec<bool>,
    /// Bodies that stay where they are.
    held: Vec<bool>,
    /// The bottom margin may be used further than usual.
    deep: bool,
    /// Pairs of blocks of which the second began on the last line of the first and now begins under it: the
    /// first came out longer, and its last line has no room left for the start of the other.
    apart: Vec<(usize, usize)>,
}

enum Change {
    Keep(usize),
    Restrict(usize),
    /// This body stays where it is.
    Hold(usize),
    /// The second block begins under the first, not on its last line.
    Apart(usize, usize),
    Deep,
}

/// A measure with horizontally scaled letters.
struct Squeezed<'a> {
    inner: &'a dyn Measure,
    by: f32,
}

impl Measure for Squeezed<'_> {
    fn width(&self, text: &str, size: f32) -> f32 {
        self.inner.width(text, size) * self.by
    }
    fn ascent(&self) -> f32 {
        self.inner.ascent()
    }
    fn descent(&self) -> f32 {
        self.inner.descent()
    }
}

type ShapeCache = HashMap<(usize, usize, u32), Shape>;

/// Place `items` on the page. The result has one entry per item, `None` where the translation cannot be placed
/// legibly and the original has to stay.
pub fn solve(
    layout: &PageLayout,
    obstacles: &[Rect],
    items: &[Item],
    rtl: bool,
) -> Vec<Option<Placed>> {
    solve_among(layout, obstacles, &[], items, rtl).placed
}

/// Place `items` on a page whose `bodies` are known: what is on it besides the text to translate. A body that
/// can move makes room for the text above it by going down, before that text is made smaller.
pub fn solve_among(
    layout: &PageLayout,
    obstacles: &[Rect],
    bodies: &[Body],
    items: &[Item],
    rtl: bool,
) -> Solved {
    let n = items.len();
    let mut state = State {
        restrict: vec![0; n],
        kept: items
            .iter()
            .map(|it| {
                let s = it.block.style.font_size;
                it.block.lines.is_empty() || !(s > 0.0 && s < 2000.0) || it.text.trim().is_empty()
            })
            .collect(),
        held: bodies.iter().map(|b| !b.movable).collect(),
        deep: false,
        apart: Vec::new(),
    };
    let mut cache = ShapeCache::new();
    // Every change confines or gives up one more block, holds one more body or sets one more pair apart, so this
    // ends.
    for _ in 0..n * 6 + bodies.len() + 8 {
        match attempt(layout, obstacles, bodies, items, rtl, &state, &mut cache) {
            Ok(solved) => return solved,
            Err(Change::Deep) => state.deep = true,
            Err(Change::Apart(a, b)) => state.apart.push((a, b)),
            Err(Change::Hold(b)) => state.held[b] = true,
            Err(Change::Keep(i)) => state.kept[i] = true,
            Err(Change::Restrict(i)) => {
                if state.restrict[i] >= 2 {
                    state.kept[i] = true;
                } else {
                    state.restrict[i] += 1;
                    cache.retain(|k, _| k.0 != i);
                }
            }
        }
    }
    Solved {
        placed: vec![None; n],
        moved: vec![0.0; bodies.len()],
        carried: vec![0.0; n],
    }
}

fn x_overlap(a: (f32, f32), b: (f32, f32)) -> bool {
    a.0 < b.1 - 0.5 && a.1 > b.0 + 0.5
}

/// Whether `o` lies under `r`: it starts in the lower half of `r` or below, and ends below it.
fn is_below(r: &Rect, o: &Rect) -> bool {
    o.y0 >= r.y0 + r.height() * 0.5 && o.y1 > r.y1
}

fn encloses(o: &Rect, r: &Rect) -> bool {
    o.x0 <= r.x0 + 0.5 && o.x1 >= r.x1 - 0.5 && o.y0 <= r.y0 + 0.5 && o.y1 >= r.y1 - 0.5
}

/// First-line and hanging indents of a block, measured from its left edge.
fn indent_of(block: &TextBlock) -> Indent {
    if block.lines.len() < 2 || !matches!(block.align, Align::Left | Align::Justify) {
        return Indent::default();
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
    Indent {
        first: keep(first),
        rest: keep(rest),
    }
}

fn is_running(b: &TextBlock) -> bool {
    !b.protected && b.lines.len() >= 2 && !b.keeps_lines()
}

/// The page as one attempt sees it.
struct Frame {
    /// A node for each item, then one for each body. `None` for what takes no part: a block that keeps its
    /// original, a body that stays where it is.
    nodes: Vec<Option<Node>>,
    /// For each item, the body it stands in and goes with.
    rides: Vec<Option<usize>>,
    /// For each obstacle, and for each block that stays as it is, the body it is part of and goes with.
    obstacle_of: Vec<Option<usize>>,
    block_of: HashMap<BlockId, usize>,
}

/// Whether `r` is part of the body in `outer`. A body is the box around ink as it was seen, to the pixel. What
/// the content says is there (a frame, a line of a label) is measured otherwise and may reach a little out of
/// that box. Nothing else on the page is this close to a body: it would have been seen as part of it.
pub(crate) fn within(outer: &Rect, r: &Rect) -> bool {
    const SLACK: f32 = 3.0;
    outer.x0 <= r.x0 + SLACK
        && outer.x1 >= r.x1 - SLACK
        && outer.y0 <= r.y0 + SLACK
        && outer.y1 >= r.y1 - SLACK
}

/// A body is not moved for less room than this under it.
const LEAST_ROOM: f32 = 3.0;

fn build(
    layout: &PageLayout,
    obstacles: &[Rect],
    bodies: &[Body],
    items: &[Item],
    rtl: bool,
    state: &State,
) -> Frame {
    let n = items.len();
    let live: HashMap<&BlockId, usize> = (0..n)
        .filter(|&i| !state.kept[i])
        .map(|i| (&items[i].block.id, i))
        .collect();
    // Text that stays where it is.
    let fixed: Vec<&TextBlock> = layout
        .blocks
        .iter()
        .filter(|b| !live.contains_key(&b.id))
        .collect();
    let running: Vec<Rect> = layout
        .blocks
        .iter()
        .filter(|b| is_running(b))
        .map(|b| b.rect)
        .collect();
    // Text that shows how wide its column is: two ragged lines do not.
    let measures: Vec<Rect> = layout
        .blocks
        .iter()
        .filter(|b| is_running(b) && (b.lines.len() >= 3 || b.align == Align::Justify))
        .map(|b| b.rect)
        .collect();
    let content_left = layout
        .blocks
        .iter()
        .map(|b| b.rect.x0)
        .fold(f32::MAX, f32::min);
    let content_right = layout
        .blocks
        .iter()
        .map(|b| b.rect.x1)
        .fold(f32::MIN, f32::max);
    // A page with little on it (a title, the last lines of a chapter) does not show how wide its text area is:
    // it is taken to have the margins of an ordinary page.
    let (content_left, content_right) = if content_right - content_left < layout.width * 0.5 {
        let margin = content_left
            .min(layout.width - content_right)
            .clamp(0.0, layout.width * 0.12);
        (
            content_left.min(margin),
            content_right.max(layout.width - margin),
        )
    } else {
        (content_left, content_right)
    };
    let content_top = layout
        .blocks
        .iter()
        .map(|b| b.rect.y0)
        .fold(f32::MAX, f32::min);
    let content_bottom = layout.blocks.iter().map(|b| b.rect.y1).fold(0.0, f32::max);
    // Where the text of the page begins, under its running head.
    let text_top = layout
        .blocks
        .iter()
        .filter(|b| b.rect.y1 >= layout.height * 0.08)
        .map(|b| b.rect.y0)
        .fold(f32::MAX, f32::min);
    // Where text may end: a little into the bottom margin of a full page, and on a page that ends early, as far
    // down as a full page would go.
    let margin = (layout.height - content_bottom).max(0.0);
    let page_bottom = {
        let into_margin = if state.deep {
            margin * 0.6
        } else {
            (margin * 0.35).min(28.0)
        };
        let full_page = layout.height - (content_top * 1.5).max(layout.height * 0.09);
        (content_bottom + into_margin).max(full_page)
    };
    // The bodies that may move: not held, not part of the head or foot of the page, clear of the text to be
    // placed (unless that text stands inside the body, and goes with it), and with room under them.
    let m = bodies.len();
    let area = |r: &Rect| r.width() * r.height();
    // A body inside the box of another is part of that one.
    let nested: Vec<Option<usize>> = (0..m)
        .map(|k| {
            (0..m).find(|&j| {
                j != k
                    && within(&bodies[j].rect, &bodies[k].rect)
                    && (area(&bodies[j].rect) > area(&bodies[k].rect) || j < k)
            })
        })
        .collect();
    let mut active: Vec<bool> = (0..m)
        .map(|k| {
            let r = &bodies[k].rect;
            !state.held[k]
                && nested[k].is_none()
                && r.y1 > layout.height * 0.08
                && r.y0 < layout.height * 0.9
                && live
                    .values()
                    .map(|&i| &items[i].block.rect)
                    .all(|t| !t.overlaps(r, 0.5) || within(r, t))
                // Nor is it cut out of something the content draws across it or around it: a tinted ground
                // too pale to be seen as ink, a frame it stands in.
                && obstacles
                    .iter()
                    .all(|o| !o.overlaps(r, 0.5) || within(r, o))
        })
        .collect();
    loop {
        let part_of = |r: &Rect| (0..m).find(|&k| active[k] && within(&bodies[k].rect, r));
        let mut room = vec![f32::MAX; m];
        for k in (0..m).filter(|&k| active[k]) {
            let r = &bodies[k].rect;
            let mut hard = page_bottom.max(r.y1);
            let under = |o: &Rect| is_below(r, o) && x_overlap((r.x0, r.x1), (o.x0, o.x1));
            for o in obstacles
                .iter()
                .filter(|o| part_of(o).is_none() && under(o))
            {
                hard = hard.min((o.y0 - 1.0).max(r.y1));
            }
            for f in fixed
                .iter()
                .filter(|f| part_of(&f.rect).is_none() && under(&f.rect))
            {
                hard = hard.min((f.rect.y0 - 0.5).max(r.y1));
            }
            for j in (0..m).filter(|&j| !active[j] && !nested[j].is_some_and(|o| active[o])) {
                if under(&bodies[j].rect) {
                    hard = hard.min((bodies[j].rect.y0 - 1.0).max(r.y1));
                }
            }
            room[k] = hard - r.y1;
        }
        // Bodies one under the other: the upper goes no further than the lower can.
        loop {
            let mut changed = false;
            for k in (0..m).filter(|&k| active[k]) {
                for j in (0..m).filter(|&j| j != k && active[j]) {
                    let (a, b) = (&bodies[k].rect, &bodies[j].rect);
                    if is_below(a, b)
                        && x_overlap((a.x0, a.x1), (b.x0, b.x1))
                        && room[j] < room[k] - 0.01
                    {
                        room[k] = room[j];
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let stuck: Vec<usize> = (0..m)
            .filter(|&k| active[k] && room[k] < LEAST_ROOM)
            .collect();
        if stuck.is_empty() {
            break;
        }
        for k in stuck {
            active[k] = false;
        }
    }
    let part_of = |r: &Rect| (0..m).find(|&k| active[k] && within(&bodies[k].rect, r));
    let obstacle_of: Vec<Option<usize>> = obstacles.iter().map(&part_of).collect();
    let block_of: HashMap<BlockId, usize> = fixed
        .iter()
        .filter_map(|f| part_of(&f.rect).map(|k| (f.id.clone(), k)))
        .collect();
    let rides: Vec<Option<usize>> = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            if state.kept[i] {
                None
            } else {
                part_of(&it.block.rect)
            }
        })
        .collect();
    // The usual line spacing of multi-line text of a size, for a single line that has to wrap.
    let usual_ratio = |size: f32| -> f32 {
        let mut ratios: Vec<f32> = layout
            .blocks
            .iter()
            .filter(|b| b.lines.len() >= 2 && (b.style.font_size - size).abs() <= 0.5)
            .map(|b| {
                (b.lines.last().unwrap().baseline - b.lines[0].baseline)
                    / (b.lines.len() - 1) as f32
                    / b.style.font_size
            })
            .filter(|r| (0.9..=2.5).contains(r))
            .collect();
        ratios.sort_by(f32::total_cmp);
        ratios.get(ratios.len() / 2).copied().unwrap_or(1.2)
    };
    let first_baseline = |b: &TextBlock| b.lines[0].baseline;
    let same_row = |a: &TextBlock, b: &TextBlock| {
        a.lines.len() <= 3
            && b.lines.len() <= 3
            && (first_baseline(a) - first_baseline(b)).abs() <= 1.0
            && (a.rect.x1 <= b.rect.x0 + 1.0 || b.rect.x1 <= a.rect.x0 + 1.0)
    };

    // Kind and alignment first: how far a block may grow sideways depends on which way its neighbours grow.
    let mut basics: Vec<Option<(Kind, Align)>> = vec![None; n];
    for (i, it) in items.iter().enumerate() {
        if state.kept[i] {
            continue;
        }
        let b = it.block;
        let r = &b.rect;
        let size = b.style.font_size;
        let kind = if b.lines.len() == 1 {
            Kind::Single
        } else if b.keeps_lines() {
            Kind::Entries
        } else {
            Kind::Running
        };
        let mut align = b.align;
        if it.companion {
            // A page number at the end of its row keeps its right edge.
            let row = || {
                layout
                    .blocks
                    .iter()
                    .filter(|o| o.id != b.id && same_row(o, b))
            };
            if row().any(|o| o.rect.x1 <= r.x0 + 1.0) && !row().any(|o| o.rect.x0 >= r.x1 - 1.0) {
                align = Align::Right;
            }
        } else if kind == Kind::Single {
            // One line says nothing about its alignment; its place in the column does.
            let column = running
                .iter()
                .filter(|c| c.x0 <= r.x0 + 0.5 && c.x1 >= r.x1 - 0.5)
                .fold(None, |u: Option<Rect>, c| {
                    Some(u.map_or(*c, |u| u.union(c)))
                });
            let (c0, c1) = column.map_or((0.0, layout.width), |c| (c.x0, c.x1));
            if ((r.x0 + r.x1) - (c0 + c1)).abs() / 2.0 < 1.5 && r.x0 - c0 > size * 1.5 {
                align = Align::Center;
            } else if column.is_some() && (r.x1 - c1).abs() < 1.0 && r.x0 - c0 > (c1 - c0) * 0.3 {
                align = Align::Right;
            }
        }
        if align == Align::Justify && kind != Kind::Running {
            align = Align::Left;
        }
        if rtl && matches!(align, Align::Left | Align::Justify) {
            align = Align::Right;
        }
        basics[i] = Some((kind, align));
    }

    let mut nodes: Vec<Option<Node>> = (0..n + m).map(|_| None).collect();
    for (i, it) in items.iter().enumerate() {
        let Some((kind, align)) = basics[i] else {
            continue;
        };
        let b = it.block;
        let r = b.rect;
        let ride = rides[i];
        let size0 = b.style.font_size;
        let gap = size0 * 0.5;

        // How far the lines may reach on each side.
        let (mut lim_l, mut lim_r) = (content_left, content_right);
        for o in obstacles.iter().filter(|o| encloses(o, &r)) {
            lim_r = lim_r.min(o.x1 - 2.0);
            lim_l = lim_l.max(o.x0 + 2.0);
        }
        let beside = |o: &Rect| o.y0 < r.y1 && o.y1 > r.y0;
        for o in obstacles.iter().filter(|o| beside(o)) {
            if o.x0 >= r.x1 - 0.5 {
                lim_r = lim_r.min(o.x0 - gap);
            } else if o.x1 <= r.x0 + 0.5 {
                lim_l = lim_l.max(o.x1 + gap);
            }
        }
        for o in layout
            .blocks
            .iter()
            .filter(|o| o.id != b.id && beside(&o.rect))
        {
            // A neighbour that grows towards this block shares the space between the two.
            let grows = |toward_left: bool| {
                live.get(&o.id)
                    .and_then(|&j| basics[j])
                    .is_some_and(|(k, a)| {
                        k != Kind::Running
                            && (a == Align::Center
                                || a == if toward_left {
                                    Align::Right
                                } else {
                                    Align::Left
                                })
                    })
            };
            if o.rect.x0 >= r.x1 - 0.5 {
                let mut lim = o.rect.x0 - gap;
                if grows(true) {
                    lim = lim.min((r.x1 + o.rect.x0 - gap) / 2.0);
                }
                lim_r = lim_r.min(lim);
            } else if o.rect.x1 <= r.x0 + 0.5 {
                let mut lim = o.rect.x1 + gap;
                if grows(false) {
                    lim = lim.max((r.x0 + o.rect.x1 + gap) / 2.0);
                }
                lim_l = lim_l.max(lim);
            }
        }
        let (mut lim_l, mut lim_r) = (lim_l.min(r.x0), lim_r.max(r.x1));
        if let Some(k) = ride {
            // Text inside a body stays inside it.
            lim_l = lim_l.max(bodies[k].rect.x0.min(r.x0));
            lim_r = lim_r.min(bodies[k].rect.x1.max(r.x1));
        }
        if state.restrict[i] >= 1 {
            (lim_l, lim_r) = (r.x0, r.x1);
        } else if kind == Kind::Running {
            // A paragraph keeps its measure. A ragged one may use the measure of the column it stands in.
            let column = running
                .iter()
                .filter(|c| {
                    c.x0 <= r.x0 + size0 * 3.0
                        && c.x1 >= r.x1
                        && c.x1 - r.x1 <= c.width() * 0.15
                        && c.x0 < r.x1
                })
                .map(|c| c.x1)
                .fold(r.x1, f32::max);
            lim_r = if align == Align::Left {
                lim_r.min(column)
            } else {
                r.x1
            };
            lim_l = r.x0;
        } else if align == Align::Left && !it.companion {
            // A heading inside a column of text does not run past the column.
            let column = measures
                .iter()
                .filter(|c| c.x0 <= r.x0 + size0 * 3.0 && c.x1 >= r.x1 - 0.5 && c.x0 < r.x1)
                .map(|c| c.x1)
                .fold(None, |m: Option<f32>, x| Some(m.map_or(x, |m| m.max(x))));
            if let Some(column) = column {
                lim_r = lim_r.min(column).max(r.x1);
            }
        }
        let (left, right) = match align {
            Align::Left | Align::Justify => (r.x0, lim_r),
            Align::Right => (lim_l, r.x1),
            Align::Center => {
                let grow = (r.x0 - lim_l).min(lim_r - r.x1).max(0.0);
                (r.x0 - grow, r.x1 + grow)
            }
        };
        let span = (left.min(r.x0), right.max(r.x1));

        // The floor: the first thing below that does not move.
        let mut floor = page_bottom.max(r.y1);
        let mut page_floor = true;
        let mut lower = |y: f32, floor: &mut f32| {
            if y.max(r.y1) < *floor {
                *floor = y.max(r.y1);
                page_floor = false;
            }
        };
        // What is part of a body that moves is no floor to the text outside it: the body stands in the chain
        // of what is pushed down. To the text inside the body, only what is in there with it counts.
        for o in obstacles
            .iter()
            .enumerate()
            .filter(|(k, _)| obstacle_of[*k] == ride)
            .map(|(_, o)| o)
        {
            if encloses(o, &r) {
                lower(o.y1 - 2.0, &mut floor);
            } else if is_below(&r, o) && x_overlap(span, (o.x0, o.x1)) {
                lower(o.y0 - 1.0, &mut floor);
            }
        }
        for f in fixed
            .iter()
            .filter(|f| block_of.get(&f.id).copied() == ride)
        {
            if is_below(&r, &f.rect) && x_overlap(span, (f.rect.x0, f.rect.x1)) {
                lower(f.rect.y0 - 0.5, &mut floor);
            }
        }
        if let Some(k) = ride {
            lower(bodies[k].rect.y1, &mut floor);
        }
        // The ceiling: the same from above. Of the space over the block a line and a half is kept at most, as
        // it is between blocks. With nothing over it the block is the first of its column, and stays.
        let over = |o: &Rect| {
            o.y1 <= r.y0 + r.height() * 0.5 && o.y0 < r.y0 && x_overlap(span, (o.x0, o.x1))
        };
        let under = |y1: f32| y1 + (r.y0 - y1).clamp(0.0, size0 * 1.5);
        let mut ceiling = f32::MIN;
        for o in obstacles
            .iter()
            .enumerate()
            .filter(|(k, _)| obstacle_of[*k] == ride)
            .map(|(_, o)| o)
        {
            if encloses(o, &r) {
                ceiling = ceiling.max(o.y0 + 2.0);
            } else if over(o) {
                ceiling = ceiling.max(under(o.y1));
            }
        }
        for f in fixed
            .iter()
            .filter(|f| block_of.get(&f.id).copied() == ride && over(&f.rect))
        {
            ceiling = ceiling.max(under(f.rect.y1));
        }
        // A body that stays is a ceiling too. One that moves stands in the chain, and the text under it
        // keeps its distance to it.
        for j in (0..m).filter(|&j| !active[j] && !nested[j].is_some_and(|o| active[o])) {
            if over(&bodies[j].rect) {
                ceiling = ceiling.max(under(bodies[j].rect.y1));
            }
        }
        if let Some(k) = ride {
            ceiling = ceiling.max(bodies[k].rect.y0);
        }
        // Running heads and feet stay where they are on every page.
        let mut anchored =
            b.lines.len() == 1 && (r.y0 > layout.height * 0.9 || r.y1 < layout.height * 0.08);
        anchored |= fixed
            .iter()
            .any(|f| block_of.get(&f.id).copied() == ride && same_row(f, b));
        // A block that has to stay in its place does not go lower than its original did. It may still take the
        // empty room beside it, as any heading may.
        if state.restrict[i] >= 2 || it.confined {
            anchored = true;
            lower(r.y1 + size0 * 0.25, &mut floor);
        }

        // What the text runs around: something that reaches into the block from a side, and that none of the
        // original lines ran over (text on a tinted background does, and may again).
        let mut around: Vec<Rect> = Vec::new();
        let mut beside_picture = false;
        if kind == Kind::Running && matches!(align, Align::Left | Align::Justify) {
            let width = right - left;
            let ran_over = |o: &Rect| {
                b.lines
                    .iter()
                    .any(|l| core(l.rect.x0, l.rect.x1, l.baseline, size0).overlaps(o, 1.0))
            };
            let from_a_side = |o: &Rect| {
                (o.x0 <= left + width * 0.15 && o.x1 < left + width * 0.6)
                    || (o.x1 >= right - width * 0.15 && o.x0 > left + width * 0.4)
            };
            let drawings: Vec<&Rect> = obstacles
                .iter()
                .filter(|o| !encloses(o, &r) && o.width() > 3.0 && o.height() > 3.0)
                .collect();
            let texts = fixed.iter().map(|f| &f.rect);
            for o in drawings.iter().copied().chain(texts) {
                if o.overlaps(&r, 0.5) && from_a_side(o) && !ran_over(o) {
                    around.push(*o);
                }
            }
            beside_picture = !around.is_empty();
            // And what stands past the end of a line: a small picture set in the text after the last words
            // before it, or the text that goes on behind that picture. The line ends before it again.
            let past_the_end = |o: &Rect| {
                let mut level = b
                    .lines
                    .iter()
                    .filter(|l| {
                        let c = core(l.rect.x0, l.rect.x1, l.baseline, size0);
                        c.y0 < o.y1 - 1.0 && c.y1 > o.y0 + 1.0
                    })
                    .peekable();
                level.peek().is_some() && level.all(|l| l.rect.x1 <= o.x0 + 0.5)
            };
            let others = layout
                .blocks
                .iter()
                .filter(|o| o.id != b.id)
                .map(|o| &o.rect);
            for o in drawings.iter().copied().chain(others) {
                let known = around.iter().any(|a| a.x0 == o.x0 && a.y0 == o.y0);
                if o.overlaps(&r, 0.5) && !known && past_the_end(o) {
                    // To the edge of the block, so that it counts as standing on the right.
                    around.push(Rect {
                        x1: o.x1.max(right),
                        ..*o
                    });
                }
            }
            // The lines are worked out for where the block stands now.
            anchored |= !around.is_empty();
        }
        // An initial moves with its paragraph.
        around.extend(it.initial);

        let ratio = if b.lines.len() > 1 {
            let leading = (b.lines.last().unwrap().baseline - b.lines[0].baseline)
                / (b.lines.len() - 1) as f32;
            Some(leading / size0).filter(|r| (0.9..=2.5).contains(r))
        } else {
            None
        }
        .unwrap_or_else(|| usual_ratio(size0));
        nodes[i] = Some(Node {
            r,
            size0,
            ratio,
            base_off: (b.lines[0].baseline - r.y0).clamp(0.0, size0 * 1.5),
            desc_off: (r.y1 - b.lines.last().unwrap().baseline).clamp(0.0, size0),
            // Lines set around something began where that left room, which says nothing about indents.
            indent: if rtl || beside_picture || it.initial.is_some() {
                Indent::default()
            } else {
                indent_of(b)
            },
            align,
            kind,
            left,
            right,
            floor,
            page_floor,
            ceiling,
            follows: false,
            anchored,
            above: Vec::new(),
            row: i,
            fixed: None,
            around,
        });
    }

    // The bodies that may move stand among the text: they are pushed down and push in turn, and never change.
    for k in (0..m).filter(|&k| active[k]) {
        let r = bodies[k].rect;
        let mut floor = page_bottom.max(r.y1);
        let mut page_floor = true;
        let mut lower = |y: f32| {
            if y.max(r.y1) < floor {
                floor = y.max(r.y1);
                page_floor = false;
            }
        };
        let under = |o: &Rect| is_below(&r, o) && x_overlap((r.x0, r.x1), (o.x0, o.x1));
        for (_, o) in obstacles
            .iter()
            .enumerate()
            .filter(|(j, o)| obstacle_of[*j].is_none() && under(o))
        {
            lower(o.y0 - 1.0);
        }
        for f in fixed
            .iter()
            .filter(|f| !block_of.contains_key(&f.id) && under(&f.rect))
        {
            lower(f.rect.y0 - 0.5);
        }
        for j in (0..m).filter(|&j| !active[j] && !nested[j].is_some_and(|o| active[o])) {
            if under(&bodies[j].rect) {
                lower(bodies[j].rect.y0 - 1.0);
            }
        }
        // How high it may go after text that came out shorter: not past what stays above it, and of the space
        // to that a line and a half is kept at most.
        let over = |o: &Rect| {
            o.y1 <= r.y0 + r.height() * 0.5 && o.y0 < r.y0 && x_overlap((r.x0, r.x1), (o.x0, o.x1))
        };
        let below = |y1: f32| y1 + (r.y0 - y1).clamp(0.0, 18.0);
        let mut ceiling = f32::MIN;
        for (_, o) in obstacles
            .iter()
            .enumerate()
            .filter(|(j, o)| obstacle_of[*j].is_none() && over(o))
        {
            ceiling = ceiling.max(below(o.y1));
        }
        for f in fixed
            .iter()
            .filter(|f| !block_of.contains_key(&f.id) && over(&f.rect))
        {
            ceiling = ceiling.max(below(f.rect.y1));
        }
        for j in (0..m).filter(|&j| !active[j] && !nested[j].is_some_and(|o| active[o])) {
            if over(&bodies[j].rect) {
                ceiling = ceiling.max(below(bodies[j].rect.y1));
            }
        }
        nodes[n + k] = Some(Node {
            r,
            size0: 0.0,
            ratio: 1.0,
            base_off: 0.0,
            desc_off: 0.0,
            indent: Indent::default(),
            align: Align::Left,
            kind: Kind::Body,
            left: r.x0,
            right: r.x1,
            floor,
            page_floor,
            ceiling,
            follows: false,
            anchored: false,
            above: Vec::new(),
            row: n + k,
            fixed: Some(Shape {
                size: 0.0,
                squeeze: 1.0,
                leading: 0.0,
                lines: Vec::new(),
                height: r.height(),
                own: true,
                gap: 1.0,
                cramped: false,
            }),
            around: Vec::new(),
        });
    }

    // Who pushes whom: the blocks directly above each block, in its span. Text inside a body is pushed only
    // by the text in there with it.
    let ids: Vec<usize> = (0..n + m).filter(|&i| nodes[i].is_some()).collect();
    let node = |i: usize| nodes[i].as_ref().unwrap();
    let group = |i: usize| if i < n { rides[i] } else { None };
    let under = |a: usize, b: usize| {
        group(a) == group(b)
            && is_below(&node(a).r, &node(b).r)
            && x_overlap(node(a).span(), node(b).span())
    };
    let mut above: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n + m];
    for &j in &ids {
        for &i in &ids {
            if i == j || !under(i, j) {
                continue;
            }
            let between = ids
                .iter()
                .any(|&m| m != i && m != j && under(i, m) && under(m, j));
            if !between {
                let gap = node(j).r.y0 - node(i).r.y1;
                let gap = if state.apart.contains(&(i, j)) {
                    gap.max(0.0)
                } else {
                    gap
                };
                above[j].push((i, gap));
            }
        }
    }
    // Rows: short pieces of text on one baseline (the cells of a table row, a bullet and its text) move together.
    let mut row: Vec<usize> = (0..n + m).collect();
    for (k, &i) in ids.iter().enumerate() {
        for &j in &ids[k + 1..] {
            if i < n && j < n && rides[i] == rides[j] && same_row(items[i].block, items[j].block) {
                let (a, b) = (row[i], row[j]);
                if a != b {
                    for r in row.iter_mut().filter(|r| **r == b) {
                        *r = a;
                    }
                }
            }
        }
    }
    let follows: Vec<bool> = (0..n + m)
        .map(|i| {
            let (Some(nd), [(a, gap)]) = (nodes[i].as_ref(), above[i].as_slice()) else {
                return false;
            };
            let over = node(*a);
            if nd.anchored || (i < n && rides[i].is_some()) {
                return false;
            }
            match (over.kind, nd.kind) {
                // A paragraph after a paragraph, with less than a blank line between them.
                (Kind::Running, Kind::Running) => {
                    (-0.5..=nd.size0 * 0.75).contains(gap)
                        && (nd.r.x0 - over.r.x0).abs() <= nd.size0 * 3.0
                }
                // A listing or a figure set into the text, no further from it than a line; and the text that
                // goes on after it, or the line that names it.
                (_, Kind::Body) => (0.0..=over.size0.max(10.0)).contains(gap),
                (Kind::Body, _) => (0.0..=nd.size0).contains(gap),
                _ => false,
            }
        })
        .collect();
    for i in ids {
        if i < n {
            let fixed = fixed_shape(&items[i], nodes[i].as_ref().unwrap());
            nodes[i].as_mut().unwrap().fixed = fixed;
        }
        nodes[i].as_mut().unwrap().follows = follows[i];
        let nd = nodes[i].as_mut().unwrap();
        nd.above = std::mem::take(&mut above[i]);
        nd.row = row[i];
        if nd.above.is_empty() && nd.ceiling == f32::MIN {
            nd.ceiling = nd.r.y0;
        }
        // Never into the margin over the text of the page.
        nd.ceiling = nd.ceiling.max(text_top).min(nd.r.y0);
    }
    Frame {
        nodes,
        rides,
        obstacle_of,
        block_of,
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The entries of a list or table of contents, if the translation kept one per original line.
fn entries<'t>(item: &'t Item, node: &Node) -> Option<Vec<&'t str>> {
    let e: Vec<&str> = item
        .text
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .collect();
    (node.kind == Kind::Entries && e.len() == item.block.lines.len()).then_some(e)
}

/// The indent of each entry: where its original line began.
fn entry_indent(item: &Item, node: &Node, k: usize) -> f32 {
    if node.align == Align::Left {
        (item.block.lines[k].rect.x0 - node.r.x0).max(0.0)
    } else {
        0.0
    }
}

fn smallest(size0: f32) -> f32 {
    (size0 * MIN_SCALE).max(MIN_SIZE).min(size0)
}

/// A single line that still fits on one line, or entries that still fit on one line each: they keep the
/// baselines of the original and so need no room of their own.
fn fixed_shape(item: &Item, node: &Node) -> Option<Shape> {
    let m = item.measure;
    let width = node.width();
    let shape = |lines: Vec<(String, f32, f32)>, squeeze: f32, size: f32| Shape {
        size,
        squeeze,
        leading: node.size0 * node.ratio,
        lines,
        height: node.r.height(),
        own: true,
        gap: 1.0,
        cramped: false,
    };
    match node.kind {
        Kind::Single => {
            let text = one_line(item.text);
            let tries = if item.companion {
                &ONE_LINE[..4]
            } else {
                ONE_LINE
            };
            tries.iter().find_map(|&(squeeze, scale)| {
                let size = (node.size0 * scale).max(smallest(node.size0));
                (m.width(&text, size) * squeeze <= width + 0.05)
                    .then(|| shape(vec![(text.clone(), 0.0, 0.0)], squeeze, size))
            })
        }
        Kind::Entries => {
            let e = entries(item, node)?;
            PER_ENTRY.iter().find_map(|&(squeeze, scale)| {
                let size = (node.size0 * scale).max(smallest(node.size0));
                let lines: Vec<(String, f32, f32)> = e
                    .iter()
                    .enumerate()
                    .map(|(k, t)| (one_line(t), entry_indent(item, node, k), 0.0))
                    .collect();
                lines
                    .iter()
                    .all(|(t, indent, _)| m.width(t, size) * squeeze <= width - indent + 0.05)
                    .then(|| shape(lines, squeeze, size))
            })
        }
        Kind::Running | Kind::Body => None,
    }
}

/// The lines and height of a block at one rung of the ladder.
fn shape_at(i: usize, item: &Item, node: &Node, level: usize, cache: &mut ShapeCache) -> Shape {
    if let Some(s) = &node.fixed {
        return s.clone();
    }
    let width = node.width();
    let key = (i, level, width.to_bits());
    if let Some(s) = cache.get(&key) {
        return s.clone();
    }
    let step = &LADDER[level];
    let size = (node.size0 * step.scale).max(smallest(node.size0));
    let m = Squeezed {
        inner: item.measure,
        by: step.squeeze,
    };
    let ratio = (node.ratio * step.lead).max(node.ratio.min(1.08));
    let leading = size * ratio;
    let lines: Vec<(String, f32, f32)> = if let Some(e) = entries(item, node) {
        // An entry too long for its line continues under itself, a little further in.
        let mut lines = Vec::new();
        for (k, entry) in e.iter().enumerate() {
            let own = entry_indent(item, node, k);
            let indent = Indent {
                first: own,
                rest: own + node.size0,
            };
            for (n, l) in fit::wrap_indented(entry, width, indent, size, &m)
                .into_iter()
                .enumerate()
            {
                lines.push((l, indent.of_line(n), 0.0));
            }
        }
        lines
    } else {
        let indent =
            if node.kind == Kind::Single || !matches!(node.align, Align::Left | Align::Justify) {
                Indent::default()
            } else {
                node.indent
            };
        let clear = |n: usize| node.insets(n, size, leading);
        let width_at = |n: usize| {
            let (l, r) = clear(n);
            width - indent.of_line(n) - l - r
        };
        fit::wrap_widths(item.text, &width_at, size, &m)
            .into_iter()
            .enumerate()
            .map(|(n, l)| (l, indent.of_line(n) + clear(n).0, clear(n).1))
            .collect()
    };
    // A word wider than a line is broken in two. That reads worse than a smaller size, as long as some rung
    // further down gives it a line of its own: a long address may not fit at any size, and is not worth the
    // smaller type.
    let room = width - node.indent.first.max(node.indent.rest);
    let last = &LADDER[LADDER.len() - 1];
    let narrowest = Squeezed {
        inner: item.measure,
        by: last.squeeze,
    };
    let cramped = level + 1 < LADDER.len()
        && item.text.split_whitespace().any(|word| {
            m.width(word, size) > room + 0.05
                && narrowest.width(word, smallest(node.size0)) <= room + 0.05
        });
    let scale = size / node.size0;
    let shape = Shape {
        height: (node.base_off + node.desc_off) * scale
            + leading * lines.len().saturating_sub(1) as f32,
        size,
        squeeze: step.squeeze,
        leading,
        lines,
        own: false,
        gap: step.gap,
        cramped,
    };
    cache.insert(key, shape.clone());
    shape
}

/// The top of every block once the blocks above have taken their room, and for each block that moved, the block
/// that moved it.
///
/// A block starts out from where its original stood, or up to `lift` higher where it has been allowed to go up,
/// and never over its ceiling.
fn push_down(
    nodes: &[Option<Node>],
    shapes: &[Option<Shape>],
    lift: &[f32],
) -> (Vec<f32>, Vec<Option<usize>>) {
    let n = nodes.len();
    let mut order: Vec<usize> = (0..n).filter(|&i| nodes[i].is_some()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (nodes[a].as_ref().unwrap(), nodes[b].as_ref().unwrap());
        a.r.y0.total_cmp(&b.r.y0)
    });
    let mut top: Vec<f32> = nodes
        .iter()
        .zip(lift)
        .map(|(nd, lift)| {
            nd.as_ref()
                .map_or(0.0, |nd| (nd.r.y0 - lift).max(nd.ceiling.min(nd.r.y0)))
        })
        .collect();
    let mut pusher: Vec<Option<usize>> = vec![None; n];
    for _ in 0..24 {
        let mut changed = false;
        for &i in &order {
            let nd = nodes[i].as_ref().unwrap();
            let shape = shapes[i].as_ref().unwrap();
            for &(a, gap) in &nd.above {
                let bottom = top[a] + shapes[a].as_ref().unwrap().height;
                // Ordinary paragraph spacing is kept (less of it on the lower rungs). Wide white space, above a
                // heading for one, is room to grow into.
                // Bodies under one another keep their distance. Above a body, as above text, no more is kept
                // than a line and a half of the text that pushes.
                let pushes = nodes[a].as_ref().unwrap();
                let keep = if gap <= 0.0 || (nd.kind == Kind::Body && pushes.kind == Kind::Body) {
                    gap
                } else if nd.kind == Kind::Body {
                    gap.min(pushes.size0 * 1.5)
                } else {
                    gap.min(nd.size0 * 1.5) * shape.gap
                };
                if bottom + keep > top[i] + 0.01 {
                    top[i] = bottom + keep;
                    pusher[i] = Some(a);
                    changed = true;
                }
            }
        }
        for &i in &order {
            let row = nodes[i].as_ref().unwrap().row;
            let lead = order
                .iter()
                .copied()
                .filter(|&j| nodes[j].as_ref().unwrap().row == row)
                .max_by(|&a, &b| {
                    let shift = |k: usize| top[k] - nodes[k].as_ref().unwrap().r.y0;
                    shift(a).total_cmp(&shift(b))
                })
                .unwrap();
            let shift = top[lead] - nodes[lead].as_ref().unwrap().r.y0;
            let mine = nodes[i].as_ref().unwrap().r.y0 + shift;
            if lead != i && mine > top[i] + 0.01 {
                top[i] = mine;
                pusher[i] = Some(lead);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (top, pusher)
}

/// The shape of a node: a body has the one it always has, text the one of its rung.
fn shape_of(i: usize, items: &[Item], node: &Node, level: usize, cache: &mut ShapeCache) -> Shape {
    match &node.fixed {
        Some(s) => s.clone(),
        None => shape_at(i, &items[i], node, level, cache),
    }
}

fn attempt(
    layout: &PageLayout,
    obstacles: &[Rect],
    bodies: &[Body],
    items: &[Item],
    rtl: bool,
    state: &State,
    cache: &mut ShapeCache,
) -> Result<Solved, Change> {
    let (n, m) = (items.len(), bodies.len());
    let frame = build(layout, obstacles, bodies, items, rtl, state);
    let nodes = &frame.nodes;
    let node = |i: usize| nodes[i].as_ref().unwrap();
    let ids: Vec<usize> = (0..n + m).filter(|&i| nodes[i].is_some()).collect();
    // A companion that cannot be drawn on its line stays as it is, and holds its row in place.
    if let Some(&i) = ids
        .iter()
        .find(|&&i| i < n && items[i].companion && node(i).fixed.is_none())
    {
        return Err(Change::Keep(i));
    }

    let mut level = vec![0usize; n + m];
    // How far each block may go up from its place, and how often that was tried. A paragraph that follows
    // another goes wherever that one ends.
    let mut lift: Vec<f32> = nodes
        .iter()
        .map(|nd| match nd {
            Some(nd) if nd.follows => f32::INFINITY,
            _ => 0.0,
        })
        .collect();
    let mut lifts = 0usize;
    let mut evened = false;
    let (shapes, top) = loop {
        let shapes: Vec<Option<Shape>> = (0..n + m)
            .map(|i| {
                nodes[i]
                    .as_ref()
                    .map(|nd| shape_of(i, items, nd, level[i], cache))
            })
            .collect();
        if let Some(i) = (0..n)
            .find(|&i| shapes[i].as_ref().is_some_and(|s| s.cramped) && level[i] + 1 < LADDER.len())
        {
            level[i] += 1;
            continue;
        }
        let (top, pusher) = push_down(nodes, &shapes, &lift);
        let height = |i: usize| shapes[i].as_ref().unwrap().height;
        // The first block, from the top, that ends below its floor or left a place it is tied to.
        let mut order = ids.clone();
        order.sort_by(|&a, &b| top[a].total_cmp(&top[b]));
        let violation = order.iter().copied().find_map(|i| {
            let nd = node(i);
            if nd.anchored && top[i] > nd.r.y0 + 0.3 {
                Some((i, false))
            } else if top[i] + height(i) > nd.floor + 0.3 {
                Some((i, true))
            } else {
                None
            }
        });
        let Some((v, bottom)) = violation else {
            if !evened {
                evened = true;
                if even_out(nodes, items, &mut level) {
                    continue;
                }
            }
            break (shapes, top);
        };
        // The blocks that brought it there: itself and whatever pushed it, up the chain.
        let mut chain = Vec::new();
        let mut next = if bottom { Some(v) } else { pusher[v] };
        while let Some(i) = next {
            if chain.contains(&i) {
                break;
            }
            chain.push(i);
            next = pusher[i];
        }
        // Room first. Where the text above came out shorter than its original, it left room: the blocks that
        // are short of room go up into it, as far as they are short, before any of them is made smaller.
        let short = |top: &[f32]| {
            if bottom {
                top[v] + height(v) - node(v).floor
            } else {
                top[v] - node(v).r.y0
            }
        };
        let rises = |i: usize| i >= n || (!node(i).anchored && frame.rides[i].is_none());
        if lifts < ids.len() * 4 && chain.iter().any(|&i| rises(i)) {
            let mut higher = lift.clone();
            for &i in chain.iter().filter(|&&i| rises(i)) {
                higher[i] += short(&top);
            }
            let (raised, _) = push_down(nodes, &shapes, &higher);
            if short(&raised) < short(&top) - 0.05 {
                lift = higher;
                lifts += 1;
                continue;
            }
        }
        // Take the block that has given the least so far one rung down, to the next rung that gains room.
        let mut best: Option<(usize, usize)> = None;
        for &i in &chain {
            if node(i).fixed.is_some() {
                continue;
            }
            let now = height(i);
            let Some(to) = (level[i] + 1..LADDER.len())
                .find(|&l| shape_at(i, &items[i], node(i), l, cache).height < now - 0.05)
            else {
                continue;
            };
            let lines = |k: usize| shapes[k].as_ref().unwrap().lines.len();
            if best.is_none_or(|(b, _)| (level[i], lines(b)) < (level[b], lines(i))) {
                best = Some((i, to));
            }
        }
        match best {
            Some((i, to)) => level[i] = to,
            None if bottom && node(v).page_floor && !state.deep => return Err(Change::Deep),
            None => {
                // Nothing left to give: the block that grew the most keeps its original.
                let grown = |i: usize| height(i) - node(i).r.height();
                let victim = chain
                    .iter()
                    .copied()
                    .filter(|&i| i < n)
                    .max_by(|&a, &b| grown(a).total_cmp(&grown(b)));
                return Err(match victim {
                    Some(i) => Change::Keep(i),
                    None if v >= n => Change::Hold(v - n),
                    None => Change::Keep(v),
                });
            }
        }
    };

    // Where every line ends up. Text inside a body goes down with it.
    let moved: Vec<f32> = (0..m)
        .map(|k| nodes[n + k].as_ref().map_or(0.0, |nd| top[n + k] - nd.r.y0))
        .collect();
    let mut placed: Vec<Option<Placed>> = vec![None; n];
    for &i in ids.iter().filter(|&&i| i < n) {
        let (nd, shape) = (node(i), shapes[i].as_ref().unwrap());
        let b = items[i].block;
        let top = top[i] + frame.rides[i].map_or(0.0, |k| moved[k]);
        let shift = top - nd.r.y0;
        let width = nd.width();
        let last = shape.lines.len().saturating_sub(1);
        let lines = shape
            .lines
            .iter()
            .enumerate()
            .map(|(k, (text, indent, clear))| {
                let w = items[i].measure.width(text, shape.size) * shape.squeeze;
                let baseline = if shape.own {
                    b.lines[k.min(b.lines.len() - 1)].baseline + shift
                } else {
                    top + nd.base_off * shape.size / nd.size0 + shape.leading * k as f32
                };
                let x = match nd.align {
                    Align::Left | Align::Justify => nd.left + indent,
                    Align::Center => (nd.left + nd.right - w) / 2.0,
                    Align::Right => nd.right - w,
                };
                Line {
                    text: text.clone(),
                    x,
                    baseline,
                    width: w,
                    fill: (nd.align == Align::Justify && k < last)
                        .then_some(width - indent - clear),
                }
            })
            .collect();
        placed[i] = Some(Placed {
            size: shape.size,
            squeeze: shape.squeeze,
            lines,
            shift,
        });
    }
    match conflict(
        layout, obstacles, bodies, items, &placed, &moved, &frame, state,
    ) {
        Some(change) => Err(change),
        None => Ok(Solved {
            carried: (0..n)
                .map(|i| frame.rides[i].map_or(0.0, |k| moved[k]))
                .collect(),
            placed,
            moved,
        }),
    }
}

/// Bring the paragraphs of one size to the same rung, so a page does not mix sizes that were one size. A few
/// paragraphs that had to go far down the ladder do not drag the others along. Returns whether anything changed.
fn even_out(nodes: &[Option<Node>], items: &[Item], level: &mut [usize]) -> bool {
    // The rung where sizes fall under 85%.
    let far = LADDER
        .iter()
        .position(|s| s.scale < 0.85)
        .unwrap_or(LADDER.len());
    let mut classes: std::collections::BTreeMap<(u32, bool, bool), Vec<usize>> = Default::default();
    for (i, nd) in nodes.iter().enumerate() {
        let Some(nd) = nd else { continue };
        if nd.kind == Kind::Running && nd.fixed.is_none() {
            let s = &items[i].block.style;
            classes
                .entry(((nd.size0 * 4.0).round() as u32, s.bold, s.italic))
                .or_default()
                .push(i);
        }
    }
    let mut changed = false;
    for members in classes.values() {
        let weight = |i: usize| items[i].block.lines.len();
        let total: usize = members.iter().map(|&i| weight(i)).sum();
        let deep: usize = members
            .iter()
            .filter(|&&i| level[i] >= far)
            .map(|&i| weight(i))
            .sum();
        let target = members
            .iter()
            .map(|&i| level[i])
            .filter(|&l| l < far || deep * 4 >= total)
            .max()
            .unwrap_or(0);
        for &i in members {
            if level[i] < target {
                level[i] = target;
                changed = true;
            }
        }
    }
    changed
}

/// The part of a line of text that must stay clear: from above the x-height to just under the baseline.
fn core(x0: f32, x1: f32, baseline: f32, size: f32) -> Rect {
    Rect {
        x0,
        y0: baseline - size * 0.7,
        x1,
        y1: baseline + size * 0.18,
    }
}

/// Check the placed lines against each other, against the text that stays and against the drawings. Returns what
/// to do about the first clash, if there is one.
#[allow(clippy::too_many_arguments)]
fn conflict(
    layout: &PageLayout,
    obstacles: &[Rect],
    bodies: &[Body],
    items: &[Item],
    placed: &[Option<Placed>],
    moved: &[f32],
    frame: &Frame,
    state: &State,
) -> Option<Change> {
    let n = items.len();
    let boxes: Vec<Vec<Rect>> = placed
        .iter()
        .map(|p| {
            p.iter()
                .flat_map(|p| {
                    p.lines
                        .iter()
                        .map(|l| core(l.x, l.x + l.width, l.baseline, p.size))
                })
                .collect()
        })
        .collect();
    // How far a block reaches out of the place of its original.
    let outside = |i: usize| -> f32 {
        let r = &items[i].block.rect;
        boxes[i]
            .iter()
            .map(|b| (b.x1 - r.x1).max(0.0) + (r.x0 - b.x0).max(0.0) + (b.y1 - r.y1).max(0.0))
            .fold(0.0, f32::max)
    };
    let confine = |i: usize| {
        if state.restrict[i] >= 2 {
            Change::Keep(i)
        } else {
            Change::Restrict(i)
        }
    };
    let clash = |a: &[Rect], b: &[Rect]| a.iter().any(|x| b.iter().any(|y| x.overlaps(y, 0.6)));
    let live: Vec<usize> = (0..n).filter(|&i| placed[i].is_some()).collect();
    let is_live = |id: &BlockId| live.iter().any(|&i| &items[i].block.id == id);
    let lower = |r: &Rect, by: f32| Rect {
        y0: r.y0 + by,
        y1: r.y1 + by,
        ..*r
    };
    // What is part of a body is where the body went.
    let carried = |part: Option<usize>| part.map_or(0.0, |b| moved[b]);
    let lines_of = |f: &TextBlock| -> Vec<Rect> {
        let by = carried(frame.block_of.get(&f.id).copied());
        f.lines
            .iter()
            .map(|l| core(l.rect.x0, l.rect.x1, l.baseline + by, f.style.font_size))
            .collect()
    };
    // Where the lines of a block stood. Two blocks whose boxes overlap need not have had lines over one another
    // (a list entry that begins on the last line of the one before, a bubble that a drawn line cut in two), and
    // their translations must not run over one another either. Only lines that stood over one another already
    // may do so again.
    let stood = |b: &TextBlock| -> Vec<Rect> {
        b.lines
            .iter()
            .map(|l| core(l.rect.x0, l.rect.x1, l.baseline, b.style.font_size))
            .collect()
    };
    let crossed =
        |a: &TextBlock, b: &TextBlock| a.rect.overlaps(&b.rect, 0.5) && clash(&stood(a), &stood(b));
    for &i in &live {
        let r = &items[i].block.rect;
        let ride = frame.rides[i];
        // A companion that did not move is still the original text.
        let still = items[i].companion && placed[i].as_ref().unwrap().shift.abs() < 0.3;
        // Text that a body ran into when it moved is left alone: the body stays where it was instead. So it
        // does when the text it took along ran into something that stayed behind.
        let settle = |part: Option<usize>| match (part, ride) {
            (Some(b), _) | (_, Some(b)) if part != ride && moved[b].abs() > 0.3 => Change::Hold(b),
            _ => confine(i),
        };
        for (k, o) in obstacles.iter().enumerate() {
            let part = frame.obstacle_of[k];
            let now = lower(o, carried(part));
            if encloses(o, r) {
                let out = boxes[i]
                    .iter()
                    .any(|b| b.x1 > now.x1 + 0.5 || b.x0 < now.x0 - 0.5 || b.y1 > now.y1 + 0.5);
                if out && !still {
                    return Some(settle(part));
                }
            } else if !o.overlaps(r, 0.5)
                && boxes[i].iter().any(|b| b.overlaps(&now, 0.6))
                && !still
            {
                return Some(settle(part));
            }
        }
        for f in layout.blocks.iter().filter(|f| !is_live(&f.id)) {
            // Something that stays inside the box of the block (a sign or a small picture in a line of it) is
            // run around where it can be, and written over where it cannot: a paragraph is not left untranslated
            // for it.
            if still || crossed(f, items[i].block) || encloses(r, &f.rect) {
                continue;
            }
            if clash(&boxes[i], &lines_of(f)) {
                return Some(settle(frame.block_of.get(&f.id).copied()));
            }
        }
        for &j in live.iter().filter(|&&j| j > i) {
            if crossed(items[j].block, items[i].block) || !clash(&boxes[i], &boxes[j]) {
                continue;
            }
            // A block that began on the last line of the one before it (a paragraph cut in two by a small
            // picture in its line, an entry that follows on the line of the last) begins under it instead.
            let (up, down) = if items[i].block.rect.y0 <= items[j].block.rect.y0 {
                (i, j)
            } else {
                (j, i)
            };
            let (ru, rd) = (&items[up].block.rect, &items[down].block.rect);
            if rd.y0 < ru.y1 - 0.5 && is_below(ru, rd) && !state.apart.contains(&(up, down)) {
                return Some(Change::Apart(up, down));
            }
            // Confine the one that strayed further, unless it is confined already.
            let (far, near) = if outside(i) >= outside(j) {
                (i, j)
            } else {
                (j, i)
            };
            return Some(if state.restrict[far] < 2 {
                Change::Restrict(far)
            } else if state.restrict[near] < 2 {
                Change::Restrict(near)
            } else {
                Change::Keep(far)
            });
        }
    }
    // A body that moved must have come to rest on empty paper: clear of the text placed outside it and of the
    // text that stays.
    for (b, &by) in moved.iter().enumerate().filter(|(_, by)| by.abs() > 0.3) {
        let now = lower(&bodies[b].rect, by);
        let hit = |boxes: &[Rect]| boxes.iter().any(|x| x.overlaps(&now, 0.6));
        let text = live
            .iter()
            .any(|&i| frame.rides[i] != Some(b) && hit(&boxes[i]));
        let stays = layout
            .blocks
            .iter()
            .filter(|f| !is_live(&f.id) && frame.block_of.get(&f.id) != Some(&b))
            .any(|f| hit(&lines_of(f)));
        if text || stays {
            return Some(Change::Hold(b));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{TextLine, TextStyle};

    /// Every character is half an em wide.
    struct Mono;
    impl Measure for Mono {
        fn width(&self, text: &str, size: f32) -> f32 {
            text.chars().count() as f32 * size * 0.5
        }
        fn ascent(&self) -> f32 {
            0.8
        }
        fn descent(&self) -> f32 {
            0.2
        }
    }

    /// A block of `lines` full lines of 10pt text, 12pt apart, starting at `y`.
    fn block(id: &str, x0: f32, x1: f32, y: f32, lines: usize, protected: bool) -> TextBlock {
        let lines: Vec<TextLine> = (0..lines)
            .map(|k| {
                let top = y + 12.0 * k as f32;
                TextLine {
                    rect: Rect {
                        x0,
                        y0: top,
                        x1,
                        y1: top + 11.0,
                    },
                    text: "x".repeat(((x1 - x0) / 5.0) as usize),
                    baseline: top + 8.5,
                    spans: Vec::new(),
                }
            })
            .collect();
        TextBlock {
            id: BlockId(id.into()),
            rect: Rect {
                x0,
                y0: y,
                x1,
                y1: lines.last().unwrap().rect.y1,
            },
            lines,
            style: TextStyle {
                font_name: None,
                font_size: 10.0,
                color: 0,
                bold: false,
                italic: false,
                serif: true,
                monospace: false,
            },
            align: Align::Left,
            protected,
            separate_lines: false,
            initial: None,
            inline: Vec::new(),
        }
    }

    fn page(blocks: Vec<TextBlock>) -> PageLayout {
        PageLayout {
            page: 1,
            width: 400.0,
            height: 600.0,
            ocr: false,
            blocks,
        }
    }

    /// `lines` lines' worth of words for a 200pt measure.
    fn words(lines: f32) -> String {
        let n = (lines * 40.0 / 5.0) as usize;
        vec!["word"; n].join(" ")
    }

    fn solve_page(layout: &PageLayout, texts: &[(&str, String)]) -> Vec<Option<Placed>> {
        let items: Vec<Item> = texts
            .iter()
            .map(|(id, text)| Item {
                block: layout.blocks.iter().find(|b| b.id.0 == *id).unwrap(),
                text,
                measure: &Mono,
                companion: false,
                initial: None,
                confined: false,
            })
            .collect();
        solve(layout, &[], &items, false)
    }

    fn bottom(p: &Placed) -> f32 {
        p.lines.last().unwrap().baseline + p.size * 0.18
    }

    #[test]
    fn a_longer_paragraph_moves_the_one_below_instead_of_shrinking() {
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 3, false),
            block("b", 100.0, 300.0, 142.0, 3, false),
        ]);
        let out = solve_page(&layout, &[("a", words(3.8)), ("b", words(2.5))]);
        let (a, b) = (out[0].as_ref().unwrap(), out[1].as_ref().unwrap());
        assert_eq!((a.size, a.squeeze), (10.0, 1.0));
        assert_eq!(a.lines.len(), 4);
        assert!(b.shift > 11.0, "the second paragraph moved a line down");
        assert!(b.lines[0].baseline - a.size * 0.7 > bottom(a));
    }

    #[test]
    fn text_that_stays_is_a_floor_and_the_page_keeps_one_size() {
        // A paragraph, then code that is not translated, then another paragraph with room below.
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 4, false),
            block("code", 100.0, 300.0, 152.0, 2, true),
            block("b", 100.0, 300.0, 190.0, 4, false),
        ]);
        let out = solve_page(&layout, &[("a", words(4.6)), ("b", words(3.5))]);
        let (a, b) = (out[0].as_ref().unwrap(), out[1].as_ref().unwrap());
        assert!(bottom(a) <= 152.0, "stops above the code");
        assert!(a.size >= 7.0);
        assert_eq!((a.size, a.squeeze), (b.size, b.squeeze));
        assert_eq!(b.shift, 0.0);
    }

    #[test]
    fn what_cannot_fit_legibly_keeps_its_original() {
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 2, false),
            block("code", 100.0, 300.0, 124.0, 2, true),
        ]);
        let out = solve_page(&layout, &[("a", words(9.0))]);
        assert!(out[0].is_none());
    }

    #[test]
    fn a_heading_grows_sideways_up_to_its_neighbour() {
        let layout = page(vec![
            block("label", 50.0, 100.0, 100.0, 1, false),
            block("other", 180.0, 260.0, 100.0, 1, true),
        ]);
        // 22 characters need 110pt at full size; 125pt are free before the neighbour.
        let out = solve_page(&layout, &[("label", "abcdefghij klmnopqrstu".into())]);
        let p = out[0].as_ref().unwrap();
        assert_eq!((p.lines.len(), p.size, p.squeeze), (1, 10.0, 1.0));
        assert!(p.lines[0].x + p.lines[0].width <= 175.0);
        // Twice as long: it has to become narrower and smaller, but stays on its line and clear of the other.
        let out = solve_page(
            &layout,
            &[("label", "abcdefghij klmnopqrstu abcdefg".into())],
        );
        let p = out[0].as_ref().unwrap();
        assert_eq!(p.lines.len(), 1);
        assert!(p.size < 10.0 && p.size >= 8.5);
        assert!(p.lines[0].x + p.lines[0].width <= 175.0);
    }

    #[test]
    fn a_line_ends_before_what_stands_behind_it() {
        // The last line of a paragraph stops short, and other text goes on from there on the same line, as
        // it does behind a small picture set in the text.
        let mut first = block("a", 100.0, 300.0, 100.0, 3, false);
        first.lines[2].rect.x1 = 180.0;
        let layout = page(vec![first, block("rest", 200.0, 300.0, 124.0, 1, true)]);
        // Two lines' worth and most of a third: more than the third line has room for.
        let out = solve_page(&layout, &[("a", words(2.9))]);
        let a = out[0].as_ref().expect("the paragraph is placed");
        assert!(a.lines.len() >= 3);
        for line in a
            .lines
            .iter()
            .filter(|l| l.baseline > 126.0 && l.baseline < 138.0)
        {
            assert!(
                line.x + line.width <= 200.0,
                "ends at {}",
                line.x + line.width
            );
        }
    }

    fn solve_bodies(layout: &PageLayout, bodies: &[Body], texts: &[(&str, String)]) -> Solved {
        let items: Vec<Item> = texts
            .iter()
            .map(|(id, text)| Item {
                block: layout.blocks.iter().find(|b| b.id.0 == *id).unwrap(),
                text,
                measure: &Mono,
                companion: false,
                initial: None,
                confined: false,
            })
            .collect();
        let obstacles: Vec<Rect> = bodies.iter().map(|b| b.rect).collect();
        solve_among(layout, &obstacles, bodies, &items, false)
    }

    fn figure(y0: f32, y1: f32, movable: bool) -> Body {
        Body {
            rect: Rect {
                x0: 100.0,
                y0,
                x1: 300.0,
                y1,
            },
            movable,
        }
    }

    #[test]
    fn a_figure_goes_down_to_make_room_for_the_text_above_it() {
        // A paragraph, a figure under it, a paragraph under the figure, and half a page of room below.
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 3, false),
            block("b", 100.0, 300.0, 262.0, 3, false),
        ]);
        let texts = [("a", words(4.8)), ("b", words(2.5))];
        let out = solve_bodies(&layout, &[figure(150.0, 250.0, true)], &texts);
        let (a, b) = (
            out.placed[0].as_ref().unwrap(),
            out.placed[1].as_ref().unwrap(),
        );
        assert_eq!((a.size, a.squeeze, a.lines.len()), (10.0, 1.0, 5));
        assert_eq!(out.moved, [24.0], "two lines down, the space above it kept");
        assert_eq!(b.shift, 24.0, "and the paragraph under it with it");
        // A figure that cannot be taken from its place is a floor, as it was: the text gets smaller.
        let out = solve_bodies(&layout, &[figure(150.0, 250.0, false)], &texts);
        let a = out.placed[0].as_ref().unwrap();
        assert!(a.size < 10.0 || a.squeeze < 1.0);
        assert!(bottom(a) <= 150.0);
        assert_eq!(out.moved, [0.0]);
    }

    #[test]
    fn a_figure_with_no_room_under_it_stays() {
        // Right under the figure stands text that is not translated and is no body: nothing can go down.
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 3, false),
            block("code", 100.0, 300.0, 252.0, 2, true),
        ]);
        let out = solve_bodies(&layout, &[figure(150.0, 250.0, true)], &[("a", words(4.2))]);
        let a = out.placed[0].as_ref().unwrap();
        assert!(bottom(a) <= 150.0);
        assert_eq!(out.moved, [0.0]);
    }

    #[test]
    fn a_figure_goes_no_further_than_the_room_under_it() {
        // Fourteen points under the figure, then text that stays. The paragraph needs two lines more: the
        // figure gives what it has, and the text makes up the rest.
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 3, false),
            block("code", 100.0, 300.0, 264.5, 2, true),
        ]);
        let out = solve_bodies(&layout, &[figure(150.0, 250.0, true)], &[("a", words(4.8))]);
        let a = out.placed[0].as_ref().unwrap();
        assert!(
            out.moved[0] > 3.0 && out.moved[0] <= 14.0,
            "{:?}",
            out.moved
        );
        assert!(bottom(a) <= 150.0 + out.moved[0]);
        assert!(a.size >= 7.0);
    }

    #[test]
    fn text_inside_a_figure_goes_with_it() {
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 3, false),
            block("label", 120.0, 170.0, 180.0, 1, false),
        ]);
        let out = solve_bodies(
            &layout,
            &[figure(150.0, 250.0, true)],
            &[("a", words(4.8)), ("label", "label".into())],
        );
        assert_eq!(out.moved, [24.0]);
        let label = out.placed[1].as_ref().unwrap();
        assert_eq!(label.shift, 24.0);
        assert_eq!(label.lines[0].baseline, 188.5 + 24.0);
    }

    #[test]
    fn bodies_under_one_another_keep_their_distance() {
        // A rule, a listing and a rule: three bodies that are one thing to the eye of a reader.
        let layout = page(vec![block("a", 100.0, 300.0, 100.0, 3, false)]);
        let bodies = [
            figure(150.0, 151.0, true),
            figure(158.0, 200.0, true),
            figure(207.0, 208.0, true),
        ];
        let out = solve_bodies(&layout, &bodies, &[("a", words(3.8))]);
        assert_eq!(out.placed[0].as_ref().unwrap().size, 10.0);
        assert_eq!(out.moved, [12.0, 12.0, 12.0]);
    }

    #[test]
    fn text_goes_up_into_the_room_a_shorter_translation_left() {
        // Two paragraphs with a blank line between them, over text that stays. The first comes out two lines
        // shorter, the second one line longer: it has no room below, and goes up as far as it is short of room
        // instead of getting smaller.
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 4, false),
            block("b", 100.0, 300.0, 160.0, 3, false),
            block("code", 100.0, 300.0, 197.0, 2, true),
        ]);
        let out = solve_page(&layout, &[("a", words(1.8)), ("b", words(3.8))]);
        let (a, b) = (out[0].as_ref().unwrap(), out[1].as_ref().unwrap());
        assert_eq!((a.shift, a.lines.len()), (0.0, 2));
        assert_eq!((b.size, b.squeeze, b.lines.len()), (10.0, 1.0, 4));
        assert_eq!(b.shift, -10.5);
        assert!(bottom(b) <= 197.0);
        // A second paragraph that fits stays where it is: nothing moves that does not have to.
        let out = solve_page(&layout, &[("a", words(1.8)), ("b", words(2.8))]);
        assert_eq!(out[1].as_ref().unwrap().shift, 0.0);
        // With the first paragraph as long as it was there is no room over the second, and it gets smaller.
        let out = solve_page(&layout, &[("a", words(3.8)), ("b", words(3.8))]);
        let b = out[1].as_ref().unwrap();
        assert!(b.size < 10.0 || b.squeeze < 1.0);
        assert!(bottom(b) <= 197.0);
    }

    #[test]
    fn a_paragraph_follows_the_one_above_it() {
        // Three paragraphs with no blank line between them, then a heading after a wide space. The first
        // comes out two lines shorter: the others close up, the heading stays where it is.
        let mut heading = block("h", 100.0, 300.0, 260.0, 1, false);
        heading.style.font_size = 14.0;
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 4, false),
            block("b", 100.0, 300.0, 148.0, 3, false),
            block("c", 100.0, 300.0, 184.0, 3, false),
            heading,
        ]);
        let out = solve_page(
            &layout,
            &[
                ("a", words(1.8)),
                ("b", words(2.8)),
                ("c", words(2.8)),
                ("h", "Heading".into()),
            ],
        );
        let shifts: Vec<f32> = out.iter().map(|p| p.as_ref().unwrap().shift).collect();
        assert_eq!(shifts, [0.0, -24.0, -24.0, 0.0]);
    }

    #[test]
    fn a_listing_follows_the_text_above_it_and_takes_what_is_under_it_along() {
        // A paragraph, a listing right under it, a paragraph right under that. The first comes out two lines
        // shorter: the listing closes up, and so does the paragraph after it.
        let layout = page(vec![
            block("a", 100.0, 300.0, 100.0, 4, false),
            block("b", 100.0, 300.0, 208.0, 3, false),
        ]);
        let texts = [("a", words(1.8)), ("b", words(2.8))];
        let out = solve_bodies(&layout, &[figure(155.0, 200.0, true)], &texts);
        assert_eq!(out.moved, [-24.0]);
        assert_eq!(out.placed[1].as_ref().unwrap().shift, -24.0);
        // A figure set apart by more than a line stays where it is, and so does what is under it.
        let out = solve_bodies(&layout, &[figure(165.0, 200.0, true)], &texts);
        assert_eq!(out.moved, [0.0]);
        assert_eq!(out.placed[1].as_ref().unwrap().shift, 0.0);
    }

    #[test]
    fn a_word_is_not_broken_while_a_smaller_size_keeps_it_whole() {
        // A narrow block, and a word of 22 letters: 110 points at full size, 100 points of room.
        let layout = page(vec![block("a", 100.0, 200.0, 100.0, 2, false)]);
        let text = format!("one {} two", "w".repeat(22));
        let out = solve_page(&layout, &[("a", text.clone())]);
        let a = out[0].as_ref().unwrap();
        assert!(a.size < 10.0, "{a:?}");
        assert!(
            a.lines.iter().any(|l| l.text.contains(&"w".repeat(22))),
            "{a:?}"
        );
        // A word that no size would fit is broken, and the text keeps its size.
        let text = format!("one {} two", "w".repeat(40));
        let a = solve_page(&layout, &[("a", text)])[0].clone().unwrap();
        assert_eq!(a.size, 10.0);
    }

    #[test]
    fn blocks_whose_boxes_overlap_do_not_write_over_one_another() {
        // A block of two lines, the second short, and a block that begins on the same row after it: their boxes
        // overlap, their lines do not. The first comes out longer, and its second line would run into the other.
        let mut a = block("a", 100.0, 300.0, 100.0, 2, false);
        a.lines[1].rect.x1 = 180.0;
        let mut b = block("b", 100.0, 300.0, 112.0, 2, false);
        b.lines[0].rect.x0 = 200.0;
        b.rect.x0 = 100.0;
        let layout = page(vec![a, b]);
        let out = solve_page(&layout, &[("a", words(1.9)), ("b", words(1.2))]);
        let lines: Vec<Vec<Rect>> = out
            .iter()
            .map(|p| {
                p.as_ref().map_or_else(Vec::new, |p| {
                    p.lines
                        .iter()
                        .map(|l| core(l.x, l.x + l.width, l.baseline, p.size))
                        .collect()
                })
            })
            .collect();
        let over = lines[0]
            .iter()
            .any(|x| lines[1].iter().any(|y| x.overlaps(y, 0.6)));
        assert!(!over, "{out:?}");
        // Both are translated: the second begins under the first.
        let (a, b) = (out[0].as_ref().unwrap(), out[1].as_ref().unwrap());
        assert!(b.lines[0].baseline > a.lines.last().unwrap().baseline + 5.0);
        assert_eq!(b.lines[0].x, 100.0, "from the start of the line");
    }

    #[test]
    fn a_row_moves_together() {
        // A paragraph over a row of two cells; the first cell is under the paragraph, the second beside it.
        let layout = page(vec![
            block("a", 50.0, 150.0, 100.0, 2, false),
            block("left", 50.0, 150.0, 130.0, 1, false),
            block("right", 250.0, 350.0, 130.0, 1, false),
        ]);
        let out = solve_page(
            &layout,
            &[
                ("a", ["word"; 12].join(" ")),
                ("left", "cell".into()),
                ("right", "cell".into()),
            ],
        );
        let shifts: Vec<f32> = out.iter().map(|p| p.as_ref().unwrap().shift).collect();
        assert!(shifts[1] > 5.0);
        assert_eq!(shifts[1], shifts[2]);
    }
}
