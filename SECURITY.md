# Security policy

## Reporting a vulnerability

Please report it privately through GitHub: the repository's **Security** tab, then
**Report a vulnerability** (a private security advisory). Do not open a public issue for
it. Say what you found, how to reproduce it, and what it lets someone do. You will get an
answer as soon as possible; botstack is a lab project kept by one person, so there is no
guaranteed delay.

Only the latest commit on the main branch is supported.

## What is secret

| Secret | Where | What it opens |
| --- | --- | --- |
| `.env` | the server (gitignored, mode 600, written by `scripts/setup.sh`) | every token below, and the xAI API key, which bills your account |
| `XAI_API_KEY` | `.env` | xAI's API, on your bill |
| `FLEET_ADMIN_TOKEN` | `.env` | `/fleet/mcp`: approve, lock, revoke and rename devices, make them speak |
| `GROK_BRIDGE_TOKEN` | `.env` | `/grok/mcp`: make the devices speak, take photos with their cameras, lock them |
| `GROK_BOT_WEBHOOK_URL`, `GROK_BOT_WEBHOOK_TOKEN` | `.env` | your Grok Bot routine's webhook. The token is required for that routine. An `http://` URL is refused unless `GROK_BOT_WEBHOOK_ALLOW_HTTP=1` |
| `ROBOT_TOKEN` and `firmware/robot-token` | `.env`, the computer that builds the firmware, and every firmware image | the bootstrap key: lets a new device introduce itself as `pending`, nothing more until you approve it |
| `firmware/signing-key.pem` | the computer that builds the firmware (gitignored, mode 600) | signs every firmware; devices install only updates signed with it |
| `firmware-p4/wifi.txt` | the computer that flashes the P4 board (gitignored) | your Wi-Fi passwords |

Never commit them, print them, paste them into an issue, or send them to the Docker build
context (`.dockerignore` keeps `.env` and the firmware directories out). The bridge logs
only whether each secret is set; it never logs a device key, and no MCP answer holds one.

## Known, accepted risks

- **The bootstrap key is in every image.** Anyone with a device and a USB cable can read
  it from flash (and the device's own key from NVS). It only lets a device wait as
  `pending`; revoke or rotate a device that leaves your hands, and rotate the bootstrap key
  ([firmware](docs/firmware.md#the-bootstrap-key-and-the-bridge-url)).
- **Approval window.** Someone with the bootstrap key who spoofs the MAC of an approved
  device that has no confirmed key yet could receive that device's key, between the
  approval and the device's first `enrolled`. Acceptable for a lab; approve a device while
  it is connected and check that `fleet_device` shows `"key": "confirmed"`.
- **No hardware secure boot.** Updates over the air must be signed, but anyone with the
  device and a USB cable can flash anything. Secure boot would burn eFuses for good.

Every public door, its credential and its limits: [docs/security.md](docs/security.md).
