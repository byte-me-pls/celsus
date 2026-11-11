//! The columns of a page, for a language that is read from right to left.
//!
//! In a book that reads right to left the first column is the one on the right. A page translated into such a
//! language keeps every column whole and changes their places: the columns are mirrored about the middle of the
//! text area, each with everything that stands in it (text, pictures, formulas, rules). What reaches across the
//! columns (a headline, a wide figure) stays where it is, and so does whatever stands in the margins.
//!
//! Nothing is mirrored unless the page plainly has columns: two or more measures of running text of about the
//! same width, side by side. A page where a column cannot be told from what lies across it is left as it is.

use crate::layout::{PageLayout, Rect, TextBlock};

/// One column: the strip it takes up, a little wider than its text, and how far it moves.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub x0: f32,
    pub x1: f32,
    pub dx: f32,
}

/// Where the columns of a page go.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// From left to right.
    pub columns: Vec<Column>,
    /// The stretches of the page, from the top, in which the columns stand side by side with nothing across them.
    pub bands: Vec<(f32, f32)>,
}

enum Place {
    /// Clear of every column.
    Outside,
    /// Inside one column.
    In(usize),
    /// In more than one column, or half out of one.
    Across,
}

fn place(columns: &[Column], r: &Rect) -> Place {
    let mut hit = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| r.x0 < c.x1 - 1.0 && r.x1 > c.x0 + 1.0);
    match (hit.next(), hit.next()) {
        (None, _) => Place::Outside,
        (Some((i, c)), None) if r.x0 >= c.x0 - 1.0 && r.x1 <= c.x1 + 1.0 => Place::In(i),
        _ => Place::Across,
    }
}

impl Plan {
    /// The column that holds a point of the page.
    pub fn column_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.bands.iter().any(|&(y0, y1)| y >= y0 && y < y1) {
            return None;
        }
        self.columns.iter().position(|c| x >= c.x0 && x < c.x1)
    }

    /// How far something with the outline `r` moves sideways, if it stands in a column.
    pub fn shift(&self, r: &Rect) -> Option<f32> {
        let Place::In(i) = place(&self.columns, r) else {
            return None;
        };
        let middle = (r.y0 + r.y1) / 2.0;
        self.bands
            .iter()
            .any(|&(y0, y1)| middle >= y0 && middle < y1)
            .then(|| self.columns[i].dx)
    }

    /// The page with its columns in their new places.
    pub fn moved(&self, layout: &PageLayout) -> PageLayout {
        let mut layout = layout.clone();
        for b in &mut layout.blocks {
            let Some(dx) = self.shift(&b.rect) else {
                continue;
            };
            let slide = |r: &mut Rect| {
                r.x0 += dx;
                r.x1 += dx;
            };
            slide(&mut b.rect);
            for line in &mut b.lines {
                slide(&mut line.rect);
            }
            if let Some(initial) = &mut b.initial {
                slide(&mut initial.rect);
            }
        }
        layout
    }

    /// Outlines of other things on the page, in their new places.
    pub fn moved_rects(&self, rects: &[Rect]) -> Vec<Rect> {
        rects
            .iter()
            .map(|r| match self.shift(r) {
                Some(dx) => Rect {
                    x0: r.x0 + dx,
                    x1: r.x1 + dx,
                    ..*r
                },
                None => *r,
            })
            .collect()
    }
}

/// Find the columns of a page and where each goes when the page is mirrored. `obstacles` are the pictures and
/// drawings on it.
pub fn plan(layout: &PageLayout, obstacles: &[Rect]) -> Option<Plan> {
    let blocks = &layout.blocks;
    let left = blocks.iter().map(|b| b.rect.x0).fold(f32::MAX, f32::min);
    let right = blocks.iter().map(|b| b.rect.x1).fold(f32::MIN, f32::max);
    let content = right - left;
    if content <= 0.0 {
        return None;
    }
    // Text that shows a column: lines that wrap, set to a measure clearly narrower than the text area.
    let shows = |b: &&TextBlock| {
        let width = b.rect.width();
        !b.protected
            && b.lines.len() >= 2
            && width >= b.style.font_size * 10.0
            && width < content * 0.6
    };
    let mut text: Vec<&TextBlock> = blocks.iter().filter(shows).collect();
    text.sort_by(|a, b| a.rect.x0.total_cmp(&b.rect.x0));
    let mut measures: Vec<Rect> = Vec::new();
    for b in text {
        let r = b.rect;
        let same = measures.iter_mut().find(|m| {
            let shared = r.x1.min(m.x1) - r.x0.max(m.x0);
            shared >= r.width().min(m.width()) * 0.6
        });
        match same {
            Some(m) => *m = m.union(&r),
            None => measures.push(r),
        }
    }
    measures.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    if measures.len() < 2 {
        return None;
    }
    // Columns are about equally wide, stand apart and side by side, and together fill the text area. A term
    // beside its definition, or a note beside the text, is not a pair of columns.
    let widest = measures.iter().map(Rect::width).fold(0.0, f32::max);
    let narrowest = measures.iter().map(Rect::width).fold(f32::MAX, f32::min);
    let mut gutter = f32::MAX;
    for pair in measures.windows(2) {
        let gap = pair[1].x0 - pair[0].x1;
        let beside = pair[0].y0 < pair[1].y1 && pair[1].y0 < pair[0].y1;
        if gap < 4.0 || !beside {
            return None;
        }
        gutter = gutter.min(gap);
    }
    let (first, last) = (measures[0], measures[measures.len() - 1]);
    if widest > narrowest * 1.25 || last.x1 - first.x0 < content * 0.7 {
        return None;
    }
    let pad = (gutter * 0.4).min(12.0);
    let columns: Vec<Column> = measures
        .iter()
        .map(|m| Column {
            x0: m.x0 - pad,
            x1: m.x1 + pad,
            dx: (first.x0 + last.x1) - (m.x0 + m.x1),
        })
        .collect();

    // Everything on the page is inside a column, across the columns, or clear of them.
    let outlines = || blocks.iter().map(|b| &b.rect).chain(obstacles);
    let inside: Vec<&Rect> = outlines()
        .filter(|r| matches!(place(&columns, r), Place::In(_)))
        .collect();
    let mut across: Vec<(f32, f32)> = outlines()
        .filter(|r| matches!(place(&columns, r), Place::Across))
        .map(|r| (r.y0, r.y1))
        .collect();
    // A column that runs beside something lying across it cannot be moved without tearing one of the two.
    if inside.iter().any(|r| {
        across
            .iter()
            .any(|&(y0, y1)| r.y0 < y1 - 1.0 && r.y1 > y0 + 1.0)
    }) {
        return None;
    }
    let top = inside.iter().map(|r| r.y0).fold(f32::MAX, f32::min) - 1.0;
    let bottom = inside.iter().map(|r| r.y1).fold(f32::MIN, f32::max) + 1.0;
    across.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut bands = Vec::new();
    let mut from = top;
    for (y0, y1) in across {
        if y0 > from {
            bands.push((from, y0.min(bottom)));
        }
        from = from.max(y1);
    }
    if from < bottom {
        bands.push((from, bottom));
    }
    bands.retain(|&(y0, y1)| {
        y1 > y0
            && inside.iter().any(|r| {
                let middle = (r.y0 + r.y1) / 2.0;
                middle >= y0 && middle < y1
            })
    });
    (!bands.is_empty()).then_some(Plan { columns, bands })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Align, BlockId, TextLine, TextStyle};

    fn block(id: &str, x0: f32, x1: f32, y0: f32, lines: usize) -> TextBlock {
        let line = |n: usize| TextLine {
            rect: Rect {
                x0,
                y0: y0 + 12.0 * n as f32,
                x1,
                y1: y0 + 12.0 * n as f32 + 10.0,
            },
            text: "words of a line".into(),
            baseline: y0 + 12.0 * n as f32 + 8.0,
            spans: Vec::new(),
        };
        TextBlock {
            id: BlockId(id.into()),
            rect: Rect {
                x0,
                y0,
                x1,
                y1: y0 + 12.0 * lines as f32 - 2.0,
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
    }

    fn page(blocks: Vec<TextBlock>) -> PageLayout {
        PageLayout {
            page: 1,
            width: 612.0,
            height: 792.0,
            ocr: false,
            blocks,
        }
    }

    /// A headline over two columns of two paragraphs each.
    fn two_columns() -> PageLayout {
        page(vec![
            block("head", 72.0, 540.0, 72.0, 1),
            block("l1", 72.0, 296.0, 110.0, 8),
            block("l2", 72.0, 296.0, 220.0, 8),
            block("r1", 316.0, 540.0, 110.0, 8),
            block("r2", 316.0, 540.0, 220.0, 5),
        ])
    }

    fn rect_of<'a>(layout: &'a PageLayout, id: &str) -> &'a Rect {
        &layout.blocks.iter().find(|b| b.id.0 == id).unwrap().rect
    }

    #[test]
    fn two_columns_change_places_and_the_headline_stays() {
        let layout = two_columns();
        let plan = plan(&layout, &[]).expect("two columns");
        assert_eq!(plan.columns.len(), 2);
        let moved = plan.moved(&layout);
        assert_eq!(rect_of(&moved, "head"), rect_of(&layout, "head"));
        assert_eq!(rect_of(&moved, "l1").x0, 316.0);
        assert_eq!(rect_of(&moved, "l2").x1, 540.0);
        assert_eq!(rect_of(&moved, "r1").x0, 72.0);
        assert_eq!(rect_of(&moved, "r2").x1, 296.0);
        // Lines move with their block, and nothing moves up or down.
        let l1 = moved.blocks.iter().find(|b| b.id.0 == "l1").unwrap();
        assert_eq!(l1.lines[3].rect.x0, 316.0);
        assert_eq!(l1.rect.y0, 110.0);
    }

    #[test]
    fn a_picture_and_a_formula_move_with_their_column() {
        let mut layout = two_columns();
        let mut formula = block("formula", 120.0, 250.0, 330.0, 1);
        formula.protected = true;
        layout.blocks.push(formula);
        let picture = Rect {
            x0: 330.0,
            y0: 300.0,
            x1: 530.0,
            y1: 400.0,
        };
        let plan = plan(&layout, &[picture]).expect("two columns");
        let moved = plan.moved(&layout);
        assert_eq!(rect_of(&moved, "formula").x0, 120.0 + 244.0);
        assert_eq!(plan.moved_rects(&[picture])[0].x0, 330.0 - 244.0);
        // A glyph of the left column belongs to it, one in the margin or in the headline to nothing.
        assert_eq!(plan.column_at(100.0, 150.0), Some(0));
        assert_eq!(plan.column_at(400.0, 350.0), Some(1));
        assert_eq!(plan.column_at(30.0, 150.0), None);
        assert_eq!(plan.column_at(100.0, 76.0), None);
    }

    #[test]
    fn columns_above_and_below_a_wide_figure_move_and_the_figure_stays() {
        let layout = page(vec![
            block("l1", 72.0, 296.0, 72.0, 8),
            block("r1", 316.0, 540.0, 72.0, 8),
            block("caption", 150.0, 460.0, 330.0, 1),
            block("l2", 72.0, 296.0, 380.0, 8),
            block("r2", 316.0, 540.0, 380.0, 8),
        ]);
        let figure = Rect {
            x0: 100.0,
            y0: 190.0,
            x1: 512.0,
            y1: 320.0,
        };
        let plan = plan(&layout, &[figure]).expect("two columns");
        assert_eq!(plan.bands.len(), 2);
        let moved = plan.moved(&layout);
        assert_eq!(rect_of(&moved, "l1").x0, 316.0);
        assert_eq!(rect_of(&moved, "r2").x0, 72.0);
        assert_eq!(rect_of(&moved, "caption"), rect_of(&layout, "caption"));
        assert_eq!(plan.moved_rects(&[figure])[0], figure);
    }

    #[test]
    fn a_single_column_is_left_alone() {
        let layout = page(vec![
            block("a", 72.0, 540.0, 72.0, 8),
            block("b", 72.0, 540.0, 180.0, 8),
            block("quote", 110.0, 500.0, 290.0, 4),
        ]);
        assert_eq!(plan(&layout, &[]), None);
    }

    #[test]
    fn a_note_beside_the_text_is_not_a_column() {
        let layout = page(vec![
            block("text", 72.0, 400.0, 72.0, 12),
            block("note", 420.0, 540.0, 72.0, 4),
        ]);
        assert_eq!(plan(&layout, &[]), None);
    }

    #[test]
    fn a_column_beside_something_that_lies_across_it_is_left_alone() {
        let layout = two_columns();
        // A picture that starts in the left column and ends in the right one, next to the text of both.
        let picture = Rect {
            x0: 200.0,
            y0: 150.0,
            x1: 400.0,
            y1: 250.0,
        };
        assert_eq!(plan(&layout, &[picture]), None);
    }
}
