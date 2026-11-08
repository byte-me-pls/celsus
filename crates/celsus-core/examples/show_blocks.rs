//! Draw the blocks Celsus finds on top of a page image, to check the layout by eye.
//!
//!     cargo run --release -p celsus-core --example show_blocks -- book.pdf PAGE out.png [--pdfium DIR] [--json]
//!
//! Blue: translatable block, red: protected, green: rules and boxes that separate blocks.

use std::path::PathBuf;

use celsus_core::extract;
use pdfium_render::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().ok_or("missing book.pdf")?);
    let n: usize = args.next().ok_or("missing page")?.parse()?;
    let out = PathBuf::from(args.next().ok_or("missing out.png")?);
    let (mut pdfium_dir, mut json) = (None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--pdfium" => pdfium_dir = args.next().map(PathBuf::from),
            "--json" => json = true,
            _ => return Err(format!("unexpected argument {a}").into()),
        }
    }

    let doc = celsus_pdf::Document::open(&path)?;
    let pages = doc.pages()?;
    let page = &pages[n - 1];
    let t = std::time::Instant::now();
    let extracted = extract::extract_page(&doc, page, n as u32)?;
    let elapsed = t.elapsed();
    for b in &extracted.layout.blocks {
        let text = b.source();
        let short: String = text.chars().take(70).collect();
        println!(
            "{} {:?} {:.1}pt{}{} {:?} [{:.0},{:.0} {:.0}x{:.0}] {short}",
            b.id.0,
            b.align,
            b.style.font_size,
            if b.style.bold { " bold" } else { "" },
            if b.protected { " PROTECTED" } else { "" },
            b.lines.len(),
            b.rect.x0,
            b.rect.y0,
            b.rect.width(),
            b.rect.height()
        );
    }
    println!("{} blocks in {elapsed:?}", extracted.layout.blocks.len());
    if json {
        println!("{}", serde_json::to_string_pretty(&extracted.layout)?);
    }

    let pdfium = Pdfium::new(Pdfium::bind_to_library(
        Pdfium::pdfium_platform_library_name_at_path(
            pdfium_dir
                .as_deref()
                .unwrap_or(std::path::Path::new("vendor/pdfium/bin")),
        ),
    )?);
    let document = pdfium.load_pdf_from_file(&path, None)?;
    let p = document.pages().get(n as i32 - 1)?;
    let bitmap = p.render_with_config(&PdfRenderConfig::new().set_target_width(1400))?;
    let (w, h) = (bitmap.width() as usize, bitmap.height() as usize);
    let mut px = bitmap.as_rgba_bytes();
    let scale = w as f32 / extracted.layout.width;
    let mut rect = |r: &celsus_core::Rect, color: [u8; 3], thick: usize| {
        let (x0, y0) = ((r.x0 * scale) as isize - 2, (r.y0 * scale) as isize - 2);
        let (x1, y1) = ((r.x1 * scale) as isize + 2, (r.y1 * scale) as isize + 2);
        let mut put = |x: isize, y: isize| {
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                let i = (y as usize * w + x as usize) * 4;
                px[i..i + 3].copy_from_slice(&color);
            }
        };
        for t in 0..thick as isize {
            for x in x0..=x1 {
                put(x, y0 + t);
                put(x, y1 - t);
            }
            for y in y0..=y1 {
                put(x0 + t, y);
                put(x1 - t, y);
            }
        }
    };
    for b in &extracted.layout.blocks {
        rect(
            &b.rect,
            if b.protected {
                [220, 30, 30]
            } else {
                [30, 90, 230]
            },
            2,
        );
    }
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(std::fs::File::create(&out)?),
        w as u32,
        h as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&px)?;
    Ok(())
}
