//! The robot's camera, answered by a Grok vision model.
//!
//! For `camera.take_photo(question)` the firmware posts a JPEG (about
//! 320×240) and the question as multipart to the URL it got in the bridge's
//! botstack/v1 `hello` (`vision`), with `Authorization: Bearer <token>`.
//! The HTTP response body becomes the tool result as is, so it is the spoken
//! answer itself.

use std::time::Duration;

use anyhow::{Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use crate::realtime::language_name;
use crate::say;

#[derive(Clone)]
pub struct Vision {
    client: reqwest::Client,
    api_key: String,
    model: String,
    /// Handed to the robot in the `hello`; random per process.
    pub token: String,
    /// Public URL the robot posts photos to.
    pub url: String,
}

impl Vision {
    pub fn new(api_key: String, model: String, url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(25))
            .build()
            .expect("http client");
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        Self { client, api_key, model, token, url }
    }

    /// A short spoken answer about the photo, in the device's `language`
    /// (any ISO 639-1 code).
    pub async fn explain(&self, question: &str, jpeg: &[u8], language: &str) -> Result<String> {
        let body = self.request(question, jpeg, language);
        let response = self
            .client
            .post("https://api.x.ai/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("vision HTTP {}", response.status());
        }
        let reply: Value = response.json().await?;
        Ok(reply["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string())
    }

    fn request(&self, question: &str, jpeg: &[u8], language: &str) -> Value {
        let question = if question.trim().is_empty() { say(language, "Qu'est-ce que tu vois ?", "What do you see?") } else { question };
        let system = format!(
            "You are the eyes of a small desk robot. The photo comes from its camera, facing the user. \
             Answer the question about it in {}, in one or two short spoken sentences. \
             No markdown, no lists. If the photo is too dark or blurry, say so.",
            language_name(language)
        );
        json!({
            "model": self.model,
            "reasoning_effort": "none",
            "max_tokens": 200,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", B64.encode(jpeg))}},
                    {"type": "text", "text": question}
                ]}
            ]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_follows_the_device_language() {
        let vision = Vision::new(String::new(), "grok-4.3".into(), String::new());
        let french = vision.request("", b"jpeg", "fr");
        assert!(french["messages"][0]["content"].as_str().unwrap().contains("in French"));
        assert_eq!(french["messages"][1]["content"][1]["text"], "Qu'est-ce que tu vois ?");
        let english = vision.request("  ", b"jpeg", "en");
        assert!(english["messages"][0]["content"].as_str().unwrap().contains("in English"));
        assert_eq!(english["messages"][1]["content"][1]["text"], "What do you see?");
        let german = vision.request("Was ist das?", b"jpeg", "de");
        assert!(german["messages"][0]["content"].as_str().unwrap().contains("in German"));
        assert_eq!(german["messages"][1]["content"][1]["text"], "Was ist das?");
        assert_eq!(german["model"], "grok-4.3");
        let japanese = vision.request("", b"jpeg", "ja");
        assert!(japanese["messages"][0]["content"].as_str().unwrap().contains("in Japanese"));
        assert_eq!(japanese["messages"][1]["content"][1]["text"], "What do you see?");
        let rare = vision.request("", b"jpeg", "sw");
        assert!(rare["messages"][0]["content"].as_str().unwrap().contains("in sw"));
    }
}
