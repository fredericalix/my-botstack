//! A fake StackChan speaking botstack/v1 (docs/protocol.md), to test the bridge
//! without the robot.
//!
//! It keeps the connection open like the firmware, follows the `state` the
//! bridge sends (microphone only while listening or thinking), plays voice and
//! answers `played`, logs expressions and animations, and answers tools
//! (`camera.take_photo` posts `--photo` to the vision URL from the hello).
//!
//! cargo run -p botstack-bridge --example fake_robot -- --talk --say "Quelle heure est-il ?"
//! cargo run -p botstack-bridge --example fake_robot -- --stay 40     (wait for pushes)
//! --barge-in-after-ms 2000: say the next --say sentence over the robot's first answer.
//! Fleet (docs/fleet.md): --device <mac>, --firmware <project>, --features head,camera,
//! --status (health every 60 s), --enroll (keeps its own key in <out>/key; deletes it
//! after a 401, then the next run uses the bootstrap key). --enroll needs --device
//! with another MAC than the robot's.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const MIC_RATE: u32 = 16_000;
const MIC_FRAME: usize = 960;
const SPEAKER_RATE: u32 = 24_000;
const FRAME: Duration = Duration::from_millis(60);

struct Args {
    url: String,
    device: String,
    talk: bool,
    say: Vec<String>,
    interrupt_after_ms: Option<u64>,
    barge_in_after_ms: Option<u64>,
    stay: u64,
    photo: Option<PathBuf>,
    out: PathBuf,
    firmware: String,
    features: Vec<String>,
    status: bool,
    enroll: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        url: "ws://127.0.0.1:8080/botstack/v1".into(),
        device: "02:00:00:00:00:01".into(),
        talk: false,
        say: Vec::new(),
        interrupt_after_ms: None,
        barge_in_after_ms: None,
        stay: 60,
        photo: None,
        out: "out/fake-robot".into(),
        firmware: "fake-robot".into(),
        features: Vec::new(),
        status: false,
        enroll: false,
    };
    let mut device_given = false;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().context("missing value");
        match flag.as_str() {
            "--url" => args.url = value()?,
            "--device" => {
                args.device = value()?;
                device_given = true;
            }
            "--talk" => args.talk = true,
            "--say" => args.say.push(value()?),
            "--interrupt-after-ms" => args.interrupt_after_ms = Some(value()?.parse()?),
            "--barge-in-after-ms" => args.barge_in_after_ms = Some(value()?.parse()?),
            "--stay" => args.stay = value()?.parse()?,
            "--photo" => args.photo = Some(value()?.into()),
            "--out" => args.out = value()?.into(),
            "--firmware" => args.firmware = value()?,
            "--features" => args.features = value()?.split(',').filter(|f| !f.is_empty()).map(str::to_string).collect(),
            "--status" => args.status = true,
            "--enroll" => args.enroll = true,
            other => bail!("unknown flag {other}"),
        }
    }
    // Under the robot's MAC it would confirm a key the real robot does not hold,
    // and lock the robot out.
    if (args.enroll || args.features.iter().any(|f| f == "enroll")) && !device_given {
        bail!("--enroll needs another MAC than the robot's, such as --device aa:bb:cc:dd:ee:01");
    }
    if args.enroll && !args.features.iter().any(|f| f == "enroll") {
        args.features.push("enroll".into());
    }
    if args.status && !args.features.iter().any(|f| f == "status") {
        args.features.push("status".into());
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    std::fs::create_dir_all(&args.out)?;
    let mut questions: VecDeque<Vec<i16>> = VecDeque::new();
    if !args.say.is_empty() {
        let key = std::env::var("XAI_API_KEY").context("XAI_API_KEY is needed for --say")?;
        for text in &args.say {
            questions.push_back(synthesise(&key, text).await?);
        }
    }

    let mut request = args.url.as_str().into_client_request()?;
    request.headers_mut().insert("Device-Id", args.device.parse()?);
    // Its own key once enrolled, else the bootstrap key (scripts/setup.sh), as the firmware does.
    let key_path = args.out.join("key");
    let own_key = args
        .enroll
        .then(|| std::fs::read_to_string(&key_path).ok())
        .flatten()
        .map(|key| key.trim().to_string())
        .filter(|key| key.len() == 64);
    let key = match &own_key {
        Some(key) => key.clone(),
        None => std::env::var("ROBOT_TOKEN").context("ROBOT_TOKEN is needed (the bootstrap key)")?,
    };
    println!("key: {}", if own_key.is_some() { "own (enrolled)" } else { "bootstrap" });
    request.headers_mut().insert("Authorization", format!("Bearer {key}").parse()?);
    let (socket, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(connected) => connected,
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 401 && own_key.is_some() => {
            // Like the firmware: an own key the bridge refuses is dropped.
            std::fs::remove_file(&key_path)?;
            bail!("the bridge refused this robot's own key; it is deleted, run again to use the bootstrap key");
        }
        Err(error) => return Err(error).context("connect"),
    };
    let (mut tx, mut rx) = socket.split();
    let start = Instant::now();
    let t = |at: Instant| at.duration_since(start).as_millis();
    tx.send(Message::text(
        json!({"type": "hello", "protocol": "botstack/v1", "firmware": args.firmware,
               "mic": {"codec": "opus", "rate": MIC_RATE, "frame_ms": 60},
               "speaker": {"codec": "opus", "rate": SPEAKER_RATE, "frame_ms": 60}, "features": args.features})
        .to_string(),
    ))
    .await?;

    let mut encoder = opus::Encoder::new(MIC_RATE, opus::Channels::Mono, opus::Application::Voip)?;
    let mut decoder = opus::Decoder::new(SPEAKER_RATE, opus::Channels::Mono)?;
    let mut packet = vec![0u8; 1500];
    let mut decoded = vec![0i16; 5760];
    let (side_tx, mut side_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    let mut state: String;
    let mut mic_on = false;
    let mut vision: (String, String) = Default::default();
    let mut mic: VecDeque<i16> = VecDeque::new();
    let mut speaking = false;
    let mut speech_started = start;
    let mut speeches: Vec<(u128, usize, f32)> = Vec::new();
    let mut packets = 0usize;
    let mut samples = 0usize;
    let mut dropped = 0usize;
    let mut interrupted = false;
    let mut play_until = start;
    let mut mic_clock = tokio::time::interval(FRAME);
    let deadline = start + Duration::from_secs(args.stay);
    let mut hello_seen = false;
    let mut status_clock = tokio::time::interval(Duration::from_secs(60));

    loop {
        tokio::select! {
            message = rx.next() => {
                let now = Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let event: Value = serde_json::from_str(&text)?;
                        let kind = event["type"].as_str().unwrap_or("");
                        match kind {
                            "hello" => {
                                // A second hello comes at approval, with the vision token.
                                vision = (event["vision"]["url"].as_str().unwrap_or("").into(), event["vision"]["token"].as_str().unwrap_or("").into());
                                let first = !hello_seen;
                                hello_seen = true;
                                // The token itself is never printed.
                                let token = if vision.1.is_empty() { "none".to_string() } else { format!("set ({} chars)", vision.1.len()) };
                                println!("{:>6} ms <- hello name {} approved {} {} vision token {token}", t(now), event["name"], event["approved"], event["voice"]);
                                if args.talk && first {
                                    tx.send(Message::text(json!({"type": "talk", "action": "start"}).to_string())).await?;
                                    println!("{:>6} ms -> talk start", t(now));
                                }
                            }
                            "state" => {
                                state = event["value"].as_str().unwrap_or("").to_string();
                                mic_on = event["mic"].as_bool().unwrap_or(state == "listening" || state == "thinking");
                                println!("{:>6} ms <- state {state} mic {mic_on}", t(now));
                                if state == "listening"
                                    && let Some(q) = questions.pop_front() {
                                        mic.extend(q);
                                    }
                            }
                            "speech" => {
                                println!("{:>6} ms <- speech {}", t(now), event["action"].as_str().unwrap_or(""));
                                if event["action"] == "cancel" {
                                    speaking = false;
                                    speeches.push((t(speech_started), packets, samples as f32 / SPEAKER_RATE as f32));
                                    tx.send(Message::text(json!({"type": "played"}).to_string())).await?;
                                    println!("{:>6} ms -> played (cancelled)", t(Instant::now()));
                                } else if event["action"] == "start" {
                                    speaking = true;
                                    speech_started = now;
                                    packets = 0;
                                    samples = 0;
                                } else {
                                    speaking = false;
                                    speeches.push((t(speech_started), packets, samples as f32 / SPEAKER_RATE as f32));
                                    // Drain, like the firmware, then say so.
                                    tokio::time::sleep(play_until.saturating_duration_since(Instant::now())).await;
                                    tx.send(Message::text(json!({"type": "played"}).to_string())).await?;
                                    println!("{:>6} ms -> played", t(Instant::now()));
                                }
                            }
                            "expression" | "animation" => println!("{:>6} ms <- {kind} {}", t(now), event["value"]),
                            "tool" => {
                                println!("{:>6} ms <- tool {} {}", t(now), event["name"], event["args"]);
                                let id = event["id"].clone();
                                if event["name"] == "camera.take_photo" {
                                    let (url, token) = vision.clone();
                                    let photo = args.photo.clone();
                                    let device = args.device.clone();
                                    let question = event["args"]["question"].as_str().unwrap_or("").to_string();
                                    let side = side_tx.clone();
                                    tokio::spawn(async move {
                                        let (ok, result) = match upload_photo(&url, &token, &device, photo, &question).await {
                                            Ok(text) => (true, text),
                                            Err(e) => (false, e.to_string()),
                                        };
                                        let _ = side.send(json!({"type": "tool_result", "id": id, "ok": ok, "result": result}).to_string());
                                    });
                                } else {
                                    tx.send(Message::text(json!({"type": "tool_result", "id": id, "ok": true, "result": "done"}).to_string())).await?;
                                }
                            }
                            "enroll" => {
                                let key = event["key"].as_str().unwrap_or("");
                                let id = event["id"].as_str().unwrap_or("");
                                if !args.enroll {
                                    println!("{:>6} ms <- enroll (ignored: no --enroll)", t(now));
                                } else if key.len() == 64 && key.chars().all(|c| c.is_ascii_hexdigit()) {
                                    write_key(&key_path, key)?;
                                    tx.send(Message::text(json!({"type": "enrolled", "id": id}).to_string())).await?;
                                    println!("{:>6} ms <- enroll: own key kept in {} -> enrolled {id}", t(now), key_path.display());
                                } else {
                                    println!("{:>6} ms <- enroll with a malformed key", t(now));
                                }
                            }
                            "reboot" => {
                                println!("{:>6} ms <- reboot: the fake robot stops here", t(now));
                                break;
                            }
                            other => println!("{:>6} ms <- {other} {text}", t(now)),
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        if !speaking {
                            dropped += 1;
                            continue;
                        }
                        packets += 1;
                        let n = decoder.decode(&data, &mut decoded, false)?;
                        samples += n;
                        play_until = play_until.max(now) + FRAME;
                        if let Some(ms) = args.barge_in_after_ms
                            && !interrupted && mic_on && now.duration_since(speech_started).as_millis() as u64 > ms
                                && let Some(q) = questions.pop_front() {
                                    interrupted = true;
                                    mic.extend(q);
                                    println!("{:>6} ms -> talking over the robot", t(now));
                                }
                        if let Some(ms) = args.interrupt_after_ms
                            && !interrupted && now.duration_since(speech_started).as_millis() as u64 > ms {
                                interrupted = true;
                                tx.send(Message::text(json!({"type": "interrupt"}).to_string())).await?;
                                println!("{:>6} ms -> interrupt", t(now));
                            }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        println!("{:>6} ms bridge closed the connection", t(now));
                        break;
                    }
                    Some(Err(error)) => {
                        println!("{:>6} ms socket error: {error}", t(now));
                        break;
                    }
                    _ => {}
                }
            }
            Some(text) = side_rx.recv() => {
                println!("{:>6} ms -> {}", t(Instant::now()), &text[..text.len().min(140)]);
                tx.send(Message::text(text)).await?;
            }
            _ = status_clock.tick(), if args.status && hello_seen => {
                let status = json!({"type": "status", "rssi": -55, "heap_internal": 80000, "heap_psram": 4_000_000,
                                    "uptime_s": start.elapsed().as_secs(), "reset_reason": "poweron",
                                    "head": "none", "camera": if args.photo.is_some() { "ok" } else { "none" }});
                tx.send(Message::text(status.to_string())).await?;
                println!("{:>6} ms -> status", t(Instant::now()));
            }
            _ = mic_clock.tick() => {
                if Instant::now() > deadline {
                    println!("{:>6} ms done", t(Instant::now()));
                    break;
                }
                if !mic_on {
                    continue;
                }
                let frame: Vec<i16> = (0..MIC_FRAME).map(|_| mic.pop_front().unwrap_or(0)).collect();
                let size = encoder.encode(&frame, &mut packet)?;
                tx.send(Message::binary(packet[..size].to_vec())).await?;
            }
        }
    }
    println!();
    for (i, (at, n, secs)) in speeches.iter().enumerate() {
        println!("speech {}: at {at} ms, {n} packets, {secs:.2} s", i + 1);
    }
    println!("voice packets outside speech: {dropped}");
    Ok(())
}

async fn upload_photo(url: &str, token: &str, device: &str, photo: Option<PathBuf>, question: &str) -> Result<String> {
    let jpeg = std::fs::read(photo.context("no --photo given")?)?;
    let form = reqwest::multipart::Form::new()
        .text("question", question.to_string())
        .part("file", reqwest::multipart::Part::bytes(jpeg).file_name("camera.jpg").mime_str("image/jpeg")?);
    let response = reqwest::Client::new()
        .post(url)
        .bearer_auth(token)
        .header("Device-Id", device)
        .multipart(form)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("vision HTTP {}", response.status());
    }
    Ok(response.text().await?)
}

async fn synthesise(key: &str, text: &str) -> Result<Vec<i16>> {
    let response = reqwest::Client::new()
        .post("https://api.x.ai/v1/tts")
        .bearer_auth(key)
        .json(&json!({"text": text, "voice_id": "rex", "language": "fr", "output_format": {"codec": "pcm", "sample_rate": MIC_RATE}}))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("tts HTTP {}", response.status());
    }
    let bytes = response.bytes().await?;
    let mut pcm = vec![0i16; MIC_RATE as usize / 2];
    pcm.extend(bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes([b[0], b[1]])));
    Ok(pcm)
}

/// The robot's own key, readable by its owner only.
fn write_key(path: &std::path::Path, key: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.write_all(key.as_bytes())?;
    Ok(())
}
