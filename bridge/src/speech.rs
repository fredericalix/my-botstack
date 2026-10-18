//! Verbatim speech through xAI `/v1/tts`, in the device's voice.
//!
//! Grok Bot replies, pushed messages and apologies must be said word for
//! word. Realtime `force_message` was tried first: a non-interruptible one made
//! xAI ignore the next question after a tap, and an interruptible one was
//! cancelled by any speech at the start of a conversation and lost. A separate
//! TTS call keeps the realtime session out of it.

use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;

use crate::audio::{DOWNLINK_RATE, pcm_from_le};

#[derive(Clone)]
pub struct Tts {
    client: reqwest::Client,
    api_key: String,
}

impl Tts {
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        Self { client, api_key }
    }

    /// Speech in `voice` and `language`, PCM 24 kHz mono.
    pub async fn synthesise(&self, text: &str, voice: &str, language: &str) -> Result<Vec<i16>> {
        let response = self
            .client
            .post("https://api.x.ai/v1/tts")
            .bearer_auth(&self.api_key)
            .json(&json!({
                "text": text,
                "voice_id": voice,
                "language": language,
                "output_format": {"codec": "pcm", "sample_rate": DOWNLINK_RATE}
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("xAI TTS HTTP {}", response.status());
        }
        Ok(pcm_from_le(&response.bytes().await?))
    }
}

/// Two short tones before a message the user did not ask for just now.
pub fn chime() -> Vec<i16> {
    let rate = DOWNLINK_RATE as f32;
    let mut pcm = Vec::new();
    for (freq, ms) in [(880.0f32, 110.0f32), (1320.0, 160.0)] {
        let n = (rate * ms / 1000.0) as usize;
        for i in 0..n {
            let fade = (i.min(n - i) as f32 / (rate * 0.01)).min(1.0);
            let t = i as f32 / rate;
            pcm.push((0.5 * fade * (std::f32::consts::TAU * freq * t).sin() * 32767.0) as i16);
        }
    }
    pcm.extend(std::iter::repeat_n(0, (rate * 0.2) as usize));
    pcm
}
