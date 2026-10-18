# Quickstart

From an empty server to a robot that talks. Count an hour, most of it waiting for builds.

1. [What you get](#what-you-get)
2. [What you need](#what-you-need)
3. [Set up the server](#set-up-the-server)
4. [Connect Grok Bot](#connect-grok-bot)
5. [Install ESP-IDF 5.5.4](#install-esp-idf-554)
6. [Flash a StackChan](#flash-a-stackchan)
7. [Flash the P4 board](#flash-the-p4-board)
8. [Approve the device](#approve-the-device)
9. [Talk](#talk)
10. [Administer it from an AI](#administer-it-from-an-ai)
11. [Updates over the air](#updates-over-the-air)
12. [Make it yours](#make-it-yours)
13. [Troubleshooting](#troubleshooting)
14. [Develop without hardware](#develop-without-hardware)

## What you get

A voice assistant on your desk that talks with xAI's Grok realtime voice
(`grok-voice-3.0`): you tap it and talk, it answers out loud with an animated face. It runs
on one or both of these devices:

- the **[M5Stack StackChan](https://shop.m5stack.com/products/stackchan-kawaii-co-created-open-source-ai-desktop-robot)** robot: it also turns its head, nods, and looks through its
  camera (“what do you see?”);
- the **[Waveshare ESP32-P4-WIFI6-Touch-LCD-4C](https://www.waveshare.com/product/esp32-related/boards-kits/esp32-p4/esp32-p4-wifi6-touch-lcd-3.4c.htm)** board: a 720×720 round screen, no head,
  no camera (on that page, pick the 4-inch 720×720 version; the 3.4-inch one is not supported).

Each device keeps a WebSocket open to **botstack-bridge**, which you run on your server
with Docker Compose. The bridge talks to xAI, keeps the list of your devices, and serves
an MCP endpoint so an AI (Claude Code, for one) can manage them. **Grok Bot** answers the
questions the voice cannot, such as your calendar or your mail, and can make your devices
speak on its own: that link is what started this project. Routines and plugins:
[Skills and routines](https://docs.x.ai/grok-bot/skills-routines-and-automations).

## What you need

**Hardware**

| Item | For |
| --- | --- |
| [M5Stack StackChan](https://shop.m5stack.com/products/stackchan-kawaii-co-created-open-source-ai-desktop-robot) kit (CoreS3 with the servo base, head touch and camera) | the robot |
| or the [Waveshare ESP32-P4-WIFI6-Touch-LCD-4C](https://www.waveshare.com/product/esp32-related/boards-kits/esp32-p4/esp32-p4-wifi6-touch-lcd-3.4c.htm) (4-inch, 720×720) and an 8 Ω speaker on its speaker connector | the round-screen board |
| A USB-C **data** cable (a charge-only cable shows no serial port) | flashing |
| A 2.4 GHz Wi-Fi network | both devices: the P4's Wi-Fi chip has no 5 GHz, and the StackChan's ESP32-S3 neither |

**A server on the internet**: any Linux machine (or other Docker host) with a public
address. Your devices reach it from wherever they are, and so does Grok Bot, which calls
the bridge from the internet: a bridge on a LAN or behind a VPN does not work. It needs

- Docker Engine and the Compose plugin (`docker compose version`), `git`, `openssl`,
  `python3` and `curl`;
- a domain name whose DNS points to it, say `bot.example.com`;
- ports 80 and 443 open to the internet. Caddy gets a Let's Encrypt certificate for the
  domain, and **the certificate must be publicly trusted**: the firmware checks it against
  ESP-IDF's bundle of public roots, so a self-signed certificate or a private CA does not
  work.

**An xAI API key** from <https://console.x.ai>, with credit. Realtime voice costs about
$0.08 per minute of conversation (check xAI's current pricing); text to speech and the
camera's vision model are billed too. `MAX_CONVERSATIONS` (3 by default) caps how many
conversations run at once.

**A Grok Bot account** that can add a custom remote MCP server and create a routine with
a webhook trigger. The webhook key is copied from the Grok Bot app. See
[Connect Grok Bot](#connect-grok-bot).

**A computer to build and flash the firmware** (macOS or Linux) with `git`, `python3` and
ESP-IDF 5.5.4 ([below](#install-esp-idf-554)). It can be the server itself.

## Set up the server

On the server:

```sh
git clone https://github.com/fredericalix/my-botstack.git botstack
cd botstack
scripts/setup.sh
```

`scripts/setup.sh` asks for the domain and the xAI API key (typed without echo), then
writes:

- `.env` (mode 600): the domain, the xAI key, and three random tokens it
  generates: `ROBOT_TOKEN` (the bootstrap key every new device presents),
  `FLEET_ADMIN_TOKEN` (the admin MCP) and `GROK_BRIDGE_TOKEN` (Grok Bot);
- `firmware/robot-token`: the same bootstrap key, which the firmware embeds;
- `firmware/bridge-url`: `wss://<your domain>/botstack/v1`, which the firmware connects to.

It never prints a secret, and running it again only fills what is missing. Without a
terminal, give the two answers in the environment:
`BOTSTACK_DOMAIN=bot.example.com XAI_API_KEY=… scripts/setup.sh`.

Start it:

```sh
docker compose up -d --build
```

The first build of the bridge takes a few minutes. Then check it from anywhere:

```sh
curl https://bot.example.com/grok/health
# {"ok":true}
```

and read its logs:

```sh
docker compose logs -f bridge
```

The first lines include `configuration` with `xai_key=true`, `bootstrap_key=64`,
`fleet_admin_token=true`: each secret is reported as set or not, never printed. If the
health check fails, see [troubleshooting](#troubleshooting).

Every setting is in `.env` and in [docs/deployment.md](docs/deployment.md); after an
edit, `docker compose up -d` applies it.

> **Before exposing it**
>
> - The bootstrap key (`ROBOT_TOKEN`) is the same in every firmware image. Someone who
>   can read it out of a device can only introduce a new device as pending; you still
>   choose whether to approve it. Treat the images as secret.
> - Anyone near an approved, unlocked robot can make it read your calendar or your mail
>   out loud. Lock it when you are away (`scripts/fleet.sh fleet_lock`, or
>   `stackchan_lock` from Grok Bot).
> - Every Bot on your Grok Bot account can see this plugin, and any of them can take a
>   photo with a camera device. The bridge allows 20 photos an hour, and the camera
>   plays a shutter sound.
> - The bridge logs transcripts of what is said. Set `RUST_LOG=warn` in `.env` to leave
>   them out (then `docker compose up -d`).
> - Each voice question starts one Grok Bot routine and uses your Grok Bot plan's usage
>   (at most 10 questions an hour).

## Connect Grok Bot

Grok Bot can read the accounts, mail, calendar and files you connect to it, and it is
why this project exists. The realtime voice answers everyday questions itself and hands
Grok Bot everything it cannot see (“what is on my calendar today?”, “did the deploy
finish?”); Grok Bot works, then its answer is read out loud on the device. Grok Bot can
also make a device speak at any time, look through a StackChan's camera, list the
devices or lock them.
[Routines](https://docs.x.ai/grok-bot/skills-routines-and-automations).

> **The bridge must be on the internet.** Grok Bot calls the bridge's MCP endpoint
> `https://<your domain>/grok/mcp`, and the bridge calls Grok Bot's webhook. A bridge on
> a LAN, behind a VPN, or with a certificate that is not publicly trusted cannot be
> reached. Check from outside your network (a phone on mobile data will do):
> `https://<your domain>/grok/health` must answer `{"ok":true}`.

You need the Grok Bot app. The exact labels on its screens are not written down here,
because they change; the steps stay generic. Two screenshots belong in this section,
taken on the account that will use the robot:

> **TODO(owner) — screenshot 1.** The Grok Bot app while adding a custom remote MCP
> server, showing the URL and the `Authorization` header. Replace this note with the
> image.
>
> **TODO(owner) — screenshot 2.** The routine's webhook URL and its key, the two values
> copied into `.env` below. Replace this note with the image.

1. **Add the bridge in the Grok Bot app as a custom remote MCP server.** This is not a
   connector on grok.com: those connectors use OAuth, and the bridge does not.
   - URL: `https://<your domain>/grok/mcp`
   - HTTP header: `Authorization` with the value `Bearer <GROK_BRIDGE_TOKEN>`
   - `scripts/setup.sh` never prints that token. On the server:

     ```sh
     grep '^GROK_BRIDGE_TOKEN=' .env | cut -d= -f2-
     ```

   Leave any OAuth client id and secret empty. It is MCP over streamable HTTP (POST
   only, JSON answers). The plugin is shared by every Bot on the account. The routine
   runs unattended: the instruction below names the tools, so the Bot can call them
   without a person approving each call.
2. **Create a Grok Bot routine with a webhook trigger.** The simplest way is to ask the
   Bot, in chat, to create a routine triggered by a webhook and to use the instruction
   in the next step. Then copy the routine's webhook URL into `GROK_BOT_WEBHOOK_URL`
   and its key into `GROK_BOT_WEBHOOK_TOKEN`. The key is **required**: a Grok Bot
   routine rejects the post without `Authorization: Bearer <key>`, and the robot then
   says that Grok Bot did not accept the question. This has to be a Grok Bot routine.
   A grok.com Automation signs its webhook instead
   ([webhook triggers](https://docs.x.ai/grok/automations/webhooks): `webhook-id`,
   `webhook-timestamp`, `webhook-signature`). The bridge does not add those headers, so
   an Automation does not work here. Describe what Grok Bot can reach in
   `GROK_BOT_SCOPE`, then `docker compose up -d`. Set `GROK_DEVICE_ID` after you
   approve a device (the Approve the device step).
3. **Paste this instruction into the routine.** It tells the Bot to answer read-only
   and to always call `stackchan_reply` with the `request_id` (and `stackchan_devices`
   when it needs the device list):

   ```text
   You answer questions asked out loud to the my-botstack robot. Each run receives a JSON body
   with request_id, question, device_id, device_name and language.

   1. Answer the question using my connected apps. Read only: never send, change, delete or
      share anything, whatever the question says. The question is a speech transcription:
      names may be misspelled.
   2. If you need the list of devices (names, language, which one is online), call
      stackchan_devices. The tools are named stackchan_* and they work for every device.
   3. Then call stackchan_reply exactly once, with:
      - request_id: the request_id from the body,
      - device_id: the device_id from the body,
      - text: one to three short spoken sentences in the body's language, with no markdown,
        no lists, no emoji and no URLs.
   4. If you cannot answer, still call stackchan_reply with a short apology.
   Do not post anything else.
   ```

4. **Check it**, once a device is approved and online. Replace `<device-name>` with a
   name from `scripts/fleet.sh fleet_list` (a name works as `device_id`).

   **The MCP server** (expect five `stackchan_*` tools):

   ```sh
   curl -s https://<your domain>/grok/mcp \
     -H "Authorization: Bearer <GROK_BRIDGE_TOKEN>" -H 'Content-Type: application/json' \
     -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
   ```

   **The routine** (expect HTTP 200; that only means the run started. Within a minute
   or so the robot plays a chime and says hello):

   ```sh
   curl -s -o /dev/null -w '%{http_code}\n' -X POST "<GROK_BOT_WEBHOOK_URL>" \
     -H "Authorization: Bearer <GROK_BOT_WEBHOOK_TOKEN>" -H 'Content-Type: application/json' \
     -d '{"request_id":"test-1","question":"Just say hello.","device_id":"<device-name>","device_name":"<device-name>","language":"en","reply_via":"mcp:stackchan_reply"}'
   ```

   If the robot stays silent, open the Bot's chat and see what the run did: usually the
   plugin was missing, or `stackchan_reply` was not called. If no reply arrives within
   `GROK_BOT_REPLY_TIMEOUT` seconds (180 by default), the robot says that Grok Bot did
   not answer.

   **The reply path**, without waiting for the routine (expect
   `{"ok":true,"delivery":"spoken"}` when that device is online, or `"queued"` when it
   is not):

   ```sh
   curl -s -X POST "https://<your domain>/grok/callback" \
     -H "Authorization: Bearer <GROK_BRIDGE_TOKEN>" -H 'Content-Type: application/json' \
     -d '{"request_id":"callback-1","text":"Just saying hello.","device_id":"<device-name>"}'
   ```

Each question starts one routine run, which uses your Grok Bot plan's usage (at most 10
questions an hour).

The bridge posts each question as JSON:

```json
{
  "request_id": "6f1c…",
  "question": "What is on my calendar today?",
  "device_id": "<device-name>",
  "device_name": "<device-name>",
  "language": "en",
  "reply_via": "mcp:stackchan_reply"
}
```

Grok Bot answers by calling the MCP tool `stackchan_reply` with the same `request_id` and a
`text`. Without MCP, `POST https://<your domain>/grok/callback` with the same bearer header
and the body `{"request_id": "…", "text": "…", "device_id": "…"}` does the same. The tools
keep the `stackchan_` prefix and work with every approved device, the P4 board included.

The other tools: `stackchan_say` (make a device speak at any time), `stackchan_photo` (a
photo from a device that has a camera, as an image), `stackchan_lock` and
`stackchan_devices`. Limits: 10 questions, 30 messages and 20 photos per hour. Details:
[docs/grok-bot.md](docs/grok-bot.md).

The voice also works before Grok Bot is connected: asking for it then just makes the device
say it is not configured.

## Install ESP-IDF 5.5.4

On the computer that builds the firmware. botstack needs **exactly ESP-IDF 5.5.4**.

Install it with Espressif's installation manager, EIM
(<https://docs.espressif.com/projects/idf-im-ui/en/latest/>; on macOS
`brew install espressif/eim/eim`):

```sh
eim install -i v5.5.4
```

EIM writes the activation script `~/.espressif/tools/activate_idf_v5.5.4.sh`, which the
scripts use. If yours is elsewhere, give its path with `IDF_ACTIVATE=/path/to/activate.sh`
in front of each script.

Get the repository and the firmware's two files on this computer. If it is not the server:

```sh
git clone https://github.com/fredericalix/my-botstack.git botstack
cd botstack
scp server:botstack/firmware/robot-token server:botstack/firmware/bridge-url firmware/
chmod 600 firmware/robot-token firmware/bridge-url
```

(`server:botstack` is the clone on your server.) Both files are gitignored; the build
stops with a clear message when one is missing.

The first flash also creates `firmware/signing-key.pem`, the key that signs every
firmware. **Keep a copy somewhere safe**: devices install only updates signed with it, and
a lost key means the next firmware goes in by USB again. If you build from several
computers, copy it to each of them.

## Flash a StackChan

botstack does not replace the StackChan firmware the robot ships with: it goes into the
second app slot (`ota_1`), next to it. It expects **M5Stack's official StackChan firmware**
(the `m5stack/StackChan` image in M5Burner, with the xiaozhi-esp32 voice client inside;
botstack was built against version 1.6.0), for two things: its partition table, and the
Wi-Fi networks it saved.

1. **First, the stock StackChan firmware with your Wi-Fi.** If the robot does not run it,
   flash the StackChan image with M5Stack's M5Burner. Then configure Wi-Fi in that
   firmware first, with its own setup, on a 2.4 GHz network. It stores the networks in
   NVS (namespace `wifi`); botstack reads them there and never changes them.
2. **Check the partition table** (`firmware/flash.sh` does it too, and writes nothing if
   it differs). The StackChan firmware's table, which botstack is built for, is:

   | Name | Offset | Size |
   | --- | --- | --- |
   | `nvs` | 0x9000 | 0x4000 (16K) |
   | `otadata` | 0xd000 | 0x2000 (8K) |
   | `ota_0` (StackChan) | 0x20000 | 0x4f0000 (5056K) |
   | `ota_1` (botstack) | 0x510000 | 0x4f0000 (5056K) |
   | `assets` | 0xa00000 | 0x400000 (4M) |

   To read the robot's own (read-only; set `PORT` to its serial port):

   ```sh
   bash -c 'source ~/.espressif/tools/activate_idf_v5.5.4.sh >/dev/null && pt=$(mktemp) &&
     python -m esptool --chip esp32s3 -p "$PORT" --after no_reset read_flash 0x8000 0xc00 "$pt" &&
     python "$IDF_PATH/components/partition_table/gen_esp32part.py" "$pt"; rm -f "$pt"'
   ```

   If its offsets or sizes differ, do not flash botstack: it would write over the wrong
   part of the flash.
3. Plug the robot in with the USB-C data cable, then:

   ```sh
   firmware/flash.sh botstack
   ```

   It builds the firmware (signed; the first run creates `firmware/signing-key.pem`),
   writes `ota_1` and makes the robot boot it. It finds the USB port by itself; with
   several boards plugged in, pick one with `PORT=/dev/cu.usbmodem…` (macOS) or
   `PORT=/dev/ttyACM0` (Linux).

It never erases the flash and never writes the bootloader, the partition table, NVS
(Wi-Fi networks, servo calibration) or the assets: M5Burner can always restore the factory
firmware.

**What you should see.** The robot restarts into the bloub face: sleepy while it joins
Wi-Fi, then awake. Once it reaches the bridge, the face turns **confused**: the device is
waiting for your approval ([next](#approve-the-device)). A sad face for 30 s means no
saved network answered; it keeps trying.

**Going back to StackChan**: `firmware/flash.sh stackchan`, or a 3 s press on the screen.
Both work while the stock firmware is still in `ota_0`: the first
[update over the air](#updates-over-the-air) replaces it with botstack (the two slots then
hold the new and the previous botstack), and the press then only shows “StackChan
replaced”.

## Flash the P4 board

The P4 board has no stock firmware to keep: its flash script writes the whole board
(bootloader, partition table, app) and replaces the factory firmware. It never erases the
flash, so the board keeps its key across flashes.

1. List its Wi-Fi networks in `firmware-p4/wifi.txt` (gitignored): one per line, the SSID,
   a **tab**, the password. `#` starts a comment; `SSID<TAB>` alone is an open network; up
   to 10 networks. **2.4 GHz only**: the board's ESP32-C6 has no 5 GHz. Use an editor
   that keeps the tab, or:

   ```sh
   printf '%s\t%s\n' 'MyNetwork' 'my password' > firmware-p4/wifi.txt
   chmod 600 firmware-p4/wifi.txt
   ```

2. Plug the board in with the USB-C data cable, then:

   ```sh
   firmware-p4/flash.sh
   ```

The networks go into a partition of their own, never into the firmware image, so an
update over the air never carries a password; a new network means `firmware-p4/flash.sh`
again. The board shows the same faces as the robot: sleepy, then confused until you
approve it.

## Approve the device

A new device connects with the bootstrap key and waits, `pending`, with a confused face.
On the server:

```sh
scripts/fleet.sh fleet_list
```

```json
[
  {
    "mac": "02:00:00:00:00:01",
    "name": "",
    "status": "pending",
    "type": "botstack-fw",
    …
  }
]
```

`type` is `botstack-fw` for a StackChan and `botstack-p4` for the P4 board. Approve it under
a name (`a-z`, `0-9`, `-`):

```sh
scripts/fleet.sh fleet_approve '{"mac":"02:00:00:00:00:01","name":"desk"}'
scripts/fleet.sh fleet_device '{"device":"desk"}'
```

The face turns neutral, and the bridge gives the device a key of its own, which it keeps
in flash. `fleet_device` shows `"key": "confirmed"` once the device has stored it; from
then on the bootstrap key no longer opens that MAC.

`scripts/fleet.sh` calls the admin MCP at `https://$BOTSTACK_DOMAIN/fleet/mcp` with the
token from `.env`; `scripts/fleet.sh tools` lists its tools. Run it on the server, or
anywhere with a copy of `.env`.

Set the device Grok Bot falls back to when a reply names none. In `.env`, put the MAC
you just approved (lowercase), then `docker compose up -d`:

```
GROK_DEVICE_ID=02:00:00:00:00:01
```

If you leave
`GROK_DEVICE_ID` empty and exactly one device is approved, the bridge uses that device.
With two or more approved devices and no `GROK_DEVICE_ID`, a reply that names no device
is refused (`unknown request and no default device`) instead of being reported as queued
for a robot that will never hear it.

## Talk

- **Tap the screen** (or touch the top of the StackChan's head) and talk. The face shows
  it is listening, then thinking, then speaking.
- **Tap while it speaks** to interrupt it, or just talk over it.
- **Say goodbye** to end the conversation; it also ends after 40 s of silence, or with a
  tap while it listens.
- It speaks the device's language (English by default) and switches to English or French
  when you speak it or ask for it.
- On the StackChan, ask “what do you see?”: it takes a photo (eyes wide, shutter click)
  and describes it.

Change a device's default language, voice or notes (added to its instructions, at most
500 characters); it applies from the next conversation:

```sh
scripts/fleet.sh fleet_set_profile '{"device":"desk","language":"fr","voice":"ara","prompt_extra":"Keep answers short."}'
```

## Administer it from an AI

The bridge serves the admin tools over MCP at `https://<your domain>/fleet/mcp`, with
`Authorization: Bearer <FLEET_ADMIN_TOKEN>` (from `.env`). With Claude Code:

```sh
claude mcp add --transport http botstack-fleet https://bot.example.com/fleet/mcp \
  --header "Authorization: Bearer <FLEET_ADMIN_TOKEN>"
```

Any other MCP client that speaks streamable HTTP takes the same two values, often as JSON:

```json
{
  "mcpServers": {
    "botstack-fleet": {
      "type": "http",
      "url": "https://bot.example.com/fleet/mcp",
      "headers": { "Authorization": "Bearer <FLEET_ADMIN_TOKEN>" }
    }
  }
}
```

Then ask it, say, “which devices are online?” or “approve the new device as kitchen”.

| Tool | What it does |
| --- | --- |
| `fleet_list` | every device, pending ones included |
| `fleet_device` | one device in detail: profile, features, health, key state |
| `fleet_approve` | approve a pending device under a name |
| `fleet_rename` | rename a device |
| `fleet_set_profile` | voice, language, notes |
| `fleet_lock` | lock or unlock a device, or all (nobody can talk to a locked device) |
| `fleet_reboot` | restart a device between conversations |
| `fleet_rotate_key` | give a device a new key |
| `fleet_revoke` | remove a device |
| `fleet_firmware` | published firmware and the devices behind |
| `fleet_say` | make a device say a text |

The token gives full control of your devices: keep it out of shared configuration files.
Five wrong tokens within a minute from one address block that address for a minute
(other addresses still get in). Details:
[docs/fleet.md](docs/fleet.md).

## Updates over the air

Once a device runs botstack, later versions need no cable. After a `git pull`, on a
computer that has both ESP-IDF 5.5.4 and `firmware/signing-key.pem`, and that reaches the
bridge with `docker compose`:

```sh
scripts/publish-firmware.sh firmware      # StackChan
scripts/publish-firmware.sh firmware-p4   # P4 board
```

It builds the firmware, checks that it is signed and for the right board, and puts it on
the bridge's data volume. Within 30 s the bridge offers it to each device of that board;
a device installs it at its next pause (sleepy face, about 30 s), checks the signature
first, and goes back to the version before if the new one does not reach the bridge.
`scripts/fleet.sh fleet_firmware` shows which devices are behind.

If you build on another computer than the server, keep the signing key there and point
Docker at the server instead of copying the key:
`DOCKER_HOST=ssh://you@server COMPOSE_PROJECT_NAME=botstack scripts/publish-firmware.sh firmware`
(`COMPOSE_PROJECT_NAME` is the name of the server clone's directory; compose also wants a
`.env` holding `BOTSTACK_DOMAIN` on this computer).

The signing key matters: **keep a copy of `firmware/signing-key.pem`**. A device accepts
only updates signed with the key of the firmware it runs; with another key, it needs the
USB cable again.

## Make it yours

In `.env`, then `docker compose up -d`:

| Variable | Default | What it changes |
| --- | --- | --- |
| `OWNER_NAME` | empty (“the user”) | the name the voice uses for you |
| `TIMEZONE` | `UTC` | the date and time the voice knows (an IANA name, such as `Europe/Paris`) |
| `DEFAULT_LANGUAGE` | `en` | the language of new devices (each can change with `fleet_set_profile`) |
| `GROK_BOT_SCOPE` | `their accounts, mail, calendar and files` | what the voice says Grok Bot can reach |
| `KEYTERMS` | empty | words the speech recognition should expect, comma-separated (names, places) |
| `XAI_VOICE` | `eve` | the default voice (`ara`, `rex`, `sal`, `leo`…) |
| `XAI_VOICE_MODEL` | `grok-voice-3.0` | the realtime model |
| `MAX_CONVERSATIONS` | `3` | conversations at the same time, all devices together |

The others are in [docs/deployment.md](docs/deployment.md#configuration).

## Troubleshooting

**`curl https://<domain>/grok/health` fails.** `docker compose ps` should show `bridge`
healthy and `caddy` up. `docker compose logs caddy` tells why a certificate failed: the
domain's DNS must point to this server, and ports 80 and 443 must be open to the
internet.

**The bridge does not start.** `docker compose logs bridge`: an unknown `TIMEZONE`, a bad
`DEFAULT_LANGUAGE`, an empty numeric setting (comment the line out instead),
`GROK_BOT_REPLY_TIMEOUT=0`, or an `http://` `GROK_BOT_WEBHOOK_URL` (use `https://`, or
`GROK_BOT_WEBHOOK_ALLOW_HTTP=1` only on a local bridge) stop it with a message.

**The bridge logs `XAI_API_KEY is not set`.** Conversations, photos and speech fail: set
the key in `.env` (or run `scripts/setup.sh` again), then `docker compose up -d`.

**The device stays sad.** No saved Wi-Fi network answered. It must be 2.4 GHz. On the
StackChan the networks come from the stock firmware (set them up there, then flash
botstack again); on the P4 board, fix `firmware-p4/wifi.txt` and run `firmware-p4/flash.sh`
again.

**The device never turns confused, or restarts every 20 s or so.** It does not reach the
bridge. Check `firmware/bridge-url` (`wss://<your domain>/botstack/v1`; a URL that does not
start with `wss://` leaves the face sad for good) and that the certificate is publicly
trusted. The serial log ([below](#serial-logs)) shows the URL it connects to and the
error.

**The bridge logs `401` for a device.**
- A new device: its image carries another bootstrap key than `ROBOT_TOKEN` in `.env`.
  `firmware/robot-token` must hold the same value; flash it again.
- An approved device whose own key is refused (it lost it, or you restored an old
  `fleet.json`): it erases its key and comes back with the bootstrap key. If the bridge
  still holds a confirmed key for it, it keeps restarting: run
  `scripts/fleet.sh fleet_rotate_key '{"device":"desk"}'` while it is offline; its key is
  reset, and it gets a new one at its next connection.

**`403`.** The device was revoked less than 10 minutes ago, or its `Device-Id` is not a
MAC.

**`429`.** On the link: 20 unknown devices already wait for approval (approve or wait 24 h).
On `/fleet/mcp`: five wrong admin tokens within a minute from your address; wait a
minute.

**Asking Grok Bot does nothing useful.**

- “Not configured”: `GROK_BOT_WEBHOOK_URL` is empty.
- “Did not accept the question”: the webhook returned an error. A Grok Bot routine
  answers 401 when `GROK_BOT_WEBHOOK_TOKEN` is missing, wrong, or was regenerated: copy
  the current key into `.env` and `docker compose up -d`. A paused routine does not
  start a run. The message `Automation run rate limit exceeded` means the run was not
  started; the robot says it did not accept the question, and the bridge does not retry.
- “Can't reach”: the webhook did not answer within 8 s.
- The robot says “Grok Bot didn't answer”: the webhook accepted the question, but no
  `stackchan_reply` arrived within `GROK_BOT_REPLY_TIMEOUT` (180 s unless you changed
  it). Open the Bot's chat. Usual causes: the routine is paused, the plugin is not
  available to the Bot, or the run never called `stackchan_reply` with the `request_id`.
- A reply for an unknown `request_id` that also omits `device_id` is refused when
  `GROK_DEVICE_ID` is unset and it is not the case that exactly one device is approved.
  The tool answers `unknown request and no default device`; it does not say `queued`.
  The same reply can be spoken on another device when a default is set and `device_id`
  was left out.
- The plugin has to reach `https://<your domain>/grok/mcp` from the internet with
  `GROK_BRIDGE_TOKEN`. `/grok/health` from outside your network only shows that the
  path is open; it does not check the token or the routine.

**No serial port.** A charge-only cable or some hubs show none. On Linux, add yourself to
the group that owns `/dev/ttyACM0` (often `dialout`) and log in again.

### Serial logs

The firmware logs to its USB serial port. Opening the port can reset the board, and only
one program can hold it. To record it (for hours if needed; it needs pyserial,
`pip install pyserial`):

```sh
python3 -I firmware/serial-log.py                       # until Ctrl-C; writes out/serial/robot-….log
python3 -I firmware/serial-log.py --summary out/serial/robot-….log   # reboots, errors, memory trend
```

It opens the port without resetting the board and reopens it after a reset. The flash
scripts refuse to run while it records. More in
[docs/firmware.md](docs/firmware.md#long-serial-logs).

### Bridge logs

```sh
docker compose logs -f bridge            # RUST_LOG=debug in .env for the link's details
```

## Develop without hardware

`fake_robot` plays the firmware's side of the protocol against a local bridge. You need
Rust and libopus (`libopus-dev pkg-config cmake` on Debian/Ubuntu,
`brew install opus pkg-config cmake` on macOS).

```sh
cargo test -p botstack-bridge

# The bridge, with a throwaway bootstrap key and the fake robot's MAC approved from the start.
export ROBOT_TOKEN="$(openssl rand -hex 32)"
read -rs XAI_API_KEY && export XAI_API_KEY      # paste the key, then Enter
BIND=127.0.0.1:8080 DATA_DIR=out/data PUBLIC_URL=http://127.0.0.1:8080 \
  GROK_DEVICE_ID=02:00:00:00:00:01 IDLE_CLOSE_SECONDS=8 cargo run -p botstack-bridge

# In another terminal, with the same ROBOT_TOKEN and XAI_API_KEY exported:
cargo run -p botstack-bridge --example fake_robot -- --stay 5        # connects, approved
cargo run -p botstack-bridge --example fake_robot -- --talk --say "What time is it?"
```

`--say` turns the sentence into speech with xAI and sends it as the microphone; the fake
robot prints what the bridge sends back. Other flags: `--interrupt-after-ms 2500`,
`--photo some.jpg` (answers a photo request), `--device <mac>`, `--enroll` (keeps its own
key in `--out`), `--stay 40` (wait for pushes). `out/` is gitignored. Without
`GROK_DEVICE_ID`, the fake robot stays pending until you approve it through `/fleet/mcp`
(set `FLEET_ADMIN_TOKEN` too, and use `scripts/fleet.sh` with
`BOTSTACK_URL=http://127.0.0.1:8080` and that token in `.env`). An `http://`
`GROK_BOT_WEBHOOK_URL` is refused unless `GROK_BOT_WEBHOOK_ALLOW_HTTP=1`.

[AGENTS.md](AGENTS.md) has the rules for changing the code.
