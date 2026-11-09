//! Ask each model to translate one short passage, to compare speed and quality:
//! `cargo run --release -p celsus-core --example probe_models -- --to tr model-a model-b ...`
//! Uses `CELSUS_API_URL` and the key from `.env` like `translate_book`.

use std::time::Instant;

use celsus_core::provider::OpenAiCompatible;
use celsus_core::translate::{BookContext, TranslationRequest, Translator};
use celsus_core::{BlockId, Language};

const SAMPLE: &str = "A model is a selectively simplified and consciously structured form of knowledge. \
An appropriate model makes sense of information and focuses it on a problem. Domain experts and developers \
crunch knowledge together until the model reflects a deep understanding of the domain.";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(text) = std::fs::read_to_string(".env") {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=')
                && std::env::var_os(k.trim()).is_none()
            {
                // SAFETY: start-up, single thread.
                unsafe { std::env::set_var(k.trim(), v.trim()) };
            }
        }
    }
    let mut args = std::env::args().skip(1);
    let mut to: Language = "tr".parse()?;
    let mut models = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--to" => to = args.next().ok_or("--to needs a value")?.parse()?,
            _ => models.push(a),
        }
    }
    let url = std::env::var("CELSUS_API_URL")?;
    let key = ["CELSUS_API_KEY", "NVIDIA_API_KEY", "OPENAI_API_KEY"]
        .iter()
        .find_map(|k| std::env::var(k).ok());
    for model in models {
        let provider = OpenAiCompatible::new(url.clone(), key.clone(), model.clone());
        let request = TranslationRequest {
            source_language: Some("en".parse()?),
            target_language: to.clone(),
            book: BookContext {
                title: "Domain-Driven Design".into(),
                ..Default::default()
            },
            previous_text: String::new(),
            next_text: String::new(),
            blocks: vec![(BlockId("b0001".into()), SAMPLE.into())],
            feedback: Vec::new(),
        };
        let t = Instant::now();
        match provider.translate(&request) {
            Ok((texts, usage)) => println!(
                "{model}: {:.1}s, {}+{} tokens\n  {}\n",
                t.elapsed().as_secs_f32(),
                usage.prompt_tokens,
                usage.completion_tokens,
                texts[0]
            ),
            Err(e) => println!("{model}: {:.1}s, {e}\n", t.elapsed().as_secs_f32()),
        }
    }
    Ok(())
}
