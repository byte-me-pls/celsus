//! Write translations into a PDF without any model, to try the writer:
//!
//!     cargo run --release -p celsus-core --example translate_pdf -- in.pdf out.pdf [--pages 1-3] [--map map.json]
//!         [--fonts C:\Windows\Fonts] [--to LANGUAGE] [--grow PERCENT] [--no-inline]
//!
//! Without `--map`, every translatable block gets a stand-in translation with Turkish letters that is about 15%
//! longer than the source, which is typical for English to Turkish. A map is `{"1": {"b0001": "...", ...}}` keyed
//! by page number. `--no-inline` leaves out the tags around words printed in another style, so the whole of a
//! block comes out in its own.

use std::collections::BTreeMap;
use std::path::PathBuf;

use celsus_core::BlockId;
use celsus_core::fonts::FontSet;
use celsus_core::render::BookWriter;

/// A block that keeps its lines grows in each of them, as a list of entries does when it is translated.
fn stand_in(source: &str, grow: usize) -> String {
    source
        .split('\n')
        .map(|line| stand_in_line(line, grow))
        .collect::<Vec<_>>()
        .join("\n")
}

fn stand_in_line(source: &str, grow: usize) -> String {
    let letters = |text: &str| -> String {
        text.chars()
            .map(|c| match c {
                'c' => 'ç',
                's' => 'ş',
                'g' => 'ğ',
                'i' => 'ı',
                'o' => 'ö',
                'u' => 'ü',
                'C' => 'Ç',
                'S' => 'Ş',
                'I' => 'İ',
                c => c,
            })
            .collect()
    };
    // The tags around words in another style stay as they are.
    let mut out = celsus_core::inline::map_text(source, letters, true);
    let extra = celsus_core::inline::without_tags(source).chars().count() * grow / 100;
    if extra > 0 {
        // A contents entry grows in its title, not behind its page number.
        let filler: String = "ışığı ".chars().cycle().take(extra).collect();
        match celsus_core::layout::leader_span(&out) {
            Some(span) => {
                let gap = if out[..span.start].ends_with(' ') { "" } else { " " };
                out.insert_str(span.start, &format!("{gap}{} ", filler.trim_end()));
            }
            None => {
                out.push(' ');
                out.push_str(&filler);
            }
        }
    }
    out.trim_end().to_owned()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut input, mut output, mut pages, mut map) = (None, None, None, None);
    let mut fonts_dir = PathBuf::from(if cfg!(windows) {
        r"C:\Windows\Fonts"
    } else {
        "/usr/share/fonts/truetype"
    });
    let mut to: celsus_core::Language = "tr".parse()?;
    let mut grow = 15;
    let mut regions: Option<PathBuf> = None;
    let mut inline = true;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-inline" => inline = false,
            "--pages" => pages = args.next(),
            "--map" => map = args.next().map(PathBuf::from),
            "--fonts" => {
                fonts_dir = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--fonts needs a directory")?
            }
            "--to" => to = args.next().ok_or("--to needs a language")?.parse()?,
            "--regions" => regions = args.next().map(PathBuf::from),
            "--grow" => grow = args.next().ok_or("--grow needs a percentage")?.parse()?,
            _ if input.is_none() => input = Some(PathBuf::from(a)),
            _ if output.is_none() => output = Some(PathBuf::from(a)),
            _ => return Err(format!("unexpected argument {a}").into()),
        }
    }
    let (input, output) = (
        input.ok_or("missing in.pdf")?,
        output.ok_or("missing out.pdf")?,
    );
    let fonts = FontSet::for_language(&fonts_dir, &to);
    if fonts.is_empty() {
        return Err(format!("no usable fonts in {}", fonts_dir.display()).into());
    }
    let map: BTreeMap<u32, BTreeMap<BlockId, String>> = match map {
        Some(path) => {
            let raw: BTreeMap<String, BTreeMap<String, String>> =
                serde_json::from_slice(&std::fs::read(path)?)?;
            let mut out = BTreeMap::new();
            for (p, blocks) in raw {
                out.insert(
                    p.parse()?,
                    blocks.into_iter().map(|(k, v)| (BlockId(k), v)).collect(),
                );
            }
            out
        }
        None => BTreeMap::new(),
    };

    let start = std::time::Instant::now();
    let doc = celsus_pdf::Document::open(&input)?;
    let all = doc.pages()?;
    let (first, last) = match pages.as_deref() {
        Some(r) => match r.split_once('-') {
            Some((a, b)) => (a.parse::<usize>()?, b.parse::<usize>()?),
            None => (r.parse()?, r.parse()?),
        },
        None => (1, all.len()),
    };
    // Regions per page, as {"page": [[label, score, x0, y0, x1, y1], ...]}.
    let regions: BTreeMap<String, Vec<(String, f32, f32, f32, f32, f32)>> = match &regions {
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
        None => BTreeMap::new(),
    };
    let mut writer = BookWriter::new(&doc, &fonts);
    let (mut replaced, mut kept) = (0, 0);
    for n in first.max(1)..=last.min(all.len()) {
        let page = &all[n - 1];
        let found: Vec<celsus_core::extract::Region> = regions
            .get(&n.to_string())
            .map(|list| {
                list.iter()
                    .map(|(label, score, x0, y0, x1, y1)| celsus_core::extract::Region {
                        label: label.clone(),
                        score: *score,
                        rect: celsus_core::layout::Rect {
                            x0: *x0,
                            y0: *y0,
                            x1: *x1,
                            y1: *y1,
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();
        let extracted = celsus_core::extract::extract_page_in(&doc, page, n as u32, &found)?;
        let translations: BTreeMap<BlockId, String> = match map.get(&(n as u32)) {
            Some(t) => t.clone(),
            None => extracted
                .layout
                .blocks
                .iter()
                .filter(|b| !b.protected)
                .map(|b| {
                    let source = if inline { b.source() } else { b.plain() };
                    (b.id.clone(), stand_in(&source, grow))
                })
                .collect(),
        };
        let report = writer.translate_page(page, &extracted, &translations, &to)?;
        replaced += report.replaced.len();
        kept += report.kept.len();
        for issue in &report.kept {
            println!("page {n}: kept {}", serde_json::to_string(issue)?);
        }
    }
    let bytes = writer.finish()?;
    std::fs::write(&output, &bytes)?;
    println!(
        "pages {first}-{last}: {replaced} blocks replaced, {kept} kept; {} -> {} bytes in {:?}",
        doc.data.len(),
        bytes.len(),
        start.elapsed()
    );
    Ok(())
}
