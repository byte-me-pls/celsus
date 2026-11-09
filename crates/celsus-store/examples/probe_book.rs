//! Check picture export and book recognition on a library book:
//! `cargo run -p celsus-store --example probe_book -- BOOK_ID [PAGE]`
//! Recognition uses CELSUS_API_URL, CELSUS_MODEL and CELSUS_API_KEY / NVIDIA_API_KEY from the environment or `.env`.

use std::collections::BTreeMap;

fn env() -> BTreeMap<String, String> {
    let mut vars: BTreeMap<String, String> = std::fs::read_to_string(".env")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().trim_matches('"').to_owned()))
        .collect();
    vars.extend(std::env::vars());
    vars
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let id: i64 = args.next().ok_or("missing book id")?.parse()?;
    let page: usize = args.next().map(|p| p.parse()).transpose()?.unwrap_or(1);
    let library = celsus_store::library_dir();
    let doc = celsus_pdf::Document::open(celsus_store::library::source_file(&library, id))?;
    let pages = doc.pages()?;
    for im in celsus_pdf::image::page_images(&doc, &pages[page - 1]) {
        let exported = celsus_pdf::image::export(&doc, im.id);
        println!(
            "image {} {}x{} -> {:?}",
            im.id,
            im.width,
            im.height,
            exported.as_ref().map(|(b, m)| (b.len(), *m))
        );
        if let Some((bytes, mime)) = exported {
            let ext = if mime == "image/png" { "png" } else { "jpg" };
            std::fs::create_dir_all("data/out")?;
            std::fs::write(format!("data/out/probe-image.{ext}"), bytes)?;
        }
    }
    let vars = env();
    let (Some(url), Some(model)) = (vars.get("CELSUS_API_URL"), vars.get("CELSUS_MODEL")) else {
        return Ok(());
    };
    let key = vars
        .get("CELSUS_API_KEY")
        .or_else(|| vars.get("NVIDIA_API_KEY"))
        .cloned();
    let client = celsus_core::provider::OpenAiCompatible::new(url.clone(), key, model.clone());
    let conn = celsus_store::open()?;
    let book = celsus_store::db::book(&conn, id)?;
    let sample = celsus_core::metadata::BookSample {
        file_name: book.original_name,
        title: book.title,
        author: book.author,
        text: celsus_store::library::opening_text(&doc, 3500),
        answer_language: "tr".into(),
    };
    let (info, usage) = celsus_core::metadata::identify(&client, &sample)?;
    println!("{info:#?}\n{usage:?}");
    Ok(())
}
