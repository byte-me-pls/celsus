//! Reading the words off scanned pages that carry no text layer.
//!
//! A page that is only a picture of a page has nothing for Celsus to translate. Here its picture is drawn and
//! read with PaddleOCR's PP-OCRv6 models (run in pure Rust): the lines that come out are laid on the page as the
//! invisible text a scan's text layer would be (see [`celsus_pdf::Document::set_read_lines`]), and the rest of
//! the work treats the page as a scan that carries its text.
//!
//! The models are not part of the program: they are fetched once, when the user agrees, from the PaddlePaddle
//! repositories they are published in, and checked against the sums below.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use celsus_pdf::interp::ScanLine;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
use pure_onnx_ocr::{DetLimitType, OcrEngine, OcrEngineBuilder};
use sha2::{Digest, Sha256};

/// The folder of the models, under Celsus's data folder.
pub const MODEL: &str = "ppocrv6-small";

/// One file of the models.
pub struct ModelFile {
    /// Where it goes, inside the models' folder.
    pub path: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// PP-OCRv6 small: text detection and recognition for about fifty languages, under the Apache 2.0 licence.
pub const FILES: [ModelFile; 4] = [
    ModelFile {
        path: "det/inference.onnx",
        url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/main/inference.onnx",
        size: 9_880_512,
        sha256: "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
    },
    ModelFile {
        path: "det/inference.yml",
        url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/main/inference.yml",
        size: 885,
        sha256: "193f435274bf9f0b5f71a929bbfbcf148282df7e633b34e7c373e8f44741b516",
    },
    ModelFile {
        path: "rec/inference.onnx",
        url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/main/inference.onnx",
        size: 21_159_378,
        sha256: "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
    },
    ModelFile {
        path: "rec/inference.yml",
        url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/main/inference.yml",
        size: 150_579,
        sha256: "ab078671bb49f06228eadccd34f1bb501e157f7a047095ffb943ba81512c77d1",
    },
];

/// How much there is to fetch, in bytes.
pub fn download_size() -> u64 {
    FILES.iter().map(|f| f.size).sum()
}

/// The models' folder inside `data_dir`.
pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join(MODEL)
}

/// Whether the models are in `dir`, whole.
pub fn ready(dir: &Path) -> bool {
    FILES.iter().all(|f| {
        std::fs::metadata(dir.join(f.path))
            .map(|m| m.len() == f.size)
            .unwrap_or(false)
    })
}

/// Fetch the models into `dir`. Each file is written next to its place and moved there only once its sum is
/// right, so a fetch cut short leaves nothing half there. `progress` is told the bytes done and the total, and
/// says whether to go on: the error is then "cancelled". A connection that stays silent for a minute ends it.
pub fn download(dir: &Path, mut progress: impl FnMut(u64, u64) -> bool) -> Result<(), String> {
    let total = download_size();
    let mut done = 0u64;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(60)))
        // The body of the largest file, at a slow line, still arrives within this.
        .timeout_recv_body(Some(std::time::Duration::from_secs(30 * 60)))
        .build()
        .into();
    for f in &FILES {
        let path = dir.join(f.path);
        if std::fs::metadata(&path)
            .map(|m| m.len() == f.size)
            .unwrap_or(false)
        {
            done += f.size;
            if !progress(done, total) {
                return Err("cancelled".into());
            }
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let part = path.with_extension("part");
        let mut answer = agent
            .get(f.url)
            .call()
            .map_err(|e| format!("{}: {e}", f.url))?;
        let mut body = answer.body_mut().as_reader();
        let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut got = 0u64;
        loop {
            let n = body.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            got += n as u64;
            if got > f.size {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            if !progress(done + got, total) {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err("cancelled".into());
            }
        }
        drop(file);
        let sum: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if got != f.size || sum != f.sha256 {
            let _ = std::fs::remove_file(&part);
            return Err(format!("{} did not arrive whole", f.path));
        }
        std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
        done += f.size;
    }
    Ok(())
}

/// Lines read with less certainty than this are left out: what remains of a smudge or a drawing.
const LEAST_CONFIDENCE: f32 = 0.5;
/// Pages are drawn at about this many pixels an inch for reading, and never larger than `MOST_SIDE`.
const DPI: f32 = 200.0;
const MOST_SIDE: f32 = 2400.0;

/// The OCR models, ready to read pages.
pub struct Reader {
    engine: OcrEngine,
}

impl Reader {
    /// Load the models from `dir` (see [`models_dir`]).
    pub fn open(dir: &Path) -> Result<Reader, String> {
        if !ready(dir) {
            return Err("ocr_models_missing".into());
        }
        let engine = OcrEngineBuilder::new()
            .det_model_dir(dir.join("det"))
            .rec_model_dir(dir.join("rec"))
            .det_limit_type(DetLimitType::Max)
            .det_limit_side_len(1600)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Reader { engine })
    }

    /// Read pages of the PDF `file`, given by number from 1 with their crop box (`[x0, y0, x1, y1]`, PDF points)
    /// and rotation, handing each page's lines to `each` as it is read; it says whether to go on. The lines come
    /// in default user space, as a text layer would. Turned pages are not read.
    pub fn read(
        &self,
        file: &[u8],
        pages: &[(usize, [f64; 4], i32)],
        mut each: impl FnMut(usize, Vec<ScanLine>) -> bool,
    ) -> Result<(), String> {
        let pdf = Pdf::new(Arc::new(file.to_vec())).map_err(|e| format!("{e:?}"))?;
        let all = pdf.pages();
        let mut out = |number, lines| each(number, lines);
        for &(number, crop, rotate) in pages {
            let Some(page) = all.get(number.wrapping_sub(1)) else {
                continue;
            };
            if rotate.rem_euclid(360) != 0 {
                if !out(number, Vec::new()) {
                    break;
                }
                continue;
            }
            let (w, h) = page.render_dimensions();
            if !(w >= 1.0 && h >= 1.0 && w < 20_000.0 && h < 20_000.0) {
                if !out(number, Vec::new()) {
                    break;
                }
                continue;
            }
            let scale = (DPI / 72.0).min(MOST_SIDE / w.max(h));
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
            let png = pixmap.into_png().map_err(|e| format!("{e:?}"))?;
            let found = self
                .engine
                .run_from_bytes(&png)
                .map_err(|e| e.to_string())?;
            let (cx0, cy1) = (crop[0] as f32, crop[3] as f32);
            let mut lines: Vec<ScanLine> = found
                .into_iter()
                .filter(|r| r.confidence >= LEAST_CONFIDENCE && !r.text.trim().is_empty())
                .filter_map(|r| {
                    let pts: Vec<(f64, f64)> = r
                        .bounding_box
                        .exterior()
                        .points()
                        .map(|p| (p.x(), p.y()))
                        .collect();
                    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                    for (x, y) in pts {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x);
                        y1 = y1.max(y);
                    }
                    (x1 > x0 && y1 > y0).then(|| ScanLine {
                        text: r.text.trim().to_owned(),
                        // Pixels from the top left to points from the bottom left.
                        bounds: [
                            cx0 + x0 as f32 / scale,
                            cy1 - y1 as f32 / scale,
                            cx0 + x1 as f32 / scale,
                            cy1 - y0 as f32 / scale,
                        ],
                    })
                })
                .collect();
            // In reading order: top to bottom, then left to right.
            lines.sort_by(|a, b| {
                b.bounds[3]
                    .total_cmp(&a.bounds[3])
                    .then(a.bounds[0].total_cmp(&b.bounds[0]))
            });
            if !out(number, lines) {
                break;
            }
        }
        Ok(())
    }
}

/// The pages of book `id` in `library` among `pages` (every page when empty) that are only scanned pictures and
/// have not been read yet. Each page is looked at once: what was found is kept with the book.
pub fn unread_pages(library: &Path, id: i64, pages: &[usize]) -> Result<Vec<usize>, String> {
    let mut scans = celsus_store::library::scans(library, id);
    let doc = celsus_pdf::Document::open(celsus_store::library::source_file(library, id))
        .map_err(|e| e.to_string())?;
    let all = doc.pages().map_err(|e| e.to_string())?;
    let wanted: Vec<usize> = if pages.is_empty() {
        (1..=all.len()).collect()
    } else {
        pages
            .iter()
            .copied()
            .filter(|&n| n >= 1 && n <= all.len())
            .collect()
    };
    let mut out = Vec::new();
    let mut changed = false;
    for n in wanted {
        if scans.read.contains_key(&n) || scans.with_text.contains(&n) {
            continue;
        }
        if celsus_core::extract::picture_only(&doc, &all[n - 1]) {
            out.push(n);
        } else {
            scans.with_text.insert(n);
            changed = true;
        }
    }
    if changed {
        celsus_store::library::keep_scans(library, id, &scans)?;
    }
    Ok(out)
}

/// Read `pages` of book `id` and keep their lines with it, page by page, so that reading cut short loses
/// nothing read. `go_on` is told the pages read so far and how many there are, and says whether to go on.
pub fn read_pages(
    library: &Path,
    id: i64,
    pages: &[usize],
    reader: &Reader,
    mut go_on: impl FnMut(usize, usize) -> bool,
) -> Result<(), String> {
    if pages.is_empty() {
        return Ok(());
    }
    let source = celsus_store::library::source_file(library, id);
    let file = std::fs::read(&source).map_err(|e| e.to_string())?;
    let doc = celsus_pdf::Document::open(&source).map_err(|e| e.to_string())?;
    let all = doc.pages().map_err(|e| e.to_string())?;
    let todo: Vec<(usize, [f64; 4], i32)> = pages
        .iter()
        .filter_map(|&n| {
            all.get(n.wrapping_sub(1))
                .map(|p| (n, p.crop_box, p.rotate))
        })
        .collect();
    let total = todo.len();
    if !go_on(0, total) {
        return Ok(());
    }
    let mut done = 0;
    let mut failed = None;
    reader.read(&file, &todo, |n, lines| {
        let mut scans = celsus_store::library::scans(library, id);
        scans.read.insert(
            n,
            lines
                .into_iter()
                .map(|l| celsus_store::library::ReadLine {
                    text: l.text,
                    bounds: l.bounds,
                })
                .collect(),
        );
        if let Err(e) = celsus_store::library::keep_scans(library, id, &scans) {
            failed = Some(e);
            return false;
        }
        done += 1;
        go_on(done, total)
    })?;
    failed.map_or(Ok(()), Err)
}
