//! Render pages to PNG with Pdfium, used only as an independent viewer to check output:
//! `cargo run --release -p celsus-core --example render_page -- file.pdf PAGE out.png [WIDTH]`

use pdfium_render::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("missing file.pdf")?;
    let n: i32 = args.next().ok_or("missing page")?.parse()?;
    let out = args.next().ok_or("missing out.png")?;
    let width: i32 = args.next().map(|w| w.parse()).transpose()?.unwrap_or(1400);
    let pdfium = Pdfium::new(Pdfium::bind_to_library(
        Pdfium::pdfium_platform_library_name_at_path("vendor/pdfium/bin"),
    )?);
    let doc = pdfium.load_pdf_from_file(&path, None)?;
    let page = doc.pages().get(n - 1)?;
    let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(width))?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(std::fs::File::create(&out)?),
        bitmap.width() as u32,
        bitmap.height() as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?
        .write_image_data(&bitmap.as_rgba_bytes())?;
    Ok(())
}
