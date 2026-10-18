//! botstack-bridge server. Configuration comes from the environment; secrets
//! are never logged, only whether they are set.
//!
//! `botstack-bridge healthcheck` asks the running server's `/healthz` (on the
//! port of `BIND`) and exits 0 when it answers `ok`: the container has no curl.
//! It always dials 127.0.0.1, whatever address `BIND` names.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use botstack_bridge::{App, firmware, fleet, grokbot, http, realtime, slots, vision};
use tracing::{info, warn};

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Like `env`, but an empty or blank value also takes the default.
fn env_or(name: &str, default: &str) -> String {
    let value = env(name, "");
    if value.trim().is_empty() { default.to_string() } else { value.trim().to_string() }
}

/// The URL the robots use for one route: `explicit` when set, else
/// `{public}{path}`, else empty (the robots then refuse it).
fn public_url(explicit: &str, public: &str, path: &str) -> String {
    let explicit = explicit.trim();
    if !explicit.is_empty() {
        return explicit.to_string();
    }
    let public = public.trim().trim_end_matches('/');
    if public.is_empty() { String::new() } else { format!("{public}{path}") }
}

/// The bridge's public base URL: `PUBLIC_URL` when set, else
/// `https://` + `BOTSTACK_DOMAIN` (compose passes `.env` whole, and it cannot
/// expand one value into another), else empty.
fn public_base(public: &str, domain: &str) -> String {
    let public = public.trim();
    if !public.is_empty() {
        return public.to_string();
    }
    let domain = domain.trim().trim_end_matches('/');
    if domain.is_empty() { String::new() } else { format!("https://{domain}") }
}

/// `KEYTERMS`: a comma-separated list; blanks are dropped.
fn keyterms(raw: &str) -> Vec<String> {
    raw.split(',').map(str::trim).filter(|term| !term.is_empty()).map(str::to_string).collect()
}

fn timezone(name: &str) -> anyhow::Result<chrono_tz::Tz> {
    name.parse::<chrono_tz::Tz>()
        .map_err(|_| anyhow!("TIMEZONE {name:?} is not an IANA timezone, such as Europe/Paris or UTC"))
}

fn language(code: &str) -> anyhow::Result<String> {
    if !fleet::valid_language(code) {
        bail!("DEFAULT_LANGUAGE {code:?} is not an ISO 639-1 code, such as en or fr");
    }
    Ok(code.to_string())
}

/// The device approved on the first start (`GROK_DEVICE_ID`); none when empty.
fn seed(device: &str, name: &str, locked: impl FnOnce() -> bool) -> Option<fleet::Seed> {
    let device = device.trim();
    if device.is_empty() {
        return None;
    }
    Some(fleet::Seed { mac: device.to_string(), name: name.to_string(), locked: locked() })
}

/// The local address to check: the port of `BIND`, on the loopback.
fn health_address(bind: &str) -> Option<SocketAddr> {
    let port: u16 = bind.rsplit_once(':')?.1.parse().ok()?;
    Some(SocketAddr::from(([127, 0, 0, 1], port)))
}

/// An HTTP response with status 200 and the body `ok`.
fn healthy(response: &[u8]) -> bool {
    let text = String::from_utf8_lossy(response);
    let Some((head, body)) = text.split_once("\r\n\r\n") else { return false };
    let status = head.lines().next().unwrap_or("");
    status.split_whitespace().nth(1) == Some("200") && body.trim() == "ok"
}

fn healthcheck(bind: &str) -> i32 {
    let Some(address) = health_address(bind) else {
        eprintln!("BIND {bind:?} has no port");
        return 1;
    };
    let check = || -> std::io::Result<Vec<u8>> {
        let timeout = Duration::from_secs(3);
        let mut stream = TcpStream::connect_timeout(&address, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
        let mut response = Vec::new();
        stream.take(64 * 1024).read_to_end(&mut response)?;
        Ok(response)
    };
    match check() {
        Ok(response) if healthy(&response) => 0,
        Ok(_) => {
            eprintln!("{address}/healthz did not answer ok");
            1
        }
        Err(error) => {
            eprintln!("{address}/healthz: {error}");
            1
        }
    }
}

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("healthcheck") => std::process::exit(healthcheck(&env("BIND", "0.0.0.0:8080"))),
        Some(other) => bail!("unknown command {other:?}; usage: botstack-bridge [healthcheck]"),
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve())
}

async fn serve() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let device = env("GROK_DEVICE_ID", "").trim().to_lowercase();
    let api_key = env("XAI_API_KEY", "");
    let token = env("GROK_BRIDGE_TOKEN", "");
    let webhook_url = env("GROK_BOT_WEBHOOK_URL", "");
    let data = std::path::PathBuf::from(env("DATA_DIR", "/data"));
    let robot_token = env("ROBOT_TOKEN", "");
    let admin_token = env("FLEET_ADMIN_TOKEN", "");
    let voice = env("XAI_VOICE", "eve");
    let timezone = timezone(&env_or("TIMEZONE", "UTC"))?;
    let default_language = language(&env_or("DEFAULT_LANGUAGE", "en"))?;
    let persona = realtime::Persona {
        owner_name: env("OWNER_NAME", "").trim().to_string(),
        timezone,
        grok_bot_scope: env_or("GROK_BOT_SCOPE", realtime::DEFAULT_GROK_BOT_SCOPE),
        keyterms: keyterms(&env("KEYTERMS", "")),
    };
    let public = public_base(&env("PUBLIC_URL", ""), &env("BOTSTACK_DOMAIN", ""));
    let vision_url = public_url(&env("PUBLIC_VISION_URL", ""), &public, "/mcp/vision/explain");
    let firmware_url = public_url(&env("PUBLIC_FIRMWARE_URL", ""), &public, "/botstack/v1/firmware");
    let fleet = Arc::new(fleet::Fleet::load(
        fleet::Config {
            path: data.join("fleet.json"),
            bootstrap: robot_token.clone(),
            default_voice: voice.clone(),
            default_language: default_language.clone(),
            timezone,
            // The first start: the default device, approved, with the lock it had.
            seed: seed(&device, &env_or("GROK_DEVICE_NAME", "stackchan"), || {
                std::fs::read_to_string(data.join("lock")).is_ok_and(|text| text.trim() == "locked")
            }),
        },
        None,
    )?);
    let max_conversations: usize = env("MAX_CONVERSATIONS", "3").parse().context("MAX_CONVERSATIONS")?;
    info!(
        xai_key = !api_key.is_empty(),
        bridge_token = !token.is_empty(),
        bootstrap_key = robot_token.len(),
        fleet_admin_token = !admin_token.is_empty(),
        webhook = !webhook_url.is_empty(),
        webhook_https = webhook_url.starts_with("https://"),
        devices = fleet.entries().len(),
        default_device = !device.is_empty(),
        max_conversations,
        owner_name = !persona.owner_name.is_empty(),
        timezone = %timezone,
        default_language,
        grok_bot_scope = persona.grok_bot_scope,
        keyterms = persona.keyterms.len(),
        public_url = %public,
        "configuration"
    );
    if api_key.trim().is_empty() {
        warn!("XAI_API_KEY is not set: conversations, vision and speech will fail");
    }
    if vision_url.is_empty() || firmware_url.is_empty() {
        warn!(
            vision = !vision_url.is_empty(),
            firmware = !firmware_url.is_empty(),
            "PUBLIC_URL and BOTSTACK_DOMAIN are not set: robots cannot upload photos or download updates"
        );
    }

    let grokbot = Arc::new(grokbot::GrokBot::new(
        grokbot::Config {
            token,
            webhook_url,
            webhook_token: env("GROK_BOT_WEBHOOK_TOKEN", ""),
            fleet: fleet.clone(),
            default_device: device,
            pending_path: data.join("grok_pending.json"),
            rate_per_hour: 10,
        },
        None,
        None,
    ));
    let vision = vision::Vision::new(api_key.clone(), env("XAI_VISION_MODEL", "grok-4.3"), vision_url);
    let settings = realtime::Settings {
        api_key,
        url: env("XAI_REALTIME_URL", "wss://api.x.ai/v1/realtime"),
        model: env("XAI_VOICE_MODEL", "grok-voice-3.0"),
        voice,
        reasoning: env("XAI_REASONING", "none"),
        persona,
    };
    let idle: u64 = env("IDLE_CLOSE_SECONDS", "40").parse().context("IDLE_CLOSE_SECONDS")?;
    let app = App::new(
        grokbot,
        fleet,
        settings,
        vision,
        Duration::from_secs(idle),
        firmware::Firmware { dir: data.join("firmware"), url: firmware_url },
        slots::Slots::new(max_conversations),
        admin_token,
    );

    let bind = env("BIND", "0.0.0.0:8080");
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    info!(%bind, "botstack-bridge listening");
    axum::serve(listener, http::router(app).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal");
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url_derives_the_robot_urls() {
        assert_eq!(public_url("", "https://bot.example.com", "/mcp/vision/explain"), "https://bot.example.com/mcp/vision/explain");
        assert_eq!(public_url("", "https://bot.example.com/", "/botstack/v1/firmware"), "https://bot.example.com/botstack/v1/firmware");
        assert_eq!(public_url("https://other.example.com/v", "https://bot.example.com", "/mcp/vision/explain"), "https://other.example.com/v");
        assert_eq!(public_url("", "", "/mcp/vision/explain"), "");
        assert_eq!(public_url("  ", " ", "/botstack/v1/firmware"), "");
    }

    #[test]
    fn the_public_url_comes_from_the_domain_when_unset() {
        assert_eq!(public_base("", "bot.example.com"), "https://bot.example.com");
        assert_eq!(public_base("  ", " bot.example.com/ "), "https://bot.example.com");
        assert_eq!(public_base("https://other.example.com", "bot.example.com"), "https://other.example.com");
        assert_eq!(public_base("http://127.0.0.1:8080", ""), "http://127.0.0.1:8080");
        assert_eq!(public_base("", ""), "");
        let public = public_base("", "bot.example.com");
        assert_eq!(public_url("", &public, "/botstack/v1/firmware"), "https://bot.example.com/botstack/v1/firmware");
        assert_eq!(public_url("", &public, "/mcp/vision/explain"), "https://bot.example.com/mcp/vision/explain");
    }

    #[test]
    fn keyterms_are_a_trimmed_comma_list() {
        assert_eq!(keyterms(" Grok Bot, StackChan ,,  "), ["Grok Bot", "StackChan"]);
        assert!(keyterms("").is_empty());
        assert!(keyterms(" , ").is_empty());
    }

    #[test]
    fn timezone_and_language_are_checked() {
        assert_eq!(timezone("Europe/Paris").unwrap(), chrono_tz::Europe::Paris);
        assert_eq!(timezone("UTC").unwrap(), chrono_tz::UTC);
        assert!(timezone("Mars/Olympus").is_err());
        assert_eq!(language("fr").unwrap(), "fr");
        assert!(language("français").is_err());
        assert!(language("EN").is_err());
    }

    #[test]
    fn no_default_device_no_seed() {
        assert!(seed("", "stackchan", || panic!("the lock is read only for a seed")).is_none());
        assert!(seed("  ", "stackchan", || true).is_none());
        let seed = seed("02:00:00:00:00:01", "desk", || true).unwrap();
        assert_eq!((seed.mac.as_str(), seed.name.as_str(), seed.locked), ("02:00:00:00:00:01", "desk", true));
    }

    #[test]
    fn healthcheck_reads_the_bind_port_and_the_answer() {
        assert_eq!(health_address("0.0.0.0:8080"), Some(SocketAddr::from(([127, 0, 0, 1], 8080))));
        assert_eq!(health_address("[::]:9000"), Some(SocketAddr::from(([127, 0, 0, 1], 9000))));
        assert_eq!(health_address("localhost"), None);
        assert!(healthy(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok"));
        assert!(!healthy(b"HTTP/1.1 503 Service Unavailable\r\n\r\nok"));
        assert!(!healthy(b"HTTP/1.1 200 OK\r\n\r\nnot ok"));
        assert!(!healthy(b""));
    }

    #[test]
    fn healthcheck_asks_a_live_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /healthz HTTP/1.1\r\n"));
            stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok").unwrap();
        });
        assert_eq!(healthcheck(&format!("0.0.0.0:{port}")), 0);
        server.join().unwrap();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_eq!(healthcheck(&format!("0.0.0.0:{closed}")), 1);
    }
}
