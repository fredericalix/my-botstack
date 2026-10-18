//! Audio between the robot and xAI.
//!
//! Uplink: the robot sends 16 kHz mono Opus, 60 ms per packet. xAI takes PCM
//! 16 kHz, so each packet is decoded as it arrives.
//!
//! Downlink: xAI sends 24 kHz PCM in bursts. xAI's own Opus is 10 ms packets
//! with mixed modes that cannot be regrouped losslessly, and the robot wants
//! one 60 ms packet per WebSocket frame. So the bridge shapes the voice for the
//! small speaker and encodes its own packets:
//!
//! - xAI voices are bass-heavy (about 75% of the energy at 150-300 Hz). The
//!   speaker buzzes there, so a 4th-order high-pass at 300 Hz and a +10 dB
//!   presence peak at 3 kHz give it a profile close to Edge, which sounded clean.
//! - Peaks above about -6 dBFS made the board stutter, with any voice. A peak
//!   limiter holds the output under -10 dBFS.
//! - 64 kbps constant bitrate, complexity 5, voice signal: the settings that
//!   gave a clean voice on the xiaozhi server.

use anyhow::Result;

pub const UPLINK_RATE: u32 = 16_000;
pub const DOWNLINK_RATE: u32 = 24_000;
pub const FRAME_MS: u32 = 60;
pub const UPLINK_FRAME: usize = (UPLINK_RATE * FRAME_MS / 1000) as usize; // 960
pub const DOWNLINK_FRAME: usize = (DOWNLINK_RATE * FRAME_MS / 1000) as usize; // 1440

/// Decodes the robot's microphone packets to PCM 16 kHz.
pub struct Uplink {
    decoder: opus::Decoder,
    pcm: Vec<i16>,
}

impl Uplink {
    pub fn new() -> Result<Self> {
        Ok(Self {
            decoder: opus::Decoder::new(UPLINK_RATE, opus::Channels::Mono)?,
            // Up to 120 ms per packet.
            pcm: vec![0; UPLINK_FRAME * 2],
        })
    }

    /// Little-endian PCM bytes for one Opus packet.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        let samples = self.decoder.decode(packet, &mut self.pcm, false)?;
        Ok(self.pcm[..samples].iter().flat_map(|s| s.to_le_bytes()).collect())
    }
}

/// Settings for the downlink voice shaping. Defaults are the values tuned on
/// the robot on 2026-10-06.
#[derive(Clone, Debug)]
pub struct VoiceShape {
    pub highpass_hz: f32,
    pub presence_hz: f32,
    pub presence_db: f32,
    pub gain_db: f32,
    pub ceiling_dbfs: f32,
}

impl Default for VoiceShape {
    fn default() -> Self {
        Self {
            highpass_hz: 300.0,
            presence_hz: 3000.0,
            presence_db: 10.0,
            // -6 dB matches the level that sounded right on the robot
            // (speech about -28 dBFS); the limiter then only catches ~1% of
            // samples instead of flattening the voice.
            gain_db: -6.0,
            ceiling_dbfs: -10.0,
        }
    }
}

/// Shapes 24 kHz PCM and cuts it into 60 ms Opus packets.
pub struct Downlink {
    filters: Vec<Biquad>,
    limiter: Limiter,
    gain: f32,
    encoder: opus::Encoder,
    pending: Vec<i16>,
    packet: Vec<u8>,
}

impl Downlink {
    pub fn new(shape: &VoiceShape) -> Result<Self> {
        let rate = DOWNLINK_RATE as f32;
        let mut encoder = opus::Encoder::new(DOWNLINK_RATE, opus::Channels::Mono, opus::Application::Voip)?;
        encoder.set_bitrate(opus::Bitrate::Bits(64_000))?;
        encoder.set_vbr(false)?;
        encoder.set_complexity(5)?;
        encoder.set_signal(opus::Signal::Voice)?;
        Ok(Self {
            // Butterworth 4th order = two 2nd-order sections with these Q.
            filters: vec![
                Biquad::highpass(shape.highpass_hz, 0.541_196_1, rate),
                Biquad::highpass(shape.highpass_hz, 1.306_563, rate),
                Biquad::peaking(shape.presence_hz, shape.presence_db, 0.7, rate),
            ],
            limiter: Limiter::new(db_to_linear(shape.ceiling_dbfs), 0.08, rate),
            gain: db_to_linear(shape.gain_db),
            encoder,
            pending: Vec::with_capacity(DOWNLINK_FRAME * 4),
            packet: vec![0; 4000],
        })
    }

    /// Shape `pcm` and return the complete 60 ms packets it fills.
    pub fn push(&mut self, pcm: &[i16]) -> Result<Vec<Vec<u8>>> {
        for &sample in pcm {
            let mut x = sample as f32 / 32768.0;
            for filter in &mut self.filters {
                x = filter.process(x);
            }
            let y = self.limiter.process(x * self.gain);
            self.pending.push((y * 32767.0).round().clamp(-32768.0, 32767.0) as i16);
        }
        let mut packets = Vec::new();
        while self.pending.len() >= DOWNLINK_FRAME {
            let frame: Vec<i16> = self.pending.drain(..DOWNLINK_FRAME).collect();
            packets.push(self.encode(&frame)?);
        }
        Ok(packets)
    }

    /// Pad the tail of a reply with silence into a last packet.
    pub fn flush(&mut self) -> Result<Option<Vec<u8>>> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let mut frame = std::mem::take(&mut self.pending);
        frame.resize(DOWNLINK_FRAME, 0);
        Ok(Some(self.encode(&frame)?))
    }

    /// Drop any partial frame, for an interrupted reply.
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    fn encode(&mut self, frame: &[i16]) -> Result<Vec<u8>> {
        let size = self.encoder.encode(frame, &mut self.packet)?;
        Ok(self.packet[..size].to_vec())
    }

    /// Shaped PCM without encoding, for offline checks.
    pub fn shape_only(&mut self, pcm: &[i16]) -> Vec<i16> {
        pcm.iter()
            .map(|&sample| {
                let mut x = sample as f32 / 32768.0;
                for filter in &mut self.filters {
                    x = filter.process(x);
                }
                let y = self.limiter.process(x * self.gain);
                (y * 32767.0).round().clamp(-32768.0, 32767.0) as i16
            })
            .collect()
    }
}

/// Little-endian PCM bytes to samples.
pub fn pcm_from_le(bytes: &[u8]) -> Vec<i16> {
    bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// RBJ cookbook biquad, transposed direct form II.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn from_coefficients(b: [f32; 3], a: [f32; 3]) -> Self {
        Self {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn highpass(f0: f32, q: f32, rate: f32) -> Self {
        let w0 = std::f32::consts::TAU * f0 / rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        Self::from_coefficients(
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    fn peaking(f0: f32, gain_db: f32, q: f32, rate: f32) -> Self {
        let amp = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * f0 / rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        Self::from_coefficients(
            [1.0 + alpha * amp, -2.0 * cos, 1.0 - alpha * amp],
            [1.0 + alpha / amp, -2.0 * cos, 1.0 - alpha / amp],
        )
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Peak limiter: instant attack, exponential release.
struct Limiter {
    ceiling: f32,
    gain: f32,
    release: f32,
}

impl Limiter {
    fn new(ceiling: f32, release_s: f32, rate: f32) -> Self {
        Self {
            ceiling,
            gain: 1.0,
            release: 1.0 - (-1.0 / (release_s * rate)).exp(),
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let peak = x.abs();
        if peak * self.gain > self.ceiling {
            self.gain = self.ceiling / peak;
        } else {
            self.gain += (1.0 - self.gain) * self.release;
        }
        x * self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, amplitude: f32, seconds: f32) -> Vec<i16> {
        let n = (DOWNLINK_RATE as f32 * seconds) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / DOWNLINK_RATE as f32;
                (amplitude * 32767.0 * (std::f32::consts::TAU * freq * t).sin()) as i16
            })
            .collect()
    }

    fn rms(pcm: &[i16]) -> f32 {
        let sum: f64 = pcm.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum();
        (sum / pcm.len() as f64).sqrt() as f32
    }

    #[test]
    fn peaks_stay_under_the_ceiling() {
        let mut downlink = Downlink::new(&VoiceShape::default()).unwrap();
        let loud = tone(3000.0, 0.99, 1.0);
        let shaped = downlink.shape_only(&loud);
        let peak = shaped.iter().map(|s| s.unsigned_abs()).max().unwrap() as f32 / 32768.0;
        assert!(peak <= db_to_linear(-10.0) + 0.001, "peak {peak}");
    }

    #[test]
    fn bass_is_cut_and_presence_kept() {
        let mut downlink = Downlink::new(&VoiceShape { gain_db: -20.0, ..VoiceShape::default() }).unwrap();
        let bass = downlink.shape_only(&tone(150.0, 0.5, 1.0));
        let mut downlink = Downlink::new(&VoiceShape { gain_db: -20.0, ..VoiceShape::default() }).unwrap();
        let presence = downlink.shape_only(&tone(3000.0, 0.5, 1.0));
        // Skip the filter warm-up.
        let bass = rms(&bass[2400..]);
        let presence = rms(&presence[2400..]);
        assert!(presence > bass * 20.0, "bass {bass}, presence {presence}");
    }

    #[test]
    fn packets_are_60_ms_and_decode() {
        let mut downlink = Downlink::new(&VoiceShape::default()).unwrap();
        let pcm = tone(440.0, 0.3, 0.25); // 6000 samples = 4 frames + 240
        let packets = downlink.push(&pcm).unwrap();
        assert_eq!(packets.len(), 4);
        let tail = downlink.flush().unwrap().expect("a padded last packet");
        let decoder = opus::Decoder::new(DOWNLINK_RATE, opus::Channels::Mono).unwrap();
        for packet in packets.iter().chain(std::iter::once(&tail)) {
            assert_eq!(decoder.get_nb_samples(packet).unwrap(), DOWNLINK_FRAME);
        }
        assert!(downlink.flush().unwrap().is_none());
    }

    #[test]
    fn uplink_decodes_robot_packets() {
        let mut encoder = opus::Encoder::new(UPLINK_RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let frame = vec![1000i16; UPLINK_FRAME];
        let mut packet = vec![0u8; 1500];
        let size = encoder.encode(&frame, &mut packet).unwrap();
        let mut uplink = Uplink::new().unwrap();
        assert_eq!(uplink.decode(&packet[..size]).unwrap().len(), UPLINK_FRAME * 2);
    }
}
