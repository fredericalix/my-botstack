# Running the bridge

The bridge runs with Docker Compose on one server: `compose.yaml` starts two containers.

| Service | Image | What it does |
| --- | --- | --- |
| `bridge` | `botstack-bridge:local`, built from the `Dockerfile` (`bridge/`) | The bridge itself, listening on port 8080 inside the compose network only. Runs as user 65532, read-only root filesystem, no capabilities, `no-new-privileges`. Its health check is `botstack-bridge healthcheck` (the image has no curl). |
| `caddy` | `caddy:2.10` | The only container reachable from outside (ports 80 and 443). Gets and renews a Let's Encrypt certificate for `BOTSTACK_DOMAIN` and passes WebSocket upgrades through. Starts once the bridge is healthy. |

The [QUICKSTART](../QUICKSTART.md) walks through the first start. In short:

```sh
scripts/setup.sh                   # writes .env, firmware/robot-token, firmware/bridge-url
docker compose up -d --build
curl https://bot.example.com/grok/health      # {"ok":true}
docker compose logs -f bridge
```

## What reaches the bridge

`Caddyfile` routes only these paths to the bridge; everything else answers 404.

| Path | Who | What |
| --- | --- | --- |
| `/botstack/v1` | devices | the always-on WebSocket ([protocol](protocol.md)) |
| `/botstack/v1/firmware` | devices | the published firmware image ([updates](firmware.md#updates)) |
| `/mcp/vision/explain` | devices | camera photos |
| `/grok/*` | Grok Bot | `/grok/mcp`, `/grok/callback`; `/grok/health` is public and answers `{"ok":true}` |
| `/fleet/*` | admin AI | `/fleet/mcp` ([fleet](fleet.md#6-the-two-mcp-levels)) |

`/healthz` and `/metrics` answer inside the compose network only. Caddy sets
`X-Forwarded-For`; the bridge records it as the device's address (`fleet_device`).

The certificate must be publicly trusted: the firmware checks it against ESP-IDF's bundle
of public roots, so a self-signed or private CA certificate is refused. Caddy needs the
domain's DNS to point at the server and ports 80 and 443 open to get it. With
`BOTSTACK_DOMAIN=localhost` Caddy uses its own internal CA instead: fine for a test with
`fake_robot`, not for a real device.

## The data volume

The named volume `botstack-data` is the bridge's `/data`:

| File | What |
| --- | --- |
| `fleet.json` | the device registry: names, profiles, locks, the SHA-256 of each device key ([fleet](fleet.md)) |
| `grok_pending.json` | messages waiting for an offline device |
| `firmware/<project>.bin` | the published firmware images (`scripts/publish-firmware.sh`) |
| `lock` | older installs only: read once, at the first start with `GROK_DEVICE_ID` set, for that device's lock |

**Back up `fleet.json`.** Without it every device is unknown again: a device's own key no
longer matches anything, so it is refused once, erases its key, comes back with the
bootstrap key as `pending`, and must be approved again (with a new name and profile). A
copy:

```sh
docker compose exec -T bridge cat /data/fleet.json > fleet-backup.json
```

It holds no key in clear, only hashes; keep it private anyway (MACs, names, notes).
Caddy's certificates live in the volumes `caddy-data` and `caddy-config`.

## Configuration

Everything comes from `.env` (gitignored, mode 600), which `scripts/setup.sh` creates from
[`.env.example`](../.env.example). Edit it, then `docker compose up -d` to apply it (compose
recreates the bridge when its environment changed). A line commented out keeps the default;
do not leave a numeric setting empty, an empty value is not the default.

### Where the bridge is reached

| Variable | Default | Meaning |
| --- | --- | --- |
| `BOTSTACK_DOMAIN` | (required) | the server's public name, for Caddy's certificate; `scripts/setup.sh` asks for it |
| `PUBLIC_URL` | `https://` + `BOTSTACK_DOMAIN` | the bridge's public base URL; leave it empty unless the robots reach the bridge elsewhere. The robots upload photos to `{PUBLIC_URL}/mcp/vision/explain` and download updates from `{PUBLIC_URL}/botstack/v1/firmware`; with neither `PUBLIC_URL` nor `BOTSTACK_DOMAIN`, they can do neither (the bridge logs a warning) |
| `PUBLIC_VISION_URL` | from `PUBLIC_URL` | overrides the photo URL |
| `PUBLIC_FIRMWARE_URL` | from `PUBLIC_URL` | overrides the firmware URL |

### Secrets

| Variable | Meaning |
| --- | --- |
| `XAI_API_KEY` | xAI API key: realtime voice, text to speech, vision |
| `ROBOT_TOKEN` | the bootstrap key, the same as `firmware/robot-token` ([security](security.md)) |
| `FLEET_ADMIN_TOKEN` | the admin MCP's token (`/fleet/mcp`, `scripts/fleet.sh`); unset, `/fleet/mcp` answers 503 |
| `GROK_BRIDGE_TOKEN` | Grok Bot's token on `/grok/mcp` and `/grok/callback`; unset, both refuse everyone |
| `GROK_BOT_WEBHOOK_URL` | where questions for Grok Bot are posted. Empty: there is no Grok Bot ([Grok Bot](grok-bot.md)). Must be `https://`. An `http://` URL is refused at start, because the webhook key would travel in clear; `GROK_BOT_WEBHOOK_ALLOW_HTTP` set to `1`, `true`, or `yes` allows it for a local bridge and the log warns |
| `GROK_BOT_WEBHOOK_TOKEN` | sent as `Authorization: Bearer` with each webhook. **Required** for a Grok Bot routine (it rejects the post without the key). Leave it empty only when the URL is empty, or when another agent does not use a bearer token |
| `GROK_BOT_WEBHOOK_ALLOW_HTTP` | unset means off. `1`, `true`, or `yes` allow an `http://` webhook URL, for local development only. Any other value leaves it off |

The bridge never logs a secret, only whether each one is set (and the bootstrap key's
length): look for the `configuration` line in `docker compose logs bridge`.

### Who the robots talk to

| Variable | Default | Meaning |
| --- | --- | --- |
| `OWNER_NAME` | empty | your first name, in the voice instructions; empty: “the user” |
| `TIMEZONE` | `UTC` | IANA timezone for the date in the instructions and the daily minutes; an unknown name stops the bridge at start |
| `DEFAULT_LANGUAGE` | `en` | ISO 639-1 language of new devices; each device can be changed with `fleet_set_profile` |
| `GROK_BOT_SCOPE` | `their accounts, mail, calendar and files` | what Grok Bot can reach, as the voice describes it |
| `KEYTERMS` | empty | comma list of words the speech recognition should expect (names) |
| `GROK_DEVICE_ID` | empty | a MAC approved from the first start, without `fleet_approve`. A reply goes to the `device_id` it names, else to the device that asked, else to this MAC while it is approved, else (when this is empty) to the only approved device. `stackchan_say` and `stackchan_photo` without `device_id` use this MAC only; they do not use the only approved device. With no approved default, a reply that names no device is refused and not queued |
| `GROK_DEVICE_NAME` | `stackchan` | the name of that device |

### Voice and limits

| Variable | Default | Meaning |
| --- | --- | --- |
| `XAI_VOICE_MODEL` | `grok-voice-3.0` | realtime voice model (xAI accepts any name and falls back to its latest model without an error: check `session.created`) |
| `XAI_VOICE` | `eve` | default voice; each device can have its own |
| `XAI_REASONING` | `none` | `high` adds ~0.2 s and longer answers |
| `XAI_VISION_MODEL` | `grok-4.3` | camera answers |
| `XAI_REALTIME_URL` | `wss://api.x.ai/v1/realtime` | realtime endpoint |
| `MAX_CONVERSATIONS` | `3` | simultaneous xAI sessions, all devices together ([fleet](fleet.md#4-conversation-cap)) |
| `IDLE_CLOSE_SECONDS` | `40` | close a silent conversation |
| `GROK_BOT_REPLY_TIMEOUT` | `180` | seconds to wait for `stackchan_reply` after the webhook accepts the question; then the robot says Grok Bot did not answer. From 1 to 3600 (a request is kept for one hour). `0` or a larger value stops the bridge at start |

### Inside the container

| Variable | Default | Meaning |
| --- | --- | --- |
| `BIND` | `0.0.0.0:8080` | listen address (Caddy expects 8080) |
| `DATA_DIR` | `/data` | the data volume |
| `RUST_LOG` | `info` | log filter (`debug` for the link's details). `info` records transcripts of what is said; `warn` leaves them out |

## Updating

```sh
git pull
docker compose up -d --build
```

The bridge restarts: every device's link drops for a few seconds and reconnects (a device
restarts after 20 s without the bridge, which is harmless). Firmware updates are separate:
`scripts/publish-firmware.sh firmware|firmware-p4` ([updates](firmware.md#updates)).

## Metrics

`/metrics` (inside the compose network) exposes `botstack_realtime_seconds_total` (xAI
realtime time, for the bill; also per `device`), `botstack_devices_online` and
`botstack_conversations_active`. To scrape it, add your Prometheus to the compose network,
or publish the bridge's port on the loopback (the commented `ports:` line in
`compose.yaml`).
