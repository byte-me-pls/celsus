//! Recognising a book: its title, author, subject and language from the first pages, asked of a model once per
//! book. The answer only fills in details; nothing is guessed when the model does not know the book.

use serde::{Deserialize, Serialize};

use crate::provider::OpenAiCompatible;
use crate::translate::{TranslateError, Usage};

/// What is known about a book before asking.
#[derive(Debug, Clone, Default)]
pub struct BookSample {
    pub file_name: String,
    pub title: String,
    pub author: String,
    /// Text of the first pages, shortened.
    pub text: String,
    /// Language the subject should be written in (the app's interface language), e.g. `tr`.
    pub answer_language: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BookInfo {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: String,
    /// One or two sentences: what the book is about and terms that need care when translating it.
    #[serde(default)]
    pub subject: String,
    /// ISO 639-1 code of the book's language.
    #[serde(default)]
    pub language: String,
    /// Whether the model recognised the book itself rather than only reading the sample.
    #[serde(default)]
    pub recognised: bool,
}

pub fn identify(
    model: &OpenAiCompatible,
    sample: &BookSample,
) -> Result<(BookInfo, Usage), TranslateError> {
    let system = "You catalogue books. From the file name, the document's own metadata and the text of its first \
pages, give the book's real title (without edition notes or file artefacts), its author or authors (comma separated), \
its language as an ISO 639-1 code, and a subject of one or two sentences: what the book is about, for whom, and \
which terms have a field-specific meaning a translator must keep. Write the subject in the requested language. \
Set recognised to true only if you know this book itself. Never invent an author: leave it empty if it is not in \
the sample and you do not know the book. Answer with one JSON object and nothing else: \
{\"title\":\"\",\"author\":\"\",\"language\":\"\",\"subject\":\"\",\"recognised\":false}";
    let user = format!(
        "Write the subject in: {}\nFile name: {}\nMetadata title: {}\nMetadata author: {}\n\nFirst pages:\n{}",
        if sample.answer_language.is_empty() {
            "en"
        } else {
            &sample.answer_language
        },
        sample.file_name,
        sample.title,
        sample.author,
        sample.text
    );
    let (answer, usage) = model.chat(system, &user)?;
    let start = answer
        .find('{')
        .ok_or_else(|| TranslateError::BadAnswer("no JSON in the answer".into()))?;
    let end = answer
        .rfind('}')
        .ok_or_else(|| TranslateError::BadAnswer("no JSON in the answer".into()))?;
    let mut info: BookInfo = serde_json::from_str(&answer[start..=end])
        .map_err(|e| TranslateError::BadAnswer(e.to_string()))?;
    for field in [
        &mut info.title,
        &mut info.author,
        &mut info.subject,
        &mut info.language,
    ] {
        *field = field.trim().to_owned();
    }
    info.language = info.language.to_lowercase();
    Ok((info, usage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::mock::{chat_answer, serve};

    #[test]
    fn reads_the_answer_even_with_words_around_it() {
        let answer = "Sure:\n{\"title\":\" Domain-Driven Design \",\"author\":\"Eric Evans\",\"language\":\"EN\",\"subject\":\"Yazılım tasarımı.\",\"recognised\":true}\nDone.";
        let server = serve(vec![(200, Vec::new(), chat_answer(answer))]);
        let model = OpenAiCompatible::new(&server.url, None, "m");
        let sample = BookSample {
            file_name: "ddd.pdf".into(),
            text: "Domain-Driven Design. Eric Evans".into(),
            answer_language: "tr".into(),
            ..Default::default()
        };
        let (info, usage) = identify(&model, &sample).unwrap();
        assert_eq!(info.title, "Domain-Driven Design");
        assert_eq!(info.language, "en");
        assert!(info.recognised);
        assert_eq!(usage.prompt_tokens, 11);
        let sent = server.requests.lock().unwrap()[0].json();
        let user = sent["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("Write the subject in: tr") && user.contains("ddd.pdf"));
    }

    #[test]
    fn missing_fields_are_empty_and_prose_is_an_error() {
        let server = serve(vec![
            (200, Vec::new(), chat_answer("{\"title\":\"X\"}")),
            (200, Vec::new(), chat_answer("I do not know this book.")),
        ]);
        let model = OpenAiCompatible::new(&server.url, None, "m");
        let (info, _) = identify(&model, &BookSample::default()).unwrap();
        assert_eq!(
            (info.title.as_str(), info.author.as_str(), info.recognised),
            ("X", "", false)
        );
        assert!(identify(&model, &BookSample::default()).is_err());
    }
}
