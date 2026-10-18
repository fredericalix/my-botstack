//! Shape a 24 kHz mono WAV like the downlink and print its level.
//! `cargo run -p botstack-bridge --example shape -- in.wav out.wav [gain_db]`

use botstack_bridge::audio::{Downlink, VoiceShape};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut reader = hound::WavReader::open(&args[1])?;
    let pcm: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
    let mut shape = VoiceShape::default();
    if let Some(gain) = args.get(3) {
        shape.gain_db = gain.parse()?;
    }
    // Same shaping with no ceiling, to see how hard the limiter works.
    let open = Downlink::new(&VoiceShape { ceiling_dbfs: 0.0, ..shape.clone() })?.shape_only(&pcm);
    let shaped = Downlink::new(&shape)?.shape_only(&pcm);
    let over = open.iter().zip(&shaped).filter(|(a, b)| a.unsigned_abs() > b.unsigned_abs() + 30).count();
    println!("gain {} dB: limiter active on {:.1}% of samples", shape.gain_db, 100.0 * over as f64 / pcm.len() as f64);
    let mut writer = hound::WavWriter::create(&args[2], reader.spec())?;
    for s in &shaped {
        writer.write_sample(*s)?;
    }
    writer.finalize()?;
    for (name, data) in [("in", &pcm), ("out", &shaped)] {
        let frames: Vec<f64> = data
            .chunks(1440)
            .map(|f| (f.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / f.len() as f64).sqrt())
            .filter(|r| *r > 0.005)
            .collect();
        let rms = (frames.iter().map(|r| r * r).sum::<f64>() / frames.len() as f64).sqrt();
        let peak = data.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f64 / 32768.0;
        println!("{name}: speech rms {:.1} dBFS, peak {:.1} dBFS", 20.0 * rms.log10(), 20.0 * peak.log10());
    }
    Ok(())
}
