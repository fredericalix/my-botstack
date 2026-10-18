# Grok Bot

![Grok Bot: replies, pushes, photos and the lock](img/grok-bot.svg)

Grok Bot is xAI's cloud agent, slower than the realtime voice but connected to your mail,
calendar, accounts or files (`GROK_BOT_SCOPE` tells the voice what it can reach). The
voice model calls it when you ask for something only it can see, or when you name it; it
never listens to you directly. It is the reason botstack exists. The bridge speaks to it
through a webhook and an MCP server, so any agent that can receive a webhook and call MCP
can take its place.

The bridge must be reachable from the internet over publicly trusted HTTPS: Grok Bot runs
in xAI's cloud and calls `https://<your domain>/grok/mcp`. Until Grok Bot is connected,
asking for it just makes the robot say it is not configured.

## Setting it up

1. Give the bridge the webhook: `GROK_BOT_WEBHOOK_URL` (HTTPS) and, if your agent wants
   one, `GROK_BOT_WEBHOOK_TOKEN` (sent as `Authorization: Bearer`). Then
   `docker compose up -d`.
2. Give Grok Bot the bridge's MCP server, as a custom connector on grok.com enabled for
   Grok Bot: `https://<your domain>/grok/mcp`, with
   `Authorization: Bearer <GROK_BRIDGE_TOKEN>` (from `.env`). It is MCP over streamable
   HTTP (POST only, JSON answers). If your agent asks for approval before each tool call,
   set these tools to always allowed: an approval card would expire before anyone sees it.
3. Tell your agent, in its instructions, to answer every webhook run by calling
   `stackchan_reply` once with the run's `request_id`, in one to three spoken sentences in
   the request's `language`, without markdown or URLs.

## Asking (A)

1. You ask something only Grok Bot can see. The voice model calls `ask_grok_bot` and says
   in a few words that it is asking.
2. The bridge posts the webhook (8 s timeout; any 2xx is accepted):

   ```json
   {
     "request_id": "6f1c…",
     "question": "What is on my calendar today?",
     "device_id": "02:00:00:00:00:01",
     "device_name": "desk",
     "language": "en",
     "reply_via": "mcp:stackchan_reply"
   }
   ```

   `question` is at most 1000 characters, in the conversation's language; `language` is
   the device's (ISO 639-1). If the post fails, the robot says so.
3. Grok Bot works, then answers with the MCP tool `stackchan_reply(request_id, text)`, or
   without MCP with `POST /grok/callback` (same Bearer token, JSON body
   `{"request_id": "…", "text": "…", "device_id": "…"}`, `device_id` optional).
4. If the conversation is still open, the robot plays a chime and reads the answer word
   for word (xAI text to speech, in the device's voice), after anything it is already
   saying. If it has closed, the answer is pushed at once (below). If the robot is
   offline, the answer waits in `grok_pending.json` and is spoken as soon as it
   reconnects.

A reply whose `request_id` is unknown (bridge restart, older than an hour) goes to the
default device (`GROK_DEVICE_ID`), or to the `device_id` it names. A `device_id` that is
not an approved device is refused, for replies as for pushes. Texts are cut at 500
characters.

## Pushing (B)

A Grok Bot agent can call `stackchan_say(text, device_id?)` at any time. `device_id` is a
device name or a MAC; `stackchan_devices` lists the approved devices (name, MAC, type,
online, locked, language, state, features). Without `device_id` the text goes to the
default device (`GROK_DEVICE_ID`) if it is approved, otherwise the answer is "unknown
device" ([fleet](fleet.md)). A device is always connected ([botstack/v1](protocol.md)), so
an idle device speaks at once: the `notify` animation, a chime, the message (xAI TTS),
then a short conversation listens for 10 s so you can answer. The message never cuts a
conversation: it waits for its end. If the robot is offline, the message waits in
`grok_pending.json` and is spoken right after it reconnects; a message cut by a dropped
link goes back there too.

## Photo (C)

A Grok Bot agent can call `stackchan_photo(device_id?)` to see through a device's camera. A
device without the `camera` feature is refused with a clear message. The
bridge sends the robot `tool camera.take_photo`, the face opens its eyes wide, and the
robot uploads one 320×240 JPEG to the vision URL as for its own photos. The `question`
field carries `botstack-photo:<key>` instead of a question, so the bridge hands the JPEG
to the waiting MCP call instead of the vision model. The tool result is the photo as MCP
image content (`image/jpeg`, base64) plus one line of text. It takes 2–4 s; a photo works
during a conversation too. If the robot is offline or no photo comes within 30 s, the
result is an error with the reason. Nothing is stored.

## Lock (D)

`stackchan_lock(locked, device_id?)` locks or unlocks one device, or the whole fleet without
`device_id`; without `locked` it only reports the state. The admin MCP has the same lock
(`scripts/fleet.sh fleet_lock '{"device":"desk","locked":true}'`). While a device is
locked, nobody can talk to it: a tap or a head touch plays the `alert` animation and opens
nothing, its face stays wary (`mefiant`), and a pushed message is spoken without the short
listen after it. Locking closes an open conversation at once. Pushes (`stackchan_say`) and
photos (`stackchan_photo`) still work. The state is kept per device in `/data/fleet.json`,
so a bridge restart does not unlock it.

## Limits

| Rule | Why |
| --- | --- |
| Only approved devices may ask, and replies and pushes go only to them | a stray `device_id` would fill the pending file |
| 10 questions per hour, for the whole server | the device id is client input |
| 30 pushes per hour | a runaway agent cannot keep the robot talking |
| 20 photos per hour | the camera can be triggered from afar |
| 5 pending messages per device | the oldest are dropped |

Keep your agent read-only: a voice request should never change a server, a repository or
an account.

All the doors, keys and limits in one picture: [security](security.md).
