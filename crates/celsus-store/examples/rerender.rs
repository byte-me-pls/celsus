//! Rewrite a book's translated PDF from the cached translations (after a layout change), without asking any model:
//! `cargo run --release -p celsus-store --example rerender -- BOOK_ID LANGUAGE`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let id: i64 = args.next().ok_or("missing book id")?.parse()?;
    let lang: celsus_core::Language = args.next().ok_or("missing language")?.parse()?;
    let conn = celsus_store::open()?;
    let kept =
        celsus_store::assistant::render(&conn, &celsus_store::library_dir(), id, &lang, &[])?;
    println!("rewritten; {kept} blocks kept their original text");
    Ok(())
}
