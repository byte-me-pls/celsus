//! Model providers that speak the OpenAI protocol: OpenAI, NVIDIA's hosted models, OpenRouter, Ollama, vLLM,
//! LM Studio and others. Chat completions translate and describe books; the images endpoint redraws pictures
//! (covers with lettering, for example) with their text in another language.

use std::time::Duration;

use serde_json::{Value, json};

use crate::translate::{
    TranslateError, TranslationRequest, Translator, Usage, parse_answer, prompt,
};

pub struct OpenAiCompatible {
    agent: ureq::Agent,
    /// Base URL ending in `/v1`, e.g. `https://integrate.api.nvidia.com/v1` or `http://localhost:11434/v1`.
    pub base_url: String,
    api_key: Option<String>,
    pub model: String,
    pub max_tokens: u32,
}

impl OpenAiCompatible {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // An API does not redirect; following one could lead a server to addresses it must not reach.
            .max_redirects(0)
            // Drawing a large picture can take several minutes.
            .timeout_global(Some(Duration::from_secs(600)))
            .build();
        OpenAiCompatible {
            agent: ureq::Agent::new_with_config(config),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.filter(|k| !k.is_empty()),
            model: model.into(),
            max_tokens: 8192,
        }
    }

    /// OpenAI's own endpoint rejects parameters it does not know, so it gets a body of its own.
    fn is_openai(&self) -> bool {
        self.base_url.contains("api.openai.com")
    }

    /// Models that reason before answering (they take no temperature).
    fn is_reasoning_model(&self) -> bool {
        let m = self.model.rsplit('/').next().unwrap_or(&self.model);
        m.starts_with("gpt-5")
            || (m.starts_with('o') && m[1..].starts_with(|c: char| c.is_ascii_digit()))
    }

    fn post(&self, path: &str, body: &Value) -> Result<(u16, String, Option<u64>), TranslateError> {
        let mut req = self
            .agent
            .post(format!("{}/{path}", self.base_url))
            .header("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let mut response = req
            .send_json(body)
            .map_err(|e| TranslateError::Provider(e.to_string()))?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let text = response
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| TranslateError::Provider(e.to_string()))?;
        Ok((status, text, retry_after))
    }

    fn failure(status: u16, text: &str, retry_after: Option<u64>) -> TranslateError {
        if status == 429 {
            return TranslateError::RateLimited {
                retry_after_secs: retry_after.unwrap_or(30).clamp(1, 3600),
            };
        }
        // OpenAI style errors carry a readable message.
        let message = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| text.chars().take(300).collect());
        let message = format!("HTTP {status}: {}", message.trim());
        match status {
            400..=499 => TranslateError::Fatal(message),
            _ => TranslateError::Provider(message),
        }
    }

    /// One chat completion: the answer's text and the tokens it took.
    pub fn chat(&self, system: &str, user: &str) -> Result<(String, Usage), TranslateError> {
        let messages =
            json!([{"role": "system", "content": system}, {"role": "user", "content": user}]);
        let body = self.chat_body(&messages);
        let (mut status, mut text, mut retry_after) = self.post("chat/completions", &body)?;
        // A server that refuses one of the optional switches gets the request again without them.
        if status == 400 {
            let lower = text.to_lowercase();
            if [
                "parameter",
                "argument",
                "unsupported",
                "not supported",
                "extra inputs",
            ]
            .iter()
            .any(|w| lower.contains(w))
            {
                let plain = json!({"model": self.model, "messages": messages});
                (status, text, retry_after) = self.post("chat/completions", &plain)?;
            }
        }
        if !(200..300).contains(&status) {
            return Err(Self::failure(status, &text, retry_after));
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| TranslateError::BadAnswer(e.to_string()))?;
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| TranslateError::BadAnswer("no message content".into()))?;
        let usage = Usage {
            prompt_tokens: value["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            completion_tokens: value["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        };
        Ok((content.to_owned(), usage))
    }

    /// The request body of a chat completion, shaped for the server it goes to.
    fn chat_body(&self, messages: &Value) -> Value {
        if self.is_openai() {
            let mut b = json!({
                "model": self.model,
                "messages": messages,
                "max_completion_tokens": self.max_tokens,
            });
            if self.is_reasoning_model() {
                b["reasoning_effort"] = json!("low");
            } else {
                b["temperature"] = json!(0.2);
            }
            b
        } else {
            json!({
                "model": self.model,
                "messages": messages,
                "temperature": 0.2,
                "max_tokens": self.max_tokens,
                "stream": false,
                // Models that reason before answering are asked not to; others ignore these switches.
                "reasoning_effort": "low",
                "chat_template_kwargs": {"enable_thinking": false, "thinking": false},
            })
        }
    }

    /// The models the endpoint offers (`GET /models`), sorted.
    pub fn list_models(&self) -> Result<Vec<String>, TranslateError> {
        let mut req = self.agent.get(format!("{}/models", self.base_url));
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let mut response = req
            .call()
            .map_err(|e| TranslateError::Provider(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| TranslateError::Provider(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(Self::failure(status, &text, None));
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| TranslateError::BadAnswer(e.to_string()))?;
        let mut ids: Vec<String> = value["data"]
            .as_array()
            .or_else(|| value["models"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        m["id"]
                            .as_str()
                            .or_else(|| m["name"].as_str())
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        Ok(ids)
    }

    /// Whether the model draws through the images endpoint (OpenAI's picture models) rather than a chat that
    /// answers with a picture (Gemini and others through OpenRouter).
    pub fn uses_images_endpoint(&self) -> bool {
        ["gpt-image", "chatgpt-image", "dall-e"]
            .iter()
            .any(|m| self.model.contains(m))
    }

    /// Redraw a picture following `instructions`. `image` is a JPEG or PNG; `aspect` is its width over height,
    /// kept where the model allows. Returns a JPEG or PNG file.
    pub fn edit_image(
        &self,
        image: &[u8],
        mime: &str,
        aspect: f32,
        instructions: &str,
    ) -> Result<Vec<u8>, TranslateError> {
        if self.uses_images_endpoint() {
            self.edit_image_endpoint(image, mime, aspect, instructions)
        } else {
            self.edit_image_by_chat(image, mime, instructions)
        }
    }

    /// A chat completion that sends the picture and asks for one back (`modalities: ["image", "text"]`).
    fn edit_image_by_chat(
        &self,
        image: &[u8],
        mime: &str,
        instructions: &str,
    ) -> Result<Vec<u8>, TranslateError> {
        let data_url = format!("data:{mime};base64,{}", crate::base64::encode(image));
        let body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": instructions},
                {"type": "image_url", "image_url": {"url": data_url}},
            ]}],
            "modalities": ["image", "text"],
        });
        let (status, text, retry_after) = self.post("chat/completions", &body)?;
        if !(200..300).contains(&status) {
            return Err(Self::failure(status, &text, retry_after));
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| TranslateError::BadAnswer(e.to_string()))?;
        let message = &value["choices"][0]["message"];
        // OpenRouter puts pictures in `images`; some servers put them among the content parts.
        let url = message["images"]
            .as_array()
            .into_iter()
            .chain(message["content"].as_array())
            .flatten()
            .find_map(|part| part["image_url"]["url"].as_str())
            .ok_or_else(|| {
                let said = message["content"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect::<String>();
                TranslateError::BadAnswer(if said.is_empty() {
                    "the model answered without a picture".into()
                } else {
                    format!("the model answered without a picture: {said}")
                })
            })?;
        let b64 = url.split_once("base64,").map_or(url, |(_, b)| b);
        crate::base64::decode(b64).ok_or_else(|| TranslateError::BadAnswer("bad image data".into()))
    }

    fn edit_image_endpoint(
        &self,
        image: &[u8],
        mime: &str,
        aspect: f32,
        instructions: &str,
    ) -> Result<Vec<u8>, TranslateError> {
        let size = image_size(&self.model, aspect);
        let mut body = json!({
            "model": self.model,
            "images": [{"image_url": format!("data:{mime};base64,{}", crate::base64::encode(image))}],
            "prompt": instructions,
            "size": size,
            "quality": "high",
            "output_format": "jpeg",
            "output_compression": 92,
            "n": 1,
        });
        if self.model.contains("gpt-image-1") && !self.model.contains("mini") {
            body["input_fidelity"] = json!("high");
        }
        let (status, text, retry_after) = self.post("images/edits", &body)?;
        if !(200..300).contains(&status) {
            return Err(Self::failure(status, &text, retry_after));
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| TranslateError::BadAnswer(e.to_string()))?;
        let b64 = value["data"][0]["b64_json"]
            .as_str()
            .ok_or_else(|| TranslateError::BadAnswer("no image in the answer".into()))?;
        crate::base64::decode(b64).ok_or_else(|| TranslateError::BadAnswer("bad image data".into()))
    }
}

/// The size to ask for. Newer models draw any size whose sides are multiples of 16, so the picture keeps its
/// shape; the others offer square, portrait and landscape only.
pub fn image_size(model: &str, aspect: f32) -> String {
    let aspect = aspect.clamp(1.0 / 3.0, 3.0);
    let custom = model.contains("gpt-image-2") || model.contains("chatgpt-image");
    if custom {
        let long = 1536.0;
        let (w, h) = if aspect < 1.0 {
            (long * aspect, long)
        } else {
            (long, long / aspect)
        };
        let r16 = |v: f32| ((v / 16.0).round() as u32).max(16) * 16;
        return format!("{}x{}", r16(w), r16(h));
    }
    if aspect < 0.83 {
        "1024x1536".into()
    } else if aspect > 1.2 {
        "1536x1024".into()
    } else {
        "1024x1024".into()
    }
}

impl Translator for OpenAiCompatible {
    fn name(&self) -> String {
        self.model.clone()
    }

    fn translate(
        &self,
        request: &TranslationRequest,
    ) -> Result<(Vec<String>, Usage), TranslateError> {
        let (system, user) = prompt(request);
        let (content, usage) = self.chat(&system, &user)?;
        Ok((parse_answer(&content, &request.blocks)?, usage))
    }
}

/// A tiny HTTP server for tests: answers requests with canned responses, in order, and records what it got.
#[cfg(test)]
pub(crate) mod mock {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    pub struct Request {
        pub method: String,
        pub path: String,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    impl Request {
        pub fn json(&self) -> serde_json::Value {
            serde_json::from_str(&self.body).unwrap_or_default()
        }
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    pub struct Server {
        /// Base URL ending in `/v1`.
        pub url: String,
        pub requests: Arc<Mutex<Vec<Request>>>,
    }

    /// One canned answer: status, extra headers and body.
    pub type Answer = (u16, Vec<(&'static str, String)>, String);

    pub fn serve(responses: Vec<Answer>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for (status, headers, body) in responses {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                let mut got_headers = Vec::new();
                let mut length = 0;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                        got_headers.push((k.trim().to_owned(), v.trim().to_owned()));
                    }
                }
                let mut buf = vec![0; length];
                reader.read_exact(&mut buf).unwrap();
                seen.lock().unwrap().push(Request {
                    method,
                    path,
                    headers: got_headers,
                    body: String::from_utf8_lossy(&buf).into_owned(),
                });
                let mut out = stream;
                let mut head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (k, v) in headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                out.write_all(head.as_bytes()).unwrap();
                out.write_all(body.as_bytes()).unwrap();
                out.flush().unwrap();
            }
        });
        Server { url, requests }
    }

    /// A chat completion answer whose message says `content`.
    pub fn chat_answer(content: &str) -> String {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": content}}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7}
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{chat_answer, serve};
    use super::*;
    use crate::BlockId;

    fn ok(body: String) -> (u16, Vec<(&'static str, String)>, String) {
        (200, Vec::new(), body)
    }

    #[test]
    fn openai_gets_only_parameters_it_knows() {
        let msgs = json!([]);
        let reasoning = OpenAiCompatible::new("https://api.openai.com/v1", None, "gpt-5.5");
        let b = reasoning.chat_body(&msgs);
        assert_eq!(b["max_completion_tokens"], 8192);
        assert_eq!(b["reasoning_effort"], "low");
        assert!(b.get("temperature").is_none() && b.get("max_tokens").is_none());
        assert!(b.get("chat_template_kwargs").is_none());
        let plain = OpenAiCompatible::new("https://api.openai.com/v1", None, "gpt-4.1");
        let b = plain.chat_body(&msgs);
        assert_eq!(b["temperature"], 0.2);
        assert!(b.get("reasoning_effort").is_none());
        // Other servers (NVIDIA, OpenRouter, Ollama, LM Studio) get the general body with the thinking switches.
        let nvidia = OpenAiCompatible::new("https://integrate.api.nvidia.com/v1", None, "x/y");
        let b = nvidia.chat_body(&msgs);
        assert_eq!(b["max_tokens"], 8192);
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
    }

    #[test]
    fn reasoning_models_are_recognised() {
        for (m, r) in [
            ("gpt-5.5", true),
            ("o3", true),
            ("o4-mini", true),
            ("gpt-4.1", false),
            ("omni", false),
        ] {
            assert_eq!(
                OpenAiCompatible::new("u", None, m).is_reasoning_model(),
                r,
                "{m}"
            );
        }
    }

    #[test]
    fn translates_with_key_and_parses_the_answer() {
        let answer = r#"{"translations":[{"id":"b1","text":"Kütüphane bir kitap bahçesidir."}]}"#;
        let server = serve(vec![ok(chat_answer(answer))]);
        let client = OpenAiCompatible::new(&server.url, Some("secret".into()), "m");
        let request = TranslationRequest {
            source_language: "en".parse().ok(),
            target_language: "tr".parse().unwrap(),
            book: Default::default(),
            previous_text: String::new(),
            next_text: String::new(),
            blocks: vec![(
                BlockId("b1".into()),
                "A library is a garden of books.".into(),
            )],
            feedback: Vec::new(),
        };
        let (texts, usage) = client.translate(&request).unwrap();
        assert_eq!(texts, vec!["Kütüphane bir kitap bahçesidir."]);
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (11, 7));
        let req = &server.requests.lock().unwrap()[0];
        assert_eq!(
            (req.method.as_str(), req.path.as_str()),
            ("POST", "/v1/chat/completions")
        );
        assert_eq!(req.header("authorization"), Some("Bearer secret"));
        assert_eq!(req.json()["model"], "m");
    }

    #[test]
    fn no_key_sends_no_authorization() {
        let server = serve(vec![ok(chat_answer("hi"))]);
        OpenAiCompatible::new(&server.url, Some(String::new()), "m")
            .chat("s", "u")
            .unwrap();
        assert!(
            server.requests.lock().unwrap()[0]
                .header("authorization")
                .is_none()
        );
    }

    #[test]
    fn refused_switches_are_dropped_and_asked_again() {
        let refusal =
            r#"{"error":{"message":"Unsupported parameter: 'reasoning_effort'"}}"#.to_owned();
        let server = serve(vec![(400, Vec::new(), refusal), ok(chat_answer("tamam"))]);
        let (text, _) = OpenAiCompatible::new(&server.url, None, "m")
            .chat("s", "u")
            .unwrap();
        assert_eq!(text, "tamam");
        let reqs = server.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[0].json().get("reasoning_effort").is_some());
        assert!(reqs[1].json().get("reasoning_effort").is_none());
        assert!(reqs[1].json().get("temperature").is_none());
    }

    #[test]
    fn rate_limits_say_how_long_to_wait() {
        let server = serve(vec![(429, vec![("Retry-After", "7".into())], "{}".into())]);
        let e = OpenAiCompatible::new(&server.url, None, "m")
            .chat("s", "u")
            .unwrap_err();
        assert!(
            matches!(
                e,
                TranslateError::RateLimited {
                    retry_after_secs: 7
                }
            ),
            "{e:?}"
        );
    }

    #[test]
    fn client_errors_are_final_and_readable() {
        let body = r#"{"error":{"message":"Incorrect API key provided"}}"#.to_owned();
        let server = serve(vec![(401, Vec::new(), body)]);
        let e = OpenAiCompatible::new(&server.url, None, "m")
            .chat("s", "u")
            .unwrap_err();
        match e {
            TranslateError::Fatal(m) => {
                assert!(m.contains("401") && m.contains("Incorrect API key"), "{m}")
            }
            other => panic!("{other:?}"),
        }
        let server = serve(vec![(503, Vec::new(), "busy".into())]);
        let e = OpenAiCompatible::new(&server.url, None, "m")
            .chat("s", "u")
            .unwrap_err();
        assert!(
            matches!(e, TranslateError::Provider(_)),
            "server errors are retried: {e:?}"
        );
    }

    #[test]
    fn lists_models_in_openai_and_ollama_shapes() {
        let server = serve(vec![
            ok(r#"{"data":[{"id":"b"},{"id":"a"}]}"#.into()),
            ok(r#"{"models":[{"name":"qwen3:14b"}]}"#.into()),
        ]);
        let c = OpenAiCompatible::new(&server.url, None, "");
        assert_eq!(c.list_models().unwrap(), vec!["a", "b"]);
        assert_eq!(c.list_models().unwrap(), vec!["qwen3:14b"]);
        assert_eq!(server.requests.lock().unwrap()[0].path, "/v1/models");
    }

    #[test]
    fn picture_models_draw_through_the_images_endpoint() {
        let picture = crate::base64::encode(b"\xFF\xD8new picture");
        let server = serve(vec![ok(
            json!({"data": [{"b64_json": picture}]}).to_string()
        )]);
        let c = OpenAiCompatible::new(&server.url, None, "gpt-image-2");
        assert!(c.uses_images_endpoint());
        let out = c
            .edit_image(b"\xFF\xD8old", "image/jpeg", 0.75, "Translate it")
            .unwrap();
        assert_eq!(out, b"\xFF\xD8new picture");
        let req = &server.requests.lock().unwrap()[0];
        assert_eq!(req.path, "/v1/images/edits");
        let body = req.json();
        assert_eq!(body["size"], "1152x1536");
        assert_eq!(body["output_format"], "jpeg");
        assert_eq!(body["prompt"], "Translate it");
        let sent = body["images"][0]["image_url"].as_str().unwrap();
        assert_eq!(
            sent,
            format!(
                "data:image/jpeg;base64,{}",
                crate::base64::encode(b"\xFF\xD8old")
            )
        );
        assert!(body.get("input_fidelity").is_none());
    }

    #[test]
    fn other_models_draw_through_a_chat() {
        let png = format!(
            "data:image/png;base64,{}",
            crate::base64::encode(b"\x89PNGdata")
        );
        let answer = json!({"choices": [{"message": {"content": "Here it is", "images": [{"type": "image_url", "image_url": {"url": png}}]}}]});
        let server = serve(vec![ok(answer.to_string())]);
        let c = OpenAiCompatible::new(&server.url, None, "google/gemini-2.5-flash-image");
        assert!(!c.uses_images_endpoint());
        let out = c
            .edit_image(b"\x89PNGold", "image/png", 1.0, "Translate it")
            .unwrap();
        assert_eq!(out, b"\x89PNGdata");
        let body = server.requests.lock().unwrap()[0].json();
        assert_eq!(body["modalities"], json!(["image", "text"]));
        assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
    }

    #[test]
    fn a_chat_without_a_picture_is_reported() {
        let server = serve(vec![ok(chat_answer("I cannot edit images."))]);
        let e = OpenAiCompatible::new(&server.url, None, "some/model")
            .edit_image(b"x", "image/png", 1.0, "t")
            .unwrap_err();
        assert!(e.to_string().contains("I cannot edit images"), "{e}");
    }

    #[test]
    fn sizes_keep_the_shape_where_possible() {
        assert_eq!(image_size("gpt-image-2", 0.75), "1152x1536");
        assert_eq!(image_size("gpt-image-1.5", 0.75), "1024x1536");
        assert_eq!(image_size("gpt-image-1.5", 1.0), "1024x1024");
    }
}
