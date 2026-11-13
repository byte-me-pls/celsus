//! Seeing a page as a picture.
//!
//! The content of a page says where its text is. What else is on the page (a drawing, a rule, a photograph, the
//! figure in a scanned page) is known for sure only by looking. The page is drawn the way a reader sees it, and
//! everything on it that is neither paper nor text to be translated is gathered into bodies: the things a
//! translation must not run into, and that can be moved as they are to make room.
//!
//! Drawing a page means reading a file from anywhere a second time, with other code than the rest of the
//! application uses. It happens on a thread of its own: a page that cannot be drawn, or takes too long, is a
//! page not seen, and the translation is placed on it by what the content says alone.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

use crate::layout::Rect;

/// Pixels to a point: about a hundred to the inch, enough to tell a hairline from paper.
const SCALE: f32 = 100.0 / 72.0;
/// A page is never drawn larger than this many pixels.
const MOST_PIXELS: f32 = 2_400_000.0;
/// How long one page may take to draw.
const PATIENCE: Duration = Duration::from_secs(8);
/// Things closer to one another than this, in points, are one body.
const JOIN: f32 = 6.0;

/// Draws the pages of one document.
pub struct Eye {
    ask: Sender<usize>,
    answer: Receiver<Option<Drawn>>,
    /// A page took too long. The thread is still at it, and no more is asked of it.
    blind: Cell<bool>,
}

struct Drawn {
    width: usize,
    height: usize,
    /// Size of the page in points, as drawn.
    points: (f32, f32),
    rgb: Vec<u8>,
}

impl Eye {
    /// `file` is the whole PDF, known to have `pages` pages. Read another way it may come out with other
    /// pages, and then none of them is gone by.
    pub fn open(file: &[u8], pages: usize) -> Eye {
        let (ask, asked) = channel::<usize>();
        let (tell, answer) = channel::<Option<Drawn>>();
        let data = Arc::new(file.to_vec());
        let work = move || {
            let pdf = std::panic::catch_unwind(|| Pdf::new(data).ok())
                .ok()
                .flatten()
                .filter(|pdf| pdf.pages().len() == pages);
            for index in asked {
                let drawn = pdf.as_ref().and_then(|pdf| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw(pdf, index)))
                        .ok()
                        .flatten()
                });
                if tell.send(drawn).is_err() {
                    break;
                }
            }
        };
        // Without a thread nothing answers, and every page goes unseen.
        let _ = std::thread::Builder::new()
            .name("page-eye".into())
            .stack_size(32 << 20)
            .spawn(work);
        Eye {
            ask,
            answer,
            blind: Cell::new(false),
        }
    }

    /// The page with this index (from 0) as a picture, if it can be drawn and is `width` by `height` points
    /// the way it is drawn: a page that is shown turned has other coordinates than its content, and is not
    /// looked at.
    pub fn look(&self, index: usize, width: f32, height: f32) -> Option<Picture> {
        if self.blind.get() || self.ask.send(index).is_err() {
            return None;
        }
        let Ok(drawn) = self.answer.recv_timeout(PATIENCE) else {
            self.blind.set(true);
            return None;
        };
        let drawn = drawn?;
        if (drawn.points.0 - width).abs() > 1.5 || (drawn.points.1 - height).abs() > 1.5 {
            return None;
        }
        Some(Picture::new(
            drawn.width,
            drawn.height,
            drawn.width as f32 / width,
            drawn.rgb,
        ))
    }
}

fn draw(pdf: &Pdf, index: usize) -> Option<Drawn> {
    let pages = pdf.pages();
    let page = pages.get(index)?;
    let (w, h) = page.render_dimensions();
    if !(w >= 1.0 && h >= 1.0 && w < 20_000.0 && h < 20_000.0) {
        return None;
    }
    let scale = SCALE.min((MOST_PIXELS / (w * h)).sqrt());
    let pixmap = render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    if width == 0 || height == 0 {
        return None;
    }
    let rgb = pixmap
        .data_as_u8_slice()
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    Some(Drawn {
        width,
        height,
        points: (w, h),
        rgb,
    })
}

/// A piece of ink: pixels that touch one another.
#[derive(Clone, Copy)]
struct Piece {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

/// What a picture shows of the text of a page and of what is around it.
pub struct Found {
    /// For each line of the text, the box around its letters as they are seen. `None` for a line no ink was
    /// found on.
    pub inks: Vec<Option<Rect>>,
    /// For each line, the row of dots that leads on from it, if there is one.
    pub leaders: Vec<Option<Rect>>,
    /// The bodies of the page: everything on it that is not paper, not the text and not such a row of dots.
    /// Each is the box around ink that lies together, in points from the top-left of the page.
    pub bodies: Vec<Rect>,
}

/// A page as it looks.
pub struct Picture {
    width: usize,
    height: usize,
    /// Pixels to a point.
    scale: f32,
    rgb: Vec<u8>,
    paper: [u8; 3],
    /// How far from the colour of the paper a pixel has to be to count as ink.
    apart: u8,
    /// For every pixel the piece of ink it is part of, counted from 1, or 0 for paper.
    piece: Vec<u32>,
    pieces: Vec<Piece>,
}

fn apart(a: [u8; 3], b: [u8; 3]) -> u8 {
    a[0].abs_diff(b[0])
        .max(a[1].abs_diff(b[1]))
        .max(a[2].abs_diff(b[2]))
}

impl Picture {
    /// A picture of `width` by `height` pixels, three bytes a pixel, drawn at `scale` pixels to a point.
    pub fn new(width: usize, height: usize, scale: f32, rgb: Vec<u8>) -> Picture {
        assert_eq!(rgb.len(), width * height * 3);
        let pixel = |i: usize| [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]];
        // The paper: the colour most of the page has.
        let mut counts = vec![0u32; 1 << 15];
        let bin = |p: [u8; 3]| {
            (usize::from(p[0]) >> 3) << 10 | (usize::from(p[1]) >> 3) << 5 | usize::from(p[2]) >> 3
        };
        for i in 0..width * height {
            counts[bin(pixel(i))] += 1;
        }
        let most = (0..counts.len()).max_by_key(|&b| counts[b]).unwrap_or(0);
        let (mut sum, mut n) = ([0u64; 3], 0u64);
        for i in 0..width * height {
            let p = pixel(i);
            if bin(p) == most {
                for c in 0..3 {
                    sum[c] += u64::from(p[c]);
                }
                n += 1;
            }
        }
        let paper = if n == 0 {
            [255; 3]
        } else {
            [0, 1, 2].map(|c| (sum[c] / n) as u8)
        };
        // Scanned paper is not of one colour. Ink is what stands further from the paper than its grain does.
        let (mut grain, mut near) = (0u64, 0u64);
        for i in 0..width * height {
            let d = apart(pixel(i), paper);
            if d < 48 {
                grain += u64::from(d);
                near += 1;
            }
        }
        let grain = if near == 0 {
            0.0
        } else {
            grain as f32 / near as f32
        };
        let limit = (10.0 + grain * 5.0).clamp(12.0, 64.0) as u8;

        // The pieces of ink.
        let mut piece = vec![0u32; width * height];
        let mut pieces: Vec<Piece> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for start in 0..width * height {
            if piece[start] != 0 || apart(pixel(start), paper) <= limit {
                continue;
            }
            let id = pieces.len() as u32 + 1;
            let mut p = Piece {
                x0: usize::MAX,
                y0: usize::MAX,
                x1: 0,
                y1: 0,
            };
            piece[start] = id;
            stack.push(start);
            while let Some(i) = stack.pop() {
                let (x, y) = (i % width, i / width);
                p.x0 = p.x0.min(x);
                p.x1 = p.x1.max(x + 1);
                p.y0 = p.y0.min(y);
                p.y1 = p.y1.max(y + 1);
                for (dx, dy) in [
                    (-1i32, -1i32),
                    (0, -1),
                    (1, -1),
                    (-1, 0),
                    (1, 0),
                    (-1, 1),
                    (0, 1),
                    (1, 1),
                ] {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                        continue;
                    }
                    let j = ny as usize * width + nx as usize;
                    if piece[j] == 0 && apart(pixel(j), paper) > limit {
                        piece[j] = id;
                        stack.push(j);
                    }
                }
            }
            pieces.push(p);
        }
        Picture {
            width,
            height,
            scale,
            rgb,
            paper,
            apart: limit,
            piece,
            pieces,
        }
    }

    /// The colour of the paper.
    pub fn paper(&self) -> [u8; 3] {
        self.paper
    }

    /// A rectangle of the page, in pixels, kept inside the picture.
    fn pixels(&self, r: &Rect) -> (usize, usize, usize, usize) {
        let at = |v: f32, most: usize| ((v * self.scale).max(0.0) as usize).min(most);
        (
            at(r.x0, self.width),
            at(r.y0, self.height),
            at(r.x1 + 1.0 / self.scale, self.width),
            at(r.y1 + 1.0 / self.scale, self.height),
        )
    }

    fn rect(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> Rect {
        Rect {
            x0: x0 as f32 / self.scale,
            y0: y0 as f32 / self.scale,
            x1: x1 as f32 / self.scale,
            y1: y1 as f32 / self.scale,
        }
    }

    /// For each piece of ink, the line of `text` it is a letter of, if any: the piece lies with its middle on the
    /// line, and is no larger than a letter of that line could be. A stroke that reaches out of the box of its
    /// line is still of that line; a rule or a drawing that crosses a line is not text.
    fn letters(&self, text: &[Rect]) -> Vec<Option<usize>> {
        let mut of = vec![None; self.pieces.len()];
        // Lines by the row of pixels they start on, to find the lines near a piece without trying them all.
        let mut order: Vec<usize> = (0..text.len()).collect();
        order.sort_by(|&a, &b| text[a].y0.total_cmp(&text[b].y0));
        let tallest = text.iter().map(|t| t.height()).fold(0.0, f32::max);
        for (k, p) in self.pieces.iter().enumerate() {
            let r = self.rect(p.x0, p.y0, p.x1, p.y1);
            let (mx, my) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
            let from = order.partition_point(|&i| text[i].y0 < my - tallest * 1.5 - 1.0);
            for &i in &order[from..] {
                let t = &text[i];
                let pad = t.height() * 0.25;
                if t.y0 - pad > my {
                    break;
                }
                if my <= t.y1 + pad
                    && mx >= t.x0 - pad
                    && mx <= t.x1 + pad
                    && r.height() <= t.height() * 2.0
                    && r.width() <= t.width() + t.height() * 2.0
                {
                    of[k] = Some(i);
                    break;
                }
            }
        }
        // The box of a line is not always around all of its letters: the text laid over a scanned page says
        // where a line is only roughly. A piece the size of a letter that stands on the line, as close to its
        // letters as letters stand to one another, is one more letter of it.
        let mut inked: Vec<Option<Rect>> = vec![None; text.len()];
        for (k, line) in of.iter().enumerate() {
            if let Some(i) = *line {
                let p = &self.pieces[k];
                let r = self.rect(p.x0, p.y0, p.x1, p.y1);
                inked[i] = Some(inked[i].map_or(r, |b| b.union(&r)));
            }
        }
        let mut near: Vec<Vec<usize>> = vec![Vec::new(); text.len()];
        for (k, p) in self.pieces.iter().enumerate() {
            if of[k].is_some() {
                continue;
            }
            let r = self.rect(p.x0, p.y0, p.x1, p.y1);
            let my = (r.y0 + r.y1) / 2.0;
            let from = order.partition_point(|&i| text[i].y0 < my - tallest * 1.5 - 1.0);
            for &i in &order[from..] {
                let t = &text[i];
                let h = t.height();
                if t.y0 - h * 0.25 > my {
                    break;
                }
                if my <= t.y1 + h * 0.25
                    && r.height() <= h * 1.5
                    && r.width() <= h * 1.5
                    && r.x1 >= t.x0 - h * 3.0
                    && r.x0 <= t.x1 + h * 3.0
                {
                    near[i].push(k);
                }
            }
        }
        for (i, candidates) in near.iter_mut().enumerate() {
            let Some(mut ink) = inked[i] else { continue };
            let reach = text[i].height() * 0.35;
            loop {
                let before = candidates.len();
                candidates.retain(|&k| {
                    if of[k].is_some() {
                        return false;
                    }
                    let p = &self.pieces[k];
                    let r = self.rect(p.x0, p.y0, p.x1, p.y1);
                    if r.x0 <= ink.x1 + reach && r.x1 >= ink.x0 - reach {
                        of[k] = Some(i);
                        ink = ink.union(&r);
                        false
                    } else {
                        true
                    }
                });
                if candidates.len() == before {
                    break;
                }
            }
        }
        of
    }

    /// The rows of dots that lead from a line of `text` to something further right on it, as in a table of
    /// contents: for each line the box of its row, and for each piece of ink whether it is such a dot. A row
    /// of dots is neither text nor a thing of its own. It fills the room the text leaves, and a longer text
    /// takes some of it.
    fn leaders(
        &self,
        text: &[Rect],
        letters: &[Option<usize>],
        inks: &[Option<Rect>],
    ) -> (Vec<Option<Rect>>, Vec<bool>) {
        let mut rows: Vec<Option<Rect>> = vec![None; text.len()];
        let mut dot = vec![false; self.pieces.len()];
        let mut order: Vec<usize> = (0..text.len()).collect();
        order.sort_by(|&a, &b| text[a].y0.total_cmp(&text[b].y0));
        let tallest = text.iter().map(|t| t.height()).fold(0.0, f32::max);
        // Small pieces that stand low on a line, past the end of its letters.
        let mut after: Vec<Vec<usize>> = vec![Vec::new(); text.len()];
        for (k, p) in self.pieces.iter().enumerate() {
            let r = self.rect(p.x0, p.y0, p.x1, p.y1);
            if letters[k].is_some() || r.height() > tallest * 0.35 || r.width() > tallest * 0.35 {
                continue;
            }
            let my = (r.y0 + r.y1) / 2.0;
            let from = order.partition_point(|&i| text[i].y0 < my - tallest * 1.5 - 1.0);
            for &i in &order[from..] {
                let (t, h) = (&text[i], text[i].height());
                if t.y0 > my {
                    break;
                }
                let Some(ink) = inks[i] else { continue };
                if my >= t.y0 + h * 0.45
                    && my <= t.y1 + h * 0.15
                    && r.height() <= h * 0.35
                    && r.width() <= h * 0.35
                    && r.x0 >= ink.x1 - h * 0.1
                {
                    after[i].push(k);
                    break;
                }
            }
        }
        for (i, pieces) in after.iter_mut().enumerate() {
            let (Some(ink), h) = (inks[i], text[i].height()) else {
                continue;
            };
            pieces.sort_by_key(|&k| self.pieces[k].x0);
            // The row begins soon after the text and goes on in even steps.
            let mut run: Vec<usize> = Vec::new();
            let mut end = ink.x1;
            for &k in pieces.iter() {
                let p = &self.pieces[k];
                let r = self.rect(p.x0, p.y0, p.x1, p.y1);
                let most = if run.is_empty() { h * 2.5 } else { h * 1.5 };
                if r.x0 - end > most {
                    break;
                }
                run.push(k);
                end = r.x1;
            }
            if run.len() >= 5 {
                for &k in &run {
                    let p = &self.pieces[k];
                    let r = self.rect(p.x0, p.y0, p.x1, p.y1);
                    rows[i] = Some(rows[i].map_or(r, |b| b.union(&r)));
                    dot[k] = true;
                }
            }
        }
        (rows, dot)
    }

    /// What is seen of the `text` (its lines, one rectangle each) and around it. `solid` are places known to
    /// hold one thing each, a block of text that stays for one.
    pub fn find(&self, text: &[Rect], solid: &[Rect]) -> Found {
        let letters = self.letters(text);
        let mut inks: Vec<Option<Rect>> = vec![None; text.len()];
        for (k, line) in letters.iter().enumerate() {
            let Some(i) = *line else { continue };
            let p = &self.pieces[k];
            let r = self.rect(p.x0, p.y0, p.x1, p.y1);
            inks[i] = Some(inks[i].map_or(r, |b| b.union(&r)));
        }
        let (leaders, dots) = self.leaders(text, &letters, &inks);
        let taken: Vec<bool> = letters
            .iter()
            .zip(&dots)
            .map(|(letter, dot)| letter.is_some() || *dot)
            .collect();
        Found {
            bodies: self.gather(&taken, solid),
            inks,
            leaders,
        }
    }

    /// Where the ink of each line of `text` really is.
    pub fn ink_of(&self, text: &[Rect]) -> Vec<Option<Rect>> {
        self.find(text, &[]).inks
    }

    /// The bodies of the page, with `text` and `solid` as for [`Picture::find`].
    pub fn bodies(&self, text: &[Rect], solid: &[Rect]) -> Vec<Rect> {
        self.find(text, solid).bodies
    }

    /// Everything seen that is not paper and not `taken` (one flag for each piece of ink), gathered into the
    /// boxes around ink that lies together.
    fn gather(&self, taken: &[bool], solid: &[Rect]) -> Vec<Rect> {
        // The page in cells of two pixels: which of them hold something.
        const CELL: usize = 2;
        let (gw, gh) = (self.width.div_ceil(CELL), self.height.div_ceil(CELL));
        let mut held = vec![false; gw * gh];
        for (i, &id) in self.piece.iter().enumerate() {
            if id != 0 && !taken[id as usize - 1] {
                held[(i / self.width / CELL) * gw + (i % self.width) / CELL] = true;
            }
        }
        for s in solid {
            let (x0, y0, x1, y1) = self.pixels(s);
            for y in y0 / CELL..y1.div_ceil(CELL).min(gh) {
                for x in x0 / CELL..x1.div_ceil(CELL).min(gw) {
                    held[y * gw + x] = true;
                }
            }
        }
        // Grown by half the joining distance on every side, things that lie together touch.
        let reach = ((JOIN * self.scale / 2.0 / CELL as f32).round() as usize).max(1);
        let mut wide = vec![false; gw * gh];
        for y in 0..gh {
            let mut last: Option<usize> = None;
            for x in 0..gw {
                if held[y * gw + x] {
                    let from = last.map_or(x.saturating_sub(reach), |l| {
                        (l + 1).max(x.saturating_sub(reach))
                    });
                    for cell in &mut wide[y * gw + from..=y * gw + (x + reach).min(gw - 1)] {
                        *cell = true;
                    }
                    last = Some((x + reach).min(gw - 1));
                }
            }
        }
        let mut grown = vec![false; gw * gh];
        for x in 0..gw {
            let mut last: Option<usize> = None;
            for y in 0..gh {
                if wide[y * gw + x] {
                    let from = last.map_or(y.saturating_sub(reach), |l| {
                        (l + 1).max(y.saturating_sub(reach))
                    });
                    for yy in from..=(y + reach).min(gh - 1) {
                        grown[yy * gw + x] = true;
                    }
                    last = Some((y + reach).min(gh - 1));
                }
            }
        }
        // What touches now is one body.
        let mut body = vec![0u32; gw * gh];
        let mut count = 0u32;
        let mut stack: Vec<usize> = Vec::new();
        for start in 0..gw * gh {
            if !grown[start] || body[start] != 0 {
                continue;
            }
            count += 1;
            body[start] = count;
            stack.push(start);
            while let Some(i) = stack.pop() {
                let (x, y) = (i % gw, i / gw);
                let mut go = |j: usize| {
                    if grown[j] && body[j] == 0 {
                        body[j] = count;
                        stack.push(j);
                    }
                };
                if x > 0 {
                    go(i - 1);
                }
                if x + 1 < gw {
                    go(i + 1);
                }
                if y > 0 {
                    go(i - gw);
                }
                if y + 1 < gh {
                    go(i + gw);
                }
            }
        }
        // The box of each body: around what it holds, not around the room it was grown by.
        let mut boxes: Vec<Option<(usize, usize, usize, usize)>> = vec![None; count as usize];
        let mut ink = vec![0usize; count as usize];
        let mut add = |n: u32, x0: usize, y0: usize, x1: usize, y1: usize| {
            let b = &mut boxes[n as usize - 1];
            *b = Some(b.map_or((x0, y0, x1, y1), |(a0, b0, a1, b1)| {
                (a0.min(x0), b0.min(y0), a1.max(x1), b1.max(y1))
            }));
        };
        for (i, &id) in self.piece.iter().enumerate() {
            if id != 0 && !taken[id as usize - 1] {
                let (x, y) = (i % self.width, i / self.width);
                let n = body[(y / CELL) * gw + x / CELL];
                add(n, x, y, x + 1, y + 1);
                ink[n as usize - 1] += 1;
            }
        }
        let mut is_solid = vec![false; count as usize];
        for s in solid {
            let (x0, y0, x1, y1) = self.pixels(s);
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            let n = body[(y0 / CELL) * gw + x0 / CELL];
            add(n, x0, y0, x1, y1);
            is_solid[n as usize - 1] = true;
        }
        // A few stray pixels are dust on the glass of a scanner.
        let dust = (self.scale * self.scale * 2.0).ceil() as usize;
        boxes
            .into_iter()
            .enumerate()
            .filter(|&(n, _)| is_solid[n] || ink[n] > dust)
            .filter_map(|(_, b)| b)
            .map(|(x0, y0, x1, y1)| self.rect(x0, y0, x1, y1))
            .collect()
    }

    /// How thick the strokes of a line of text look, in points: in the middle band of its letters (`ink`, the
    /// box around them as they are seen), the ink of each row divided by the number of strokes it crosses, the
    /// middle of that over the rows. `None` where nothing is seen.
    pub fn stroke(&self, ink: &Rect) -> Option<f32> {
        let (x0, y0, x1, y1) = self.pixels(ink);
        if x1 <= x0 + 2 || y1 <= y0 + 4 {
            return None;
        }
        let band = (y1 - y0) as f32;
        let (b0, b1) = (
            y0 + (band * 0.40) as usize,
            (y0 + (band * 0.65).ceil() as usize).min(y1),
        );
        // How dark a pixel is, from the paper (0) to the darkest ink of the line (1).
        let darkest = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (y, x)))
            .map(|(y, x)| self.apart_at(x, y))
            .max()
            .unwrap_or(0);
        if darkest < self.apart.saturating_mul(2) {
            return None;
        }
        let mut strokes: Vec<f32> = Vec::new();
        for y in b0..b1.max(b0 + 1) {
            let (mut ink, mut runs, mut on) = (0.0f32, 0usize, false);
            for x in x0..x1 {
                let d = f32::from(self.apart_at(x, y)) / f32::from(darkest);
                ink += d.min(1.0);
                if d > 0.5 && !on {
                    runs += 1;
                }
                on = d > 0.5;
            }
            if runs > 0 {
                strokes.push(ink / runs as f32);
            }
        }
        if strokes.is_empty() {
            return None;
        }
        // A row through the bar of an E or a T crosses one long stroke and says little; most rows cross only
        // the upright ones, and the thinner rows are taken.
        strokes.sort_by(f32::total_cmp);
        Some(strokes[strokes.len() * 3 / 10] / self.scale)
    }

    fn apart_at(&self, x: usize, y: usize) -> u8 {
        let i = (y * self.width + x) * 3;
        apart([self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]], self.paper)
    }

    /// The colour around `r`, if the paper there is of one colour: what the place of a body is to be painted
    /// with once the body has been moved away.
    pub fn paper_around(&self, r: &Rect) -> Option<[u8; 3]> {
        let (x0, y0, x1, y1) = self.pixels(r);
        let (a0, b0) = (x0.saturating_sub(3), y0.saturating_sub(3));
        let (a1, b1) = ((x1 + 3).min(self.width), (y1 + 3).min(self.height));
        let mut ring: Vec<[u8; 3]> = Vec::new();
        for y in b0..b1 {
            for x in a0..a1 {
                // One pixel next to the box is left out: the soft edge of the ink.
                let inside = x + 1 >= x0 && x < x1 + 1 && y + 1 >= y0 && y < y1 + 1;
                if !inside {
                    let i = (y * self.width + x) * 3;
                    ring.push([self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]);
                }
            }
        }
        if ring.is_empty() {
            return Some(self.paper);
        }
        let middle = [0, 1, 2].map(|c| {
            let mut v: Vec<u8> = ring.iter().map(|p| p[c]).collect();
            v.sort_unstable();
            v[v.len() / 2]
        });
        let same = ring
            .iter()
            .filter(|p| apart(**p, middle) <= self.apart)
            .count();
        (same * 100 >= ring.len() * 97).then_some(middle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white picture of 200 by 300 pixels, two pixels to a point, with black rectangles (in pixels).
    fn picture(black: &[(usize, usize, usize, usize)]) -> Picture {
        let (w, h) = (200, 300);
        let mut rgb = vec![255u8; w * h * 3];
        for &(x0, y0, x1, y1) in black {
            for y in y0..y1 {
                for x in x0..x1 {
                    rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(0);
                }
            }
        }
        Picture::new(w, h, 2.0, rgb)
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    #[test]
    fn what_is_not_text_is_gathered_into_bodies() {
        // Two letters of a line, and under them a drawing in two parts that lie together.
        let p = picture(&[
            (20, 20, 26, 30),
            (30, 20, 36, 30),
            (20, 100, 80, 140),
            (86, 100, 150, 140),
        ]);
        let line = rect(10.0, 10.0, 18.0, 15.0);
        assert_eq!(p.bodies(&[line], &[]), [rect(10.0, 50.0, 75.0, 70.0)]);
        // With no text known, the letters are a body of their own.
        assert_eq!(p.bodies(&[], &[]).len(), 2);
    }

    #[test]
    fn a_letter_that_reaches_out_of_its_line_is_still_text() {
        // The second letter stands a little past the end of the box of the line.
        let p = picture(&[(20, 20, 26, 30), (34, 20, 40, 30)]);
        let line = rect(10.0, 10.0, 18.0, 15.0);
        assert!(p.bodies(&[line], &[]).is_empty());
        assert_eq!(p.ink_of(&[line]), [Some(rect(10.0, 10.0, 20.0, 15.0))]);
    }

    #[test]
    fn letters_past_the_end_of_the_box_of_a_line_are_of_the_line() {
        // Four letters, the box of the line around the first two only: the others follow as letters do. What
        // stands a word and more away is something else.
        let p = picture(&[
            (20, 20, 26, 30),
            (28, 20, 34, 30),
            (36, 20, 42, 30),
            (44, 20, 50, 30),
            (70, 20, 76, 30),
        ]);
        let line = rect(10.0, 10.0, 17.0, 15.0);
        assert_eq!(p.ink_of(&[line]), [Some(rect(10.0, 10.0, 25.0, 15.0))]);
        assert_eq!(p.bodies(&[line], &[]), [rect(35.0, 10.0, 38.0, 15.0)]);
    }

    #[test]
    fn a_row_of_dots_after_a_line_is_its_leader() {
        // Two letters, then eight dots on the foot of the line, then a number far to the right.
        let mut black = vec![(20, 20, 26, 30), (28, 20, 34, 30)];
        black.extend((0..8).map(|k| (44 + k * 8, 28, 46 + k * 8, 30)));
        black.push((150, 20, 156, 30));
        let p = picture(&black);
        let line = rect(10.0, 10.0, 17.0, 15.0);
        let found = p.find(&[line], &[]);
        assert_eq!(found.inks, [Some(rect(10.0, 10.0, 17.0, 15.0))]);
        assert_eq!(found.leaders, [Some(rect(22.0, 14.0, 51.0, 15.0))]);
        assert_eq!(
            found.bodies,
            [rect(75.0, 10.0, 78.0, 15.0)],
            "only the number"
        );
        // Three dots are an ellipsis or dust, not a leader.
        let p = picture(&black[..5]);
        assert_eq!(p.find(&[line], &[]).leaders, [None]);
    }

    #[test]
    fn the_strokes_of_letters_are_measured() {
        // Four upright strokes of 3 pixels and four of 6, at 2 pixels to a point.
        let thin: Vec<_> = (0..4).map(|k| (20 + k * 12, 20, 23 + k * 12, 40)).collect();
        let thick: Vec<_> = (0..4).map(|k| (20 + k * 14, 60, 26 + k * 14, 80)).collect();
        let p = picture(&[thin, thick].concat());
        assert_eq!(p.stroke(&rect(10.0, 10.0, 30.0, 20.0)), Some(1.5));
        assert_eq!(p.stroke(&rect(10.0, 30.0, 40.0, 40.0)), Some(3.0));
        assert_eq!(p.stroke(&rect(60.0, 100.0, 80.0, 120.0)), None);
    }

    #[test]
    fn things_far_apart_are_bodies_of_their_own_and_dust_is_none() {
        let p = picture(&[(20, 40, 180, 42), (20, 80, 180, 82), (100, 200, 102, 202)]);
        assert_eq!(
            p.bodies(&[], &[]),
            [rect(10.0, 20.0, 90.0, 21.0), rect(10.0, 40.0, 90.0, 41.0)]
        );
    }

    #[test]
    fn a_place_known_to_hold_one_thing_is_one_body() {
        // Three lines of a listing that stays, too far apart to be joined by what is seen of them.
        let p = picture(&[(20, 20, 100, 24), (20, 50, 100, 54), (20, 80, 100, 84)]);
        assert_eq!(p.bodies(&[], &[]).len(), 3);
        assert_eq!(
            p.bodies(&[], &[rect(10.0, 10.0, 50.0, 42.0)]),
            [rect(10.0, 10.0, 50.5, 42.5)]
        );
    }

    #[test]
    fn the_paper_around_a_body_is_known_when_it_is_of_one_colour() {
        let p = picture(&[(40, 40, 100, 100)]);
        assert_eq!(p.paper(), [255; 3]);
        let body = p.bodies(&[], &[])[0];
        assert_eq!(p.paper_around(&body), Some([255; 3]));
        // Something stands right beside this box.
        assert_eq!(p.paper_around(&rect(20.0, 20.0, 49.0, 30.0)), None);
    }
}
