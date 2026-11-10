//! Running a page's content to find where every glyph, line and image ends up.

use std::collections::HashMap;
use std::rc::Rc;

use crate::Result;
use crate::content::{self, Op};
use crate::document::{Document, Page};
use crate::font::Font;
use crate::object::{Dict, ObjRef, Object, get, numbers};

/// An affine matrix `[a b c d e f]` mapping `(x, y)` to `(a x + c y + e, b x + d y + f)`.
pub type Matrix = [f64; 6];

pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `m` followed by `n`.
pub fn multiply(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

/// The inverse of `m`, or `None` when it is singular.
pub fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return None;
    }
    Some([
        m[3] / det,
        -m[1] / det,
        -m[2] / det,
        m[0] / det,
        (m[2] * m[5] - m[3] * m[4]) / det,
        (m[1] * m[4] - m[0] * m[5]) / det,
    ])
}

pub fn apply(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// Where an operation lives: the page's own content or a form XObject drawn by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Page,
    Form(ObjRef),
}

/// A rectangle in default user space (PDF points, y up): `[x0, y0, x1, y1]`.
pub type Bounds = [f32; 4];

#[derive(Debug, Clone)]
pub struct PlacedGlyph {
    pub text: String,
    pub bounds: Bounds,
    /// Baseline origin in user space.
    pub origin: (f32, f32),
    /// The character code as it appears in the string.
    pub code: Vec<u8>,
    /// How far this glyph moves the text position, in unscaled text space (what a `TJ` number must undo).
    pub advance: f64,
    /// `TJ` adjustment (thousandths of an em) between the previous glyph and this one.
    pub kern_before: f64,
    /// Inside the clipping area. Clipped glyphs are kept so the operation can be rewritten exactly.
    pub visible: bool,
}

/// The glyphs drawn by one text-showing operation (`Tj`, `TJ`, `'` or `"`).
#[derive(Debug, Clone)]
pub struct TextRun {
    pub source: Source,
    /// Index of the operation in `PageContent::ops[source]`.
    pub op: usize,
    pub glyphs: Vec<PlacedGlyph>,
    /// `TJ` adjustment after the last glyph.
    pub trailing_kern: f64,
    /// The `Tf` size and horizontal scaling (1.0 = 100%) in effect, needed to rewrite the operation.
    pub font_size: f64,
    pub h_scale: f64,
    pub font: Rc<Font>,
    /// The font's name in the resource dictionary that was current.
    pub font_resource: Vec<u8>,
    /// Font size after all scaling: the height of an em in user space.
    pub size: f32,
    pub color: [u8; 3],
    /// Text render mode; 3 is invisible (OCR layers of scanned pages).
    pub render_mode: u8,
    /// Baseline direction in radians, 0 for normal horizontal text.
    pub angle: f32,
    /// The transformation matrix in effect, to draw new text at the same depth in the content.
    pub ctm: Matrix,
    /// Bounding box of the clipping path in effect: anything drawn at this depth shows only inside it.
    pub clip: Bounds,
}

#[derive(Debug, Clone, Copy)]
pub struct PathItem {
    pub bounds: Bounds,
    /// The subpath is an axis-aligned rectangle (a box or a rule), not a curve or a diagonal.
    pub is_rect: bool,
    pub stroked: bool,
    pub filled: bool,
    pub fill: [u8; 3],
    /// How many text runs had been drawn before it: a filled path covers the earlier text it lies on.
    pub after: usize,
}

#[derive(Debug, Default)]
pub struct PageContent {
    pub runs: Vec<TextRun>,
    pub paths: Vec<PathItem>,
    pub images: Vec<Bounds>,
    pub ops: HashMap<Source, Vec<Op>>,
    /// The decoded content each source's operations were parsed from.
    pub data: HashMap<Source, Vec<u8>>,
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    fill: [u8; 3],
    stroke: [u8; 3],
    line_width: f64,
    fill_components: usize,
    stroke_components: usize,
    font: Option<(Rc<Font>, Vec<u8>)>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
    render_mode: u8,
    /// Bounding box of the clipping path; glyphs outside it are not visible.
    clip: Bounds,
}

impl Default for GState {
    fn default() -> Self {
        GState {
            ctm: IDENTITY,
            fill: [0, 0, 0],
            stroke: [0, 0, 0],
            line_width: 1.0,
            fill_components: 1,
            stroke_components: 1,
            font: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
            clip: [f32::MIN, f32::MIN, f32::MAX, f32::MAX],
        }
    }
}

struct Interpreter<'d> {
    doc: &'d Document,
    out: PageContent,
    fonts: HashMap<FontKey, Rc<Font>>,
    depth: usize,
    /// Operators still allowed on this page. Forms can draw forms, so a few lines can ask for billions.
    budget: usize,
}

/// The most operators one page may run, forms included. Dense vector drawings stay well below this.
const MAX_OPERATORS: usize = if cfg!(test) { 200_000 } else { 20_000_000 };

/// How many names a colour space may pass through before it is given up on.
const MAX_ALIASES: usize = 8;

#[derive(Hash, PartialEq, Eq)]
enum FontKey {
    Ref(u32),
    Inline(Vec<u8>),
}

/// Interpret a page.
pub fn interpret(doc: &Document, page: &Page) -> Result<PageContent> {
    Ok(interpret_data(doc, page, doc.page_content(page)?))
}

/// Interpret `data` as the content of `page` (for example an edited version of it).
pub fn interpret_data(doc: &Document, page: &Page, data: Vec<u8>) -> PageContent {
    let mut it = Interpreter {
        doc,
        out: PageContent::default(),
        fonts: HashMap::new(),
        depth: 0,
        budget: MAX_OPERATORS,
    };
    let ops = content::parse(&data);
    let b = page.crop_box;
    let initial = GState {
        clip: [b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32],
        ..GState::default()
    };
    it.run(Source::Page, &ops, &page.resources, initial);
    it.out.ops.insert(Source::Page, ops);
    it.out.data.insert(Source::Page, data);
    it.out
}

fn num(ops: &[Object], i: usize) -> f64 {
    ops.get(i).and_then(Object::as_f64).unwrap_or(0.0)
}

/// Component count markers for colour spaces that are not gray, RGB or CMYK.
const TINT: usize = 100;
const OTHER: usize = 1000;

fn to_rgb(values: &[f64]) -> [u8; 3] {
    let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match values.len() {
        1 => [c(values[0]); 3],
        3 => [c(values[0]), c(values[1]), c(values[2])],
        4 => {
            let k = values[3];
            [
                c((1.0 - values[0]) * (1.0 - k)),
                c((1.0 - values[1]) * (1.0 - k)),
                c((1.0 - values[2]) * (1.0 - k)),
            ]
        }
        _ => [0, 0, 0],
    }
}

impl Interpreter<'_> {
    fn colorspace_components(&self, resources: &Dict, name: &[u8]) -> usize {
        self.colorspace_named(resources, name, 0)
    }

    fn colorspace_named(&self, resources: &Dict, name: &[u8], aliases: usize) -> usize {
        match name {
            b"DeviceGray" | b"G" | b"CalGray" => 1,
            b"Indexed" | b"I" | b"Pattern" => OTHER,
            b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => 3,
            b"DeviceCMYK" | b"CMYK" => 4,
            _ => {
                let space = self
                    .doc
                    .get_in(resources, "ColorSpace")
                    .and_then(|cs| {
                        cs.as_dict()
                            .and_then(|d| get(d, &String::from_utf8_lossy(name)).cloned())
                    })
                    .and_then(|o| self.doc.resolve(&o).ok());
                match space.as_deref() {
                    Some(Object::Array(a)) => match a.first().and_then(Object::as_name) {
                        Some(b"ICCBased") => a
                            .get(1)
                            .and_then(|s| self.doc.resolve(s).ok())
                            .and_then(|s| {
                                s.as_dict()
                                    .and_then(|d| get(d, "N"))
                                    .and_then(Object::as_i64)
                            })
                            .unwrap_or(3) as usize,
                        Some(b"CalRGB" | b"Lab") => 3,
                        Some(b"CalGray") => 1,
                        // Spot colours: values are ink tints, 1 meaning full ink.
                        Some(b"Separation") => TINT + 1,
                        Some(b"DeviceN") => {
                            TINT + a
                                .get(1)
                                .and_then(Object::as_array)
                                .map_or(1, <[Object]>::len)
                        }
                        _ => OTHER,
                    },
                    // A name can stand for another name, and a file can make that a circle.
                    Some(Object::Name(n)) if aliases < MAX_ALIASES => {
                        self.colorspace_named(resources, n, aliases + 1)
                    }
                    _ => 3,
                }
            }
        }
    }

    fn font(&mut self, resources: &Dict, name: &[u8]) -> Option<Rc<Font>> {
        let fonts = self.doc.get_in(resources, "Font")?;
        let entry = fonts.as_dict()?.get(name)?.clone();
        let key = match &entry {
            Object::Ref(r) => FontKey::Ref(r.num),
            _ => FontKey::Inline(name.to_vec()),
        };
        if let Some(f) = self.fonts.get(&key) {
            return Some(f.clone());
        }
        let dict = self.doc.resolve(&entry).ok()?;
        let font = Rc::new(Font::load(self.doc, dict.as_dict()?, entry.as_ref()));
        self.fonts.insert(key, font.clone());
        Some(font)
    }

    fn run(&mut self, source: Source, ops: &[Op], resources: &Dict, initial: GState) {
        let mut gs = initial;
        let mut stack: Vec<GState> = Vec::new();
        let mut tm = IDENTITY;
        let mut tlm = IDENTITY;
        let mut path: Vec<(f64, f64)> = Vec::new();
        let mut subpaths: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut pending_clip = false;

        for (index, op) in ops.iter().enumerate() {
            if self.budget == 0 {
                return;
            }
            self.budget -= 1;
            let o = &op.operands;
            match op.operator.as_slice() {
                b"q" => {
                    if stack.len() < 256 {
                        stack.push(gs.clone());
                    }
                }
                b"Q" => {
                    if let Some(s) = stack.pop() {
                        gs = s;
                    }
                }
                b"cm" if o.len() == 6 => {
                    let m = [
                        num(o, 0),
                        num(o, 1),
                        num(o, 2),
                        num(o, 3),
                        num(o, 4),
                        num(o, 5),
                    ];
                    gs.ctm = multiply(&m, &gs.ctm);
                }
                b"w" => gs.line_width = num(o, 0),
                // Colour.
                b"g" => (gs.fill, gs.fill_components) = (to_rgb(&[num(o, 0)]), 1),
                b"G" => (gs.stroke, gs.stroke_components) = (to_rgb(&[num(o, 0)]), 1),
                b"rg" => {
                    (gs.fill, gs.fill_components) = (to_rgb(&[num(o, 0), num(o, 1), num(o, 2)]), 3)
                }
                b"RG" => {
                    (gs.stroke, gs.stroke_components) =
                        (to_rgb(&[num(o, 0), num(o, 1), num(o, 2)]), 3)
                }
                b"k" => {
                    (gs.fill, gs.fill_components) =
                        (to_rgb(&[num(o, 0), num(o, 1), num(o, 2), num(o, 3)]), 4)
                }
                b"K" => {
                    (gs.stroke, gs.stroke_components) =
                        (to_rgb(&[num(o, 0), num(o, 1), num(o, 2), num(o, 3)]), 4)
                }
                b"cs" | b"CS" => {
                    let n = o
                        .first()
                        .and_then(Object::as_name)
                        .map_or(1, |n| self.colorspace_components(resources, n));
                    // Every colour space starts at black (gray 0, CMYK 0 0 0 1, full ink).
                    let black = [0, 0, 0];
                    if op.operator == b"cs" {
                        (gs.fill, gs.fill_components) = (black, n);
                    } else {
                        (gs.stroke, gs.stroke_components) = (black, n);
                    }
                }
                b"sc" | b"scn" | b"SC" | b"SCN" => {
                    let values: Vec<f64> = o.iter().filter_map(Object::as_f64).collect();
                    let fill = op.operator[0] == b's';
                    let n = if fill {
                        gs.fill_components
                    } else {
                        gs.stroke_components
                    };
                    let rgb = if n >= OTHER {
                        Some([0, 0, 0])
                    } else if n >= TINT {
                        // Approximate spot inks as black at the strongest tint.
                        let ink = values.iter().copied().fold(0.0f64, f64::max);
                        Some(to_rgb(&[1.0 - ink]))
                    } else if values.len() >= n && matches!(n, 1 | 3 | 4) {
                        Some(to_rgb(&values[..n]))
                    } else {
                        None
                    };
                    if let Some(rgb) = rgb {
                        if fill {
                            gs.fill = rgb;
                        } else {
                            gs.stroke = rgb;
                        }
                    }
                }
                // Text state.
                b"BT" => {
                    tm = IDENTITY;
                    tlm = IDENTITY;
                }
                b"Tf" => {
                    gs.font = o
                        .first()
                        .and_then(Object::as_name)
                        .and_then(|n| self.font(resources, n).map(|f| (f, n.to_vec())));
                    gs.font_size = num(o, 1);
                }
                b"Tc" => gs.char_spacing = num(o, 0),
                b"Tw" => gs.word_spacing = num(o, 0),
                b"Tz" => gs.h_scale = num(o, 0) / 100.0,
                b"TL" => gs.leading = num(o, 0),
                b"Ts" => gs.rise = num(o, 0),
                b"Tr" => gs.render_mode = num(o, 0).clamp(0.0, 7.0) as u8,
                b"Td" => {
                    tlm = multiply(&[1.0, 0.0, 0.0, 1.0, num(o, 0), num(o, 1)], &tlm);
                    tm = tlm;
                }
                b"TD" => {
                    gs.leading = -num(o, 1);
                    tlm = multiply(&[1.0, 0.0, 0.0, 1.0, num(o, 0), num(o, 1)], &tlm);
                    tm = tlm;
                }
                b"Tm" if o.len() == 6 => {
                    tlm = [
                        num(o, 0),
                        num(o, 1),
                        num(o, 2),
                        num(o, 3),
                        num(o, 4),
                        num(o, 5),
                    ];
                    tm = tlm;
                }
                b"T*" => {
                    tlm = multiply(&[1.0, 0.0, 0.0, 1.0, 0.0, -gs.leading], &tlm);
                    tm = tlm;
                }
                b"Tj" => {
                    if let Some(s) = o.first().and_then(Object::as_str_bytes) {
                        let mut glyphs = Vec::new();
                        self.show(&gs, &mut tm, s, &mut glyphs, &mut 0.0);
                        self.push_run(source, index, &gs, &tm, glyphs, 0.0);
                    }
                }
                b"'" | b"\"" => {
                    if op.operator == b"\"" {
                        gs.word_spacing = num(o, 0);
                        gs.char_spacing = num(o, 1);
                    }
                    tlm = multiply(&[1.0, 0.0, 0.0, 1.0, 0.0, -gs.leading], &tlm);
                    tm = tlm;
                    if let Some(s) = o.last().and_then(Object::as_str_bytes) {
                        let mut glyphs = Vec::new();
                        self.show(&gs, &mut tm, s, &mut glyphs, &mut 0.0);
                        self.push_run(source, index, &gs, &tm, glyphs, 0.0);
                    }
                }
                b"TJ" => {
                    let mut glyphs = Vec::new();
                    let mut kern = 0.0;
                    for item in o.first().and_then(Object::as_array).unwrap_or_default() {
                        if let Object::String(s) = item {
                            self.show(&gs, &mut tm, s, &mut glyphs, &mut kern);
                        } else if let Some(n) = item.as_f64() {
                            let tx = -n / 1000.0 * gs.font_size * gs.h_scale;
                            tm = multiply(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], &tm);
                            kern += n;
                        }
                    }
                    self.push_run(source, index, &gs, &tm, glyphs, kern);
                }
                // Paths: only their extent matters (table rules, boxes, backgrounds).
                b"m" => {
                    if path.len() > 1 {
                        subpaths.push(std::mem::take(&mut path));
                    }
                    path = vec![apply(&gs.ctm, num(o, 0), num(o, 1))];
                }
                b"l" => path.push(apply(&gs.ctm, num(o, 0), num(o, 1))),
                b"c" => {
                    path.push(apply(&gs.ctm, num(o, 4), num(o, 5)));
                }
                b"v" | b"y" => path.push(apply(&gs.ctm, num(o, 2), num(o, 3))),
                b"re" => {
                    if path.len() > 1 {
                        subpaths.push(std::mem::take(&mut path));
                    }
                    let (x, y, w, h) = (num(o, 0), num(o, 1), num(o, 2), num(o, 3));
                    subpaths.push(vec![
                        apply(&gs.ctm, x, y),
                        apply(&gs.ctm, x + w, y),
                        apply(&gs.ctm, x + w, y + h),
                        apply(&gs.ctm, x, y + h),
                    ]);
                    path.clear();
                }
                b"h" => {}
                b"W" | b"W*" => pending_clip = true,
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
                    if path.len() > 1 {
                        subpaths.push(std::mem::take(&mut path));
                    }
                    let name = op.operator.as_slice();
                    let stroked = matches!(name, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*");
                    let filled = matches!(name, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*");
                    if stroked || filled {
                        let [a, b, c, d, ..] = gs.ctm;
                        let pen = gs.line_width * (a * d - b * c).abs().sqrt();
                        for sp in &subpaths {
                            let (mut bounds, is_rect) = (bounds_of(sp), is_axis_rect(sp));
                            // A rule drawn with a broad pen is a bar: the label on it stands inside a box.
                            if stroked && !filled && is_rect && pen > 4.0 {
                                let half = (pen / 2.0) as f32;
                                let (w, h) = (bounds[2] - bounds[0], bounds[3] - bounds[1]);
                                if h <= w {
                                    bounds[1] -= half;
                                    bounds[3] += half;
                                }
                                if w <= h {
                                    bounds[0] -= half;
                                    bounds[2] += half;
                                }
                                self.out.paths.push(PathItem {
                                    bounds,
                                    is_rect,
                                    stroked: false,
                                    filled: true,
                                    fill: gs.stroke,
                                    after: self.out.runs.len(),
                                });
                                continue;
                            }
                            self.out.paths.push(PathItem {
                                bounds,
                                is_rect,
                                stroked,
                                filled,
                                fill: gs.fill,
                                after: self.out.runs.len(),
                            });
                        }
                    }
                    if pending_clip {
                        let all: Vec<(f64, f64)> = subpaths.iter().flatten().copied().collect();
                        if !all.is_empty() {
                            gs.clip = intersect(gs.clip, bounds_of(&all));
                        }
                        pending_clip = false;
                    }
                    subpaths.clear();
                    path.clear();
                }
                b"Do" => {
                    if let Some(name) = o.first().and_then(Object::as_name) {
                        self.draw_xobject(resources, name, &gs);
                    }
                }
                b"BI" => {
                    let corners: Vec<(f64, f64)> = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
                        .iter()
                        .map(|&(x, y)| apply(&gs.ctm, x, y))
                        .collect();
                    self.out.images.push(bounds_of(&corners));
                }
                _ => {}
            }
        }
    }

    fn draw_xobject(&mut self, resources: &Dict, name: &[u8], gs: &GState) {
        let Some(xobjects) = self.doc.get_in(resources, "XObject") else {
            return;
        };
        let Some(entry) = xobjects.as_dict().and_then(|d| d.get(name)).cloned() else {
            return;
        };
        let Ok(obj) = self.doc.resolve(&entry) else {
            return;
        };
        let Some(stream) = obj.as_stream() else {
            return;
        };
        match get(&stream.dict, "Subtype").and_then(Object::as_name) {
            Some(b"Image") => {
                let corners: Vec<(f64, f64)> = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
                    .iter()
                    .map(|&(x, y)| apply(&gs.ctm, x, y))
                    .collect();
                self.out.images.push(bounds_of(&corners));
            }
            Some(b"Form") => {
                let Some(id) = entry.as_ref() else { return };
                if self.depth >= 12 || self.budget == 0 {
                    return;
                }
                let matrix = get(&stream.dict, "Matrix")
                    .and_then(numbers)
                    .filter(|m| m.len() == 6);
                let mut inner = gs.clone();
                if let Some(m) = matrix {
                    inner.ctm = multiply(&[m[0], m[1], m[2], m[3], m[4], m[5]], &gs.ctm);
                }
                if let Some(b) = get(&stream.dict, "BBox")
                    .and_then(numbers)
                    .filter(|b| b.len() == 4)
                {
                    let corners: Vec<(f64, f64)> =
                        [(b[0], b[1]), (b[2], b[1]), (b[2], b[3]), (b[0], b[3])]
                            .iter()
                            .map(|&(x, y)| apply(&inner.ctm, x, y))
                            .collect();
                    inner.clip = intersect(inner.clip, bounds_of(&corners));
                }
                let form_resources = self
                    .doc
                    .get_in(&stream.dict, "Resources")
                    .and_then(|r| r.as_dict().cloned());
                let resources = form_resources.as_ref().unwrap_or(resources).clone();
                let source = Source::Form(id);
                let ops = match self.out.ops.get(&source) {
                    Some(ops) => ops.clone(),
                    None => match self.doc.stream_data(stream) {
                        Ok(data) => {
                            let ops = content::parse(&data);
                            self.out.data.insert(source, data);
                            ops
                        }
                        Err(_) => return,
                    },
                };
                self.depth += 1;
                self.run(source, &ops, &resources, inner);
                self.depth -= 1;
                self.out.ops.entry(source).or_insert(ops);
            }
            _ => {}
        }
    }

    /// Lay out the glyphs of one string, advancing the text matrix.
    fn show(
        &self,
        gs: &GState,
        tm: &mut Matrix,
        bytes: &[u8],
        out: &mut Vec<PlacedGlyph>,
        kern: &mut f64,
    ) {
        let Some((font, _)) = &gs.font else { return };
        let (fs, th) = (gs.font_size, gs.h_scale);
        let (asc, desc) = (f64::from(font.ascent), f64::from(font.descent));
        let mut at = 0;
        for g in font.decode(bytes) {
            let code = bytes[at.min(bytes.len())..(at + g.len).min(bytes.len())].to_vec();
            at += g.len;
            let w = f64::from(g.width);
            let trm = multiply(
                &multiply(&[fs * th, 0.0, 0.0, fs, 0.0, gs.rise], tm),
                &gs.ctm,
            );
            let corners = [
                apply(&trm, 0.0, desc),
                apply(&trm, w, desc),
                apply(&trm, w, asc),
                apply(&trm, 0.0, asc),
            ];
            let (ox, oy) = apply(&trm, 0.0, 0.0);
            let bounds = bounds_of(&corners);
            let (cx, cy) = ((bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0);
            let c = gs.clip;
            let visible =
                cx >= c[0] - 1.0 && cx <= c[2] + 1.0 && cy >= c[1] - 1.0 && cy <= c[3] + 1.0;
            let spacing = gs.char_spacing
                + if g.is_space_code {
                    gs.word_spacing
                } else {
                    0.0
                };
            let advance = if font.vertical {
                let ty = -(w.max(1.0) * fs) - spacing;
                *tm = multiply(&[1.0, 0.0, 0.0, 1.0, 0.0, ty], tm);
                ty
            } else {
                let tx = (w * fs + spacing) * th;
                *tm = multiply(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], tm);
                tx
            };
            out.push(PlacedGlyph {
                text: g.text,
                bounds,
                origin: (ox as f32, oy as f32),
                code,
                advance,
                kern_before: std::mem::take(kern),
                visible,
            });
        }
    }

    fn push_run(
        &mut self,
        source: Source,
        op: usize,
        gs: &GState,
        tm: &Matrix,
        glyphs: Vec<PlacedGlyph>,
        trailing_kern: f64,
    ) {
        let Some((font, resource)) = &gs.font else {
            return;
        };
        if glyphs.is_empty() {
            return;
        }
        let m = multiply(tm, &gs.ctm);
        let size = (gs.font_size * (m[2] * m[2] + m[3] * m[3]).sqrt()).abs() as f32;
        let angle = (m[1].atan2(m[0])) as f32;
        let color = if matches!(gs.render_mode, 1 | 5) {
            gs.stroke
        } else {
            gs.fill
        };
        self.out.runs.push(TextRun {
            clip: gs.clip,
            source,
            op,
            glyphs,
            trailing_kern,
            font_size: gs.font_size,
            h_scale: gs.h_scale,
            font: font.clone(),
            font_resource: resource.clone(),
            size,
            color,
            render_mode: gs.render_mode,
            angle,
            ctm: gs.ctm,
        });
    }
}

/// Whether a subpath only runs along horizontal and vertical edges.
fn is_axis_rect(points: &[(f64, f64)]) -> bool {
    points.len() >= 2
        && points.len() <= 6
        && points
            .windows(2)
            .chain(std::iter::once(&[points[points.len() - 1], points[0]][..]))
            .all(|w| (w[0].0 - w[1].0).abs() < 0.5 || (w[0].1 - w[1].1).abs() < 0.5)
}

fn intersect(a: Bounds, b: Bounds) -> Bounds {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}

fn bounds_of(points: &[(f64, f64)]) -> Bounds {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for &(x, y) in points {
        b[0] = b[0].min(x as f32);
        b[1] = b[1].min(y as f32);
        b[2] = b[2].max(x as f32);
        b[3] = b[3].max(y as f32);
    }
    b
}
