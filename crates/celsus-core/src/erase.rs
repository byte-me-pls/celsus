//! Taking the words out of a scanned picture, so that a translation can be written on clean paper.
//!
//! A box in the colour of the paper shows on paper that has a grain, a stain or a shadow: its edges and its flat
//! colour stand out. Here the ink itself is taken out of the picture instead. Inside each line, what is darker
//! than the paper around it is ink; it is filled from the paper that surrounds it, and the grain of the paper
//! nearby is laid over the fill, so that nothing marks where the words were.

use celsus_pdf::interp::{Matrix, apply, invert};
use celsus_pdf::{Document, ObjRef, object::get};

/// A scanned picture drawn on a page, open for taking words out of it.
pub struct Scan {
    pub id: ObjRef,
    /// From the page to the picture's unit square.
    from_page: Matrix,
    width: usize,
    height: usize,
    /// One byte a channel, one or three channels.
    channels: usize,
    pixels: Vec<u8>,
    /// The areas to clear, in pixels: x0, y0, x1, y1.
    areas: Vec<[usize; 4]>,
}

/// Pictures smaller than this, on either side, are no scan of a page.
const LEAST_SIDE: usize = 200;

impl Scan {
    /// Open the picture `id`, drawn with `ctm`, if it can be taken apart and put together again: eight bits a
    /// sample, grey or colour, with nothing that changes how its samples are read (a mask, a decode array).
    pub fn open(doc: &Document, id: ObjRef, ctm: &Matrix) -> Option<Scan> {
        let obj = doc.get(id).ok()?;
        let dict = &obj.as_stream()?.dict;
        for key in ["SMask", "Mask", "Decode", "ImageMask"] {
            if get(dict, key).is_some() {
                return None;
            }
        }
        if doc
            .get_in(dict, "BitsPerComponent")
            .and_then(|b| b.as_i64())
            != Some(8)
        {
            return None;
        }
        let from_page = invert(ctm)?;
        let (file, _) = celsus_pdf::image::export(doc, id)?;
        let picture = image::load_from_memory(&file).ok()?;
        let (channels, pixels) = if picture.color().has_color() {
            (3, picture.to_rgb8().into_raw())
        } else {
            (1, picture.to_luma8().into_raw())
        };
        let (width, height) = (picture.width() as usize, picture.height() as usize);
        (width >= LEAST_SIDE && height >= LEAST_SIDE).then_some(Scan {
            id,
            from_page,
            width,
            height,
            channels,
            pixels,
            areas: Vec::new(),
        })
    }

    /// The pixels under `r`, a rectangle of the page in its own space (x0, y0, x1, y1), if the picture holds
    /// it: its middle lies on the picture.
    fn under(&self, r: [f64; 4]) -> Option<[usize; 4]> {
        let corners = [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])]
            .map(|(x, y)| apply(&self.from_page, x, y));
        let (mx, my) = apply(&self.from_page, (r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
        if !(0.0..=1.0).contains(&mx) || !(0.0..=1.0).contains(&my) {
            return None;
        }
        // The picture's first row is its top: the unit square's y runs the other way.
        let px = |s: f64, size: usize| (s * size as f64).clamp(0.0, size as f64);
        let xs = corners.map(|(s, _)| px(s, self.width));
        let ys = corners.map(|(_, t)| px(1.0 - t, self.height));
        let lo = |v: [f64; 4]| v.iter().copied().fold(f64::MAX, f64::min).floor() as usize;
        let hi = |v: [f64; 4]| v.iter().copied().fold(f64::MIN, f64::max).ceil() as usize;
        let area = [
            lo(xs),
            lo(ys),
            hi(xs).min(self.width),
            hi(ys).min(self.height),
        ];
        (area[2] > area[0] && area[3] > area[1]).then_some(area)
    }

    /// Take the words under `r` (a rectangle of the page) out of the picture, if it holds them. Says whether
    /// it did.
    pub fn clear(&mut self, r: [f64; 4]) -> bool {
        match self.under(r) {
            Some(area) => {
                self.areas.push(area);
                true
            }
            None => false,
        }
    }

    /// Whether anything is to be taken out.
    pub fn changed(&self) -> bool {
        !self.areas.is_empty()
    }

    /// The picture with the words taken out, as a JPEG file.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        for area in std::mem::take(&mut self.areas) {
            erase(
                &mut self.pixels,
                self.width,
                self.height,
                self.channels,
                area,
            );
        }
        let mut out = Vec::new();
        let kind = if self.channels == 3 {
            image::ExtendedColorType::Rgb8
        } else {
            image::ExtendedColorType::L8
        };
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
            .encode(&self.pixels, self.width as u32, self.height as u32, kind)
            .ok()?;
        Some(out)
    }
}

/// Fill the ink inside `area` (x0, y0, x1, y1 in pixels) of a picture from the paper around it.
pub fn erase(pixels: &mut [u8], width: usize, height: usize, channels: usize, area: [usize; 4]) {
    let [x0, y0, x1, y1] = area;
    let line = y1.saturating_sub(y0).max(1);
    // Room around the area, for the paper the fill is taken from: two lines above and below.
    let room = (line * 2).max(8);
    let (a0, b0) = (x0.saturating_sub(room), y0.saturating_sub(room));
    let (a1, b1) = ((x1 + room).min(width), (y1 + room).min(height));
    let (w, h) = (a1 - a0, b1 - b0);
    if w < 3 || h < 3 {
        return;
    }
    let at = |x: usize, y: usize| ((b0 + y) * width + a0 + x) * channels;
    let light: Vec<u8> = (0..w * h)
        .map(|i| {
            let p = &pixels[at(i % w, i / w)..at(i % w, i / w) + channels];
            if channels == 3 {
                ((u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000)
                    as u8
            } else {
                p[0]
            }
        })
        .collect();
    // The paper's level at each pixel: the lightest nearby, over a reach wider than a stroke, softened.
    let reach = (line / 5).max(2);
    let paper = blur(&most(&light, w, h, reach), w, h, reach);
    // Paper is a little darker than its own lightest; ink is much darker.
    let mut below: Vec<u8> = light
        .iter()
        .zip(&paper)
        .map(|(&l, &p)| p.saturating_sub(l))
        .collect();
    below.sort_unstable();
    let grain = below[below.len() / 2];
    let spread = below[below.len() * 3 / 4].saturating_sub(grain);
    let apart = grain
        .saturating_add(spread.saturating_mul(3))
        .max(grain + 14);
    // Ink anywhere around, grown by the soft edge letters have: none of it is paper to fill from. What is taken
    // out is the ink inside the area.
    let dark: Vec<bool> = light
        .iter()
        .zip(&paper)
        .map(|(&l, &p)| p.saturating_sub(l) > apart)
        .collect();
    let edge = (line / 20).max(2);
    let taint = grow(&dark, w, h, edge);
    let inside = |i: usize| {
        let (x, y) = (i % w + a0, i / w + b0);
        x >= x0 && x < x1 && y >= y0 && y < y1
    };
    let in_area: Vec<bool> = dark
        .iter()
        .enumerate()
        .map(|(i, &d)| d && inside(i))
        .collect();
    let ink = grow(&in_area, w, h, edge);
    if !ink.iter().any(|&b| b) {
        return;
    }
    // What the paper is like under the ink: the mean of the paper around it, which follows a shadow or a stain,
    // and over that the grain of the paper a line above or below, which is paper of the same kind. A copy of
    // real paper, kept in one piece, looks like paper; noise made up pixel by pixel does not.
    let known: Vec<bool> = taint.iter().map(|&b| !b).collect();
    let value: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let p = at(i % w, i / w);
            let mut v = [0.0; 3];
            for c in 0..channels {
                v[c] = f32::from(pixels[p + c]);
            }
            v
        })
        .collect();
    let mean = paper_mean(&value, &known, w, h, (line / 2).max(reach * 2));
    let step = line as isize;
    let shifts = [
        (0, -step),
        (0, step),
        (0, -step * 3 / 2),
        (0, step * 3 / 2),
        (0, -2 * step),
        (0, 2 * step),
        (-step, 0),
        (step, 0),
        (-2 * step, 0),
        (2 * step, 0),
    ];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !ink[i] {
                continue;
            }
            let from = shifts.iter().find_map(|&(dx, dy)| {
                let (sx, sy) = (x as isize + dx, y as isize + dy);
                let j = (sx >= 0 && sy >= 0 && sx < w as isize && sy < h as isize)
                    .then(|| sy as usize * w + sx as usize)?;
                known[j].then_some(j)
            });
            let p = at(x, y);
            for c in 0..channels {
                let grain = from.map_or(0.0, |j| value[j][c] - mean[j][c]);
                pixels[p + c] = (mean[i][c] + grain).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// The mean colour of the pixels marked `known` within `reach` of each pixel; where none are, of those further.
fn paper_mean(
    value: &[[f32; 3]],
    known: &[bool],
    w: usize,
    h: usize,
    reach: usize,
) -> Vec<[f32; 3]> {
    // Tables of sums of the known pixels' channels, and of their number.
    let mut sums = vec![[0.0f64; 4]; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = [0.0f64; 4];
        for x in 0..w {
            let i = y * w + x;
            if known[i] {
                for c in 0..3 {
                    row[c] += f64::from(value[i][c]);
                }
                row[3] += 1.0;
            }
            let above = sums[y * (w + 1) + x + 1];
            sums[(y + 1) * (w + 1) + x + 1] = [0, 1, 2, 3].map(|c| above[c] + row[c]);
        }
    }
    let total = sums[h * (w + 1) + w];
    let overall = [0, 1, 2].map(|c| (total[c] / total[3].max(1.0)) as f32);
    let window = |x: usize, y: usize, r: usize| {
        let (t, b) = (y.saturating_sub(r), (y + r + 1).min(h));
        let (l, rr) = (x.saturating_sub(r), (x + r + 1).min(w));
        let s = |yy: usize, xx: usize| sums[yy * (w + 1) + xx];
        let (a, bb, cc, d) = (s(b, rr), s(t, l), s(t, rr), s(b, l));
        [0, 1, 2, 3].map(|c| a[c] + bb[c] - cc[c] - d[c])
    };
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            [reach, reach * 3, reach * 9]
                .iter()
                .map(|&r| window(x, y, r))
                .find(|s| s[3] >= 4.0)
                .map_or(overall, |s| [0, 1, 2].map(|c| (s[c] / s[3]) as f32))
        })
        .collect()
}

/// The largest value within `reach` of each pixel (a square), in two passes.
fn most(plane: &[u8], w: usize, h: usize, reach: usize) -> Vec<u8> {
    let mut across = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let (from, to) = (x.saturating_sub(reach), (x + reach).min(w - 1));
            across[y * w + x] = *plane[y * w + from..=y * w + to].iter().max().unwrap_or(&0);
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let (from, to) = (y.saturating_sub(reach), (y + reach).min(h - 1));
        for x in 0..w {
            out[y * w + x] = (from..=to).map(|yy| across[yy * w + x]).max().unwrap_or(0);
        }
    }
    out
}

/// The mean within `reach` of each pixel (a square), from a table of sums.
fn blur(plane: &[u8], w: usize, h: usize, reach: usize) -> Vec<u8> {
    let mut sums = vec![0u64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u64;
        for x in 0..w {
            row += u64::from(plane[y * w + x]);
            sums[(y + 1) * (w + 1) + x + 1] = sums[y * (w + 1) + x + 1] + row;
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let (t, b) = (y.saturating_sub(reach), (y + reach + 1).min(h));
        for x in 0..w {
            let (l, r) = (x.saturating_sub(reach), (x + reach + 1).min(w));
            let total = sums[b * (w + 1) + r] + sums[t * (w + 1) + l]
                - sums[t * (w + 1) + r]
                - sums[b * (w + 1) + l];
            out[y * w + x] = (total / ((b - t) * (r - l)) as u64) as u8;
        }
    }
    out
}

/// `mask` grown by `by` pixels on every side.
fn grow(mask: &[bool], w: usize, h: usize, by: usize) -> Vec<bool> {
    let plane: Vec<u8> = mask.iter().map(|&b| u8::from(b)).collect();
    most(&plane, w, h, by).into_iter().map(|v| v > 0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_is_taken_out_and_the_paper_kept() {
        // Grey paper with a grain, a gradient from left to right, and a black bar of "ink" across its middle.
        let (w, h) = (120, 60);
        let mut pixels: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                (200 + x / 6 + (x * 7 + y * 13) % 5) as u8
            })
            .collect();
        let paper = pixels.clone();
        for y in 25..35 {
            for x in 20..100 {
                pixels[y * w + x] = 20;
            }
        }
        erase(&mut pixels, w, h, 1, [15, 20, 105, 40]);
        // No ink is left, and each pixel is near what the paper there was.
        for y in 25..35 {
            for x in 20..100 {
                let (got, was) = (pixels[y * w + x], paper[y * w + x]);
                assert!(got.abs_diff(was) <= 8, "({x}, {y}): {got}, paper {was}");
            }
        }
        // Paper outside the ink is not touched.
        assert_eq!(pixels[5 * w + 5], paper[5 * w + 5]);
        assert_eq!(pixels[22 * w + 50], paper[22 * w + 50]);
    }
}
