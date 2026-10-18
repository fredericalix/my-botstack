"""Generates the remaining botstack doc diagrams as plain SVG."""
import math, sys
OUT = sys.argv[1]
FONT = 'font-family="-apple-system, Segoe UI, Helvetica, Arial, sans-serif"'
COL = {"robot": ("#fff4e8", "#f08a24", "#111", "#444"), "bridge": ("#eaf1ff", "#2f6fdf", "#111", "#444"),
       "xai": ("#16181d", "#16181d", "#fff", "#cfd5df"), "bot": ("#f1ebff", "#7c4dff", "#111", "#5b3cc4"),
       "note": ("#fffbe6", "#e0c96b", "#6b5a10", "#6b5a10"), "gray": ("#f3f4f6", "#9aa7b8", "#111", "#444"),
       "store": ("#eef8f0", "#2e9e5b", "#111", "#2b6b43")}

class Svg:
    def __init__(s, w, h, title):
        s.w, s.h = w, h
        s.o = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" {FONT}>',
               '<defs><marker id="a" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5" markerHeight="6.5" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>',
               f'<rect width="{w}" height="{h}" fill="#fff"/>',
               f'<text x="{w/2}" y="34" text-anchor="middle" font-size="22" font-weight="700" fill="#111">{title}</text>']
    def box(s, x, y, w, h, kind, head, lines=(), size=14):
        f, st, tc, sc = COL[kind]
        s.o.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="12" fill="{f}" stroke="{st}" stroke-width="2"/>')
        top = y + h / 2 - (len(lines) * 17) / 2 + (5 if not lines else -2)
        s.o.append(f'<text x="{x+w/2}" y="{top}" text-anchor="middle" font-size="{size}" font-weight="700" fill="{tc}">{head}</text>')
        for i, l in enumerate(lines):
            s.o.append(f'<text x="{x+w/2}" y="{top+19+i*17}" text-anchor="middle" font-size="12" fill="{sc}">{l}</text>')
    def arrow(s, x1, y1, x2, y2, label="", color="#555", dy=-7, anchor="middle", dash=False, dx=0):
        d = ' stroke-dasharray="6 5"' if dash else ""
        s.o.append(f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{color}" stroke-width="2"{d} marker-end="url(#a)"/>')
        if label:
            for i, part in enumerate(label.split("|")):
                s.o.append(f'<text x="{(x1+x2)/2+dx}" y="{(y1+y2)/2+dy+i*15}" text-anchor="{anchor}" font-size="12" fill="#222">{part}</text>')
    def text(s, x, y, t, size=12, color="#444", anchor="start", weight="400"):
        s.o.append(f'<text x="{x}" y="{y}" text-anchor="{anchor}" font-size="{size}" font-weight="{weight}" fill="{color}">{t}</text>')
    def raw(s, t):
        s.o.append(t)
    def save(s, name):
        open(f"{OUT}/{name}", "w").write("\n".join(s.o + ["</svg>"]))

# ---------------- Grok Bot ----------------
g = Svg(1000, 900, "Grok Bot: replies, pushes, photos and the lock")
g.text(20, 72, "A. You ask something only Grok Bot can see", 15, "#5b3cc4", weight="700")
g.box(20, 90, 170, 80, "robot", "You", ["“What is on my", "calendar today?”"])
g.box(230, 90, 190, 80, "xai", "Realtime voice", ["calls ask_grok_bot,", "says it is asking"])
g.box(460, 90, 170, 80, "bridge", "Bridge", ["POST webhook", "request_id + question"])
g.box(670, 90, 170, 80, "bot", "Grok Bot", ["reads mail, calendar,", "accounts…"])
g.arrow(192, 130, 228, 130); g.arrow(422, 130, 458, 130); g.arrow(632, 130, 668, 130)
g.arrow(755, 172, 755, 222, "MCP stackchan_reply|(request_id, text)", "#7c4dff", -4, "end", dx=-8)
g.box(560, 225, 280, 60, "bridge", "Bridge receives the answer")
g.box(300, 320, 250, 74, "robot", "Conversation still open", ["chime, then xAI TTS reads it", "word for word (after any reply)"], 13)
g.box(600, 320, 240, 74, "store", "Conversation closed", ["notify, chime, spoken at once;", "offline: kept until reconnect"], 13)
g.arrow(640, 287, 470, 318, "yes", "#555", -2); g.arrow(720, 287, 720, 318, "no", "#555", -2, "start")
g.text(20, 452, "B. A Grok Bot agent pushes a message on its own", 15, "#5b3cc4", weight="700")
g.box(20, 470, 210, 70, "bot", "stackchan_say(text)", ["any time, 30 per hour max"], 13)
g.box(270, 470, 240, 70, "bridge", "Conversation open?", ["yes: wait for its end", "no: spoken at once"], 13)
g.box(550, 470, 290, 70, "robot", "Notify, chime, then spoken", ["then listens 10 s for an answer"], 13)
g.arrow(232, 505, 268, 505); g.arrow(512, 505, 548, 505)
g.box(870, 90, 110, 120, "gray", "Limits", ["approved only", "10 asks/h", "30 pushes/h", "20 photos/h", "5 kept offline"], 13)
g.text(20, 592, "C. Grok Bot asks for a photo", 15, "#5b3cc4", weight="700")
g.box(20, 610, 170, 74, "bot", "stackchan_photo", ["20 per hour"], 13)
g.box(230, 610, 210, 74, "bridge", "Bridge", ["tool camera.take_photo", "question botstack-photo:‹key›"], 12)
g.box(480, 610, 200, 74, "robot", "Robot", ["eyes wide, shutter click,", "JPEG 320×240"], 13)
g.box(720, 610, 260, 74, "bridge", "/mcp/vision/explain", ["sees the prefix: the JPEG goes", "back to the waiting call"], 12)
g.arrow(192, 647, 228, 647); g.arrow(442, 647, 478, 647); g.arrow(682, 647, 718, 647, "upload", "#555", -6)
g.raw('<polyline points="850,686 850,712 105,712 105,688" fill="none" stroke="#7c4dff" stroke-width="2" marker-end="url(#a)"/>')
g.text(478, 730, "MCP image content (image/jpeg), shown in Grok Bot · error after 30 s or when the robot is offline", 12, "#5b3cc4", "middle")
g.text(20, 782, "D. Lock the robot", 15, "#5b3cc4", weight="700")
g.box(20, 800, 210, 80, "bot", "stackchan_lock", ["or fleet_lock", "on · off · status"], 13)
g.box(270, 800, 210, 80, "store", "Bridge", ["saves fleet.json:", "a restart keeps it"], 13)
g.box(520, 800, 220, 80, "robot", "Locked robot", ["wary face (mefiant), open", "conversation closed, taps → alert"], 12)
g.box(780, 800, 200, 80, "note", "Still works", ["Grok Bot replies and pushes", "(no listen after), photos"], 12)
g.arrow(232, 840, 268, 840); g.arrow(482, 840, 518, 840)
g.save("grok-bot.svg")

# ---------------- Camera ----------------
c = Svg(1000, 450, "“What do you see?” — the camera tool")
c.box(20, 90, 170, 76, "xai", "Realtime voice", ["says “Let me look”,", "take_photo(question)"], 13)
c.box(250, 90, 210, 76, "bridge", "Bridge", ["tool request", "camera.take_photo"], 13)
c.box(530, 90, 200, 76, "robot", "Robot camera", ["eyes wide, shutter click,", "GC0308 → JPEG 320×240"], 13)
c.box(530, 270, 200, 76, "bridge", "/mcp/vision/explain", ["on the bridge, checks", "the Bearer token"], 13)
c.box(790, 270, 190, 76, "xai", "grok-4.3", ["describes the photo in", "the device's language, 3–5 s"], 13)
c.arrow(192, 118, 248, 118, "1", "#555", -6)
c.arrow(462, 118, 528, 118, "2", "#555", -6)
c.arrow(600, 168, 600, 268, "3. POST photo|+ question", "#555", -6, "end", dx=-8)
c.arrow(732, 300, 788, 300, "4", "#555", -6)
c.arrow(788, 322, 732, 322, "5. text", "#2f6fdf", 18)
c.arrow(660, 268, 660, 168, "6. HTTP response|= the text", "#2f6fdf", -6, "start", dx=8)
c.arrow(528, 146, 462, 146, "7. tool_result", "#2f6fdf", 20)
c.arrow(248, 146, 192, 146, "", "#2f6fdf")
c.text(220, 186, "8. function output", 12, "#222", "middle")
c.text(20, 390, "The robot learns the vision URL (HTTPS only) and a per-process token from the bridge's hello; nothing else may post photos.", 12.5, "#444")
c.text(20, 410, "A Grok Bot photo takes the same road with the question botstack-photo:‹key›: the bridge returns the JPEG to Grok Bot instead.", 12.5, "#444")
c.text(20, 430, "Head moves are tool requests too: the robot answers at once (“Done.” or “The head does not answer.”); the bridge waits ≤ 3 s.", 12.5, "#444")
c.save("camera.svg")

# ---------------- Flash layout ----------------
f = Svg(1000, 350, "16 MB flash: two app slots")
parts = [("nvs", 0x9000, 0x4000, "gray", "Wi-Fi, servo zeros"), ("otadata", 0xD000, 0x2000, "note", "which app boots"),
         ("phy", 0xF000, 0x1000, "gray", ""), ("ota_0", 0x20000, 0x4F0000, "gray", "StackChan, until the first update"),
         ("ota_1", 0x510000, 0x4F0000, "robot", "botstack"), ("assets", 0xA00000, 0x400000, "gray", "StackChan assets"),
         ("coredump", 0xE00000, 0x10000, "gray", "")]
x0, width, total = 20, 960, 0x1000000
def px(addr): return x0 + width * addr / total
y = 80
for name, off, size, kind, desc in parts:
    a, b = px(off), px(off + size)
    w = max(b - a, 6)
    fl, st, tc, sc = COL[kind]
    f.raw(f'<rect x="{a:.1f}" y="{y}" width="{w:.1f}" height="70" fill="{fl}" stroke="{st}" stroke-width="1.5"/>')
    if size >= 0x400000:
        f.text(a + w / 2, y + 32, name, 14, tc, "middle", "700")
        f.text(a + w / 2, y + 52, desc, 12, sc, "middle")
        f.text(a + 2, y + 88, f"0x{off:06X}", 11, "#6b7787")
f.text(px(0x9000), y - 8, "nvs · otadata · phy", 11, "#6b7787")
f.arrow(px(0x510000) + 60, 210, px(0x510000) + 60, 156, "", "#f08a24")
f.text(px(0x510000) + 70, 205, "./flash.sh botstack writes only ota_1 + otadata", 13, "#b25f10", weight="700")
f.arrow(px(0xD000) + 30, 250, px(0xD000) + 3, 156, "", "#e0c96b")
f.text(px(0xD000) + 40, 248, "otadata picks the app; ./flash.sh stackchan or a 3 s press switches back to ota_0 while StackChan is there", 12.5, "#6b5a10")
f.text(20, 290, "An update over the air goes to the slot that is not running: the first one replaces StackChan, then the two slots alternate.", 12.5, "#444")
f.text(20, 310, "Never written: the bootloader, the partition table, NVS (Wi-Fi credentials, servo zero offsets) and assets.", 12.5, "#444")
f.text(20, 330, "An updated app that has not reached the bridge yet is rolled back to the other slot on the next reset.", 12.5, "#444")
f.save("flash-layout.svg")

# ---------------- Updates over the air ----------------
u = Svg(1000, 470, "An update over the air")
u.box(20, 80, 200, 86, "gray", "Your computer", ["scripts/publish-firmware.sh", "build, signed with", "signing-key.pem"], 13)
u.box(290, 80, 220, 86, "store", "Bridge /data", ["firmware/botstack-fw.bin", "build = ELF SHA-256, 8 bytes"], 13)
u.box(580, 80, 400, 86, "robot", "Robot, running build A", ["gets firmware {build B, url}: not its own,", "not rolled back before → waits for a pause"], 13)
u.arrow(222, 123, 288, 123, "copy", "#555", -6)
u.arrow(512, 123, 578, 123, "≤ 30 s", "#2f6fdf", -6)
u.box(580, 210, 400, 76, "robot", "Idle, nothing playing", ["sleepy face, taps ignored; GET /botstack/v1/firmware", "with its key → the other app slot"], 13)
u.arrow(780, 168, 780, 208)
u.box(580, 330, 190, 86, "robot", "Signature OK", ["signed with the key", "of build A: boot B"], 13)
u.box(790, 330, 190, 86, "note", "Signature wrong", ["image refused, A keeps", "running; B not retried"], 13)
u.arrow(700, 288, 675, 328, "", "#2e9e5b"); u.arrow(860, 288, 885, 328, "", "#c0392b")
u.box(290, 330, 230, 86, "store", "B says hello to the bridge", ["marks itself valid:", "B stays"], 13)
u.box(20, 330, 230, 86, "gray", "B crashes or never connects", ["next reset boots A again;", "A will not install B again"], 13)
u.arrow(578, 372, 522, 372, "hello", "#2e9e5b", -8)
u.raw('<polyline points="675,418 675,444 135,444 135,418" fill="none" stroke="#c0392b" stroke-width="2" stroke-dasharray="6 5" marker-end="url(#a)"/>')
u.text(405, 462, "otherwise", 12, "#c0392b", "middle")
u.text(20, 205, "First time: the USB flash installs a signed", 12.5, "#444")
u.text(20, 223, "firmware, and with it the key the robot trusts.", 12.5, "#444")
u.text(20, 249, "The first update overwrites StackChan in ota_0;", 12.5, "#444")
u.text(20, 267, "then the two slots hold the new and the previous", 12.5, "#444")
u.text(20, 285, "botstack.", 12.5, "#444")
u.save("ota.svg")

# ---------------- Firmware tasks ----------------
t = Svg(1000, 640, "Firmware: tasks on the two cores")
t.box(20, 70, 470, 40, "gray", "Core 0", (), 15)
t.box(510, 70, 470, 40, "gray", "Core 1", (), 15)
core0 = [("LVGL", "the bloub face, 30 fps, small redraws", "robot"), ("playback (prio 9)", "speaker queue → I2S, silence when idle", "robot"),
         ("voice", "Opus decode 24 kHz", "robot"), ("mic_send", "Opus encode 16 kHz + TLS write (25.7 kB of stack used)", "robot"),
         ("link", "botstack/v1, state from the bridge; 20 s without a frame → restart", "bridge"), ("WebSocket + Wi-Fi", "esp_websocket_client (ping 5 s), lwIP, mbedTLS", "bridge"),
         ("head", "servos at 50 Hz, torque off at rest; missing at boot: retried every 10 s", "robot"), ("camera (prio 3)", "photo → JPEG → HTTPS upload, shutter click", "robot")]
core1 = [("capture (prio 8)", "I2S read mic + reference, 24 → 16 kHz", "robot"), ("afe_fetch", "ESP-SR AFE: echo canceller, voice detection, AGC", "robot"),
         ("touch (prio 5)", "head touch (Si12T) every 50 ms = a tap", "robot")]
for i, (name, desc, kind) in enumerate(core0):
    t.box(20, 125 + i * 58, 470, 48, kind, name, [desc], 13)
for i, (name, desc, kind) in enumerate(core1):
    t.box(510, 125 + i * 58, 470, 48, kind, name, [desc], 13)
t.box(510, 320, 470, 110, "note", "Why this split", ["The AFE fills most of core 1; encoding there starved it.", "Internal RAM is scarce: task stacks of our audio and", "link tasks live in PSRAM; Wi-Fi, lwIP and TLS", "buffers too. Display buffers are 40 lines."], 13)
t.text(20, 620, "Orange: audio, face, head, camera and touch. Blue: network. All Opus work uses esp_audio_codec; resampling is our own polyphase filter.", 12.5, "#444")
t.save("firmware-tasks.svg")

# ---------------- Face model ----------------
m = Svg(1000, 470, "The bloub face: eyes on a sphere")
cx, cy, r = 250, 260, 150
m.raw(f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="#f08a24"/>')
for k in range(-2, 3):
    m.raw(f'<ellipse cx="{cx}" cy="{cy}" rx="{abs(k)*0.3*r+0.02*r if k else 2}" ry="{r}" fill="none" stroke="#ffd2a8" stroke-width="1" stroke-dasharray="4 5"/>')
    m.raw(f'<ellipse cx="{cx}" cy="{cy}" rx="{r}" ry="{abs(k)*0.3*r+0.02*r if k else 2}" fill="none" stroke="#ffd2a8" stroke-width="1" stroke-dasharray="4 5"/>')
m.raw(f'<rect x="{cx+18}" y="{cy-95}" width="22" height="50" rx="11" fill="#f9f9f9" transform="rotate(-25 {cx+29} {cy-70})"/>')
m.raw(f'<rect x="{cx+75}" y="{cy-108}" width="16" height="46" rx="8" fill="#f9f9f9" transform="rotate(-25 {cx+83} {cy-85})"/>')
m.arrow(cx, cy, cx + 40, cy - 70, "", "#16181d")
m.text(cx - 6, cy + 22, "gaze", 13, "#fff", "end", "700")
m.text(cx - 6, cy + 38, "yaw · pitch · roll", 11, "#fff", "end")
lines = [("Gaze (yaw, pitch, roll)", "turns both eyes around the sphere; neutral looks up and right"),
         ("Split", "angle between the two eyes on the sphere (≈ 14–20°)"),
         ("Per eye: w, h", "size of a rounded rectangle, as a fraction of the radius"),
         ("Per eye: tilt", "rotation in the eye's own plane (anger, sadness…)"),
         ("Per eye: open", "eyelid; blinks squash it to 6 %"),
         ("Depth", "an eye turning away fades out"),
         ("At rest", "slow wander (3 harmonics) and blinks every 2–5 s")]
for i, (head, desc) in enumerate(lines):
    m.text(450, 110 + i * 46, head, 14, "#111", weight="700")
    m.text(450, 128 + i * 46, desc, 12.5, "#444")
m.save("face-model.svg")
print("ok")

# ---------------- Conversation ----------------
ROBOT, BRIDGE, XAI = 150, 480, 810
steps = [
    ("note", ROBOT, "Robot powered on"),
    (ROBOT, BRIDGE, "WebSocket open + hello (botstack/v1, mic Opus 16 kHz, 60 ms)", "#555"),
    (BRIDGE, ROBOT, "hello (voice Opus 24 kHz, 60 ms; vision URL) · state idle", "#555"),
    ("note", ROBOT, "User taps the screen or the head"),
    (ROBOT, BRIDGE, "talk start", "#555"),
    (BRIDGE, ROBOT, "animation wake · state listening", "#555"),
    (BRIDGE, XAI, "connect + session.update (voice, language, tools)", "#555"),
    (ROBOT, BRIDGE, "microphone, one Opus packet every 60 ms", "#f08a24"),
    (BRIDGE, XAI, "PCM 16 kHz", "#f08a24"),
    (XAI, BRIDGE, "speech_stopped (server VAD)", "#555"),
    (BRIDGE, ROBOT, "state thinking → dots orbit, microphone still on", "#555"),
    (XAI, BRIDGE, "response audio, PCM 24 kHz in bursts", "#2f6fdf"),
    (BRIDGE, ROBOT, "state speaking · speech start · 60 ms Opus packets, paced", "#2f6fdf"),
    (BRIDGE, ROBOT, "speech end (everything sent)", "#555"),
    (ROBOT, BRIDGE, "played (speaker drained)", "#555"),
    (BRIDGE, ROBOT, "state listening", "#555"),
    ("note", BRIDGE, "40 s without speech"),
    (BRIDGE, XAI, "close the session", "#c0392b"),
    (BRIDGE, ROBOT, "animation sleep · state idle (the WebSocket stays open)", "#c0392b"),
]
height = 130 + 42 * len(steps) + 20
q = Svg(960, height, "One conversation, step by step")
for x, name, kind in ((ROBOT, "StackChan robot", "robot"), (BRIDGE, "botstack-bridge", "bridge"), (XAI, "xAI realtime voice", "xai")):
    fl, st, tc, _ = COL[kind]
    q.raw(f'<rect x="{x-95}" y="60" width="190" height="40" rx="10" fill="{fl}" stroke="{st}" stroke-width="2"/>')
    q.text(x, 86, name, 15, tc, "middle", "700")
    q.raw(f'<line x1="{x}" y1="100" x2="{x}" y2="{height-20}" stroke="#c5cbd3" stroke-width="2" stroke-dasharray="5 5"/>')
y = 120
for step in steps:
    if step[0] == "note":
        _, x, label = step
        q.raw(f'<rect x="{x-110}" y="{y-14}" width="220" height="28" rx="8" fill="#fffbe6" stroke="#e0c96b"/>')
        q.text(x, y + 5, label, 13, "#6b5a10", "middle")
    else:
        a, b, label, color = step
        x1, x2 = (a + 8, b - 8) if b > a else (a - 8, b + 8)
        q.arrow(x1, y + 8, x2, y + 8, label, color)
    y += 42
q.save("conversation.svg")

# ---------------- Security ----------------
k = Svg(1000, 750, "Who may do what: keys, limits and the lock")
rows = [
    ("robot", "Each device", "/botstack/v1|and …/firmware", ["Device-Id = its MAC (else 403); Bearer its own key,",
     "or the bootstrap key ROBOT_TOKEN while it has none;", "else 401 · 403 revoked · 429 pending full · 503"]),
    ("robot", "Approved device", "/mcp/vision/explain", ["Bearer vision token: random per bridge process,",
     "in the hello of approved devices only;", "approved MAC with a camera, else 401"]),
    ("bot", "Grok Bot", "/grok/mcp", ["Bearer GROK_BRIDGE_TOKEN, compared in constant time",
     "replies and pushes only for approved devices;", "10 asks, 30 pushes, 20 photos per hour"]),
    ("bot", "Admin AI", "/fleet/mcp", ["Bearer FLEET_ADMIN_TOKEN (unset: 503), constant time;",
     "wrong: 401; 5 wrong from one address in a minute:", "429 to it for a minute; answers never hold a device key"]),
    ("gray", "Anyone else", "/healthz, /metrics", ["not routed by Caddy: inside the compose network only"]),
]
for i, (kind, who, door, checks) in enumerate(rows):
    y = 70 + i * 105
    k.box(20, y, 180, 80, kind, who, (), 14)
    k.box(250, y, 220, 80, "bridge", "", (), 14)
    parts = door.split("|")
    for j, part in enumerate(parts):
        k.text(360, y + 45 - (len(parts) - 1) * 9 + j * 18, part, 14 if j == 0 else 12.5, "#111" if j == 0 else "#444", "middle", "700" if j == 0 else "400")
    k.box(520, y, 460, 80, "note", "", (), 13)
    for j, line in enumerate(checks):
        k.text(540, y + 80 / 2 - (len(checks) - 1) * 9 + j * 18 + 4, line, 12.5, "#6b5a10")
    k.arrow(202, y + 40, 248, y + 40)
    k.arrow(472, y + 40, 518, y + 40, "", "#e0c96b")
k.box(20, 610, 450, 120, "store", "Secrets: .env on the server", ["XAI_API_KEY · GROK_BRIDGE_TOKEN · FLEET_ADMIN_TOKEN",
      "ROBOT_TOKEN · GROK_BOT_WEBHOOK_URL / _TOKEN", "never printed; filled by scripts/setup.sh;", "devices' own keys: SHA-256 only, in /data/fleet.json"], 14)
k.box(520, 610, 460, 120, "robot", "The lock", ["stackchan_lock (Grok Bot) or fleet_lock (admin)",
      "locked: wary face, taps answered with alert,", "the open conversation closes; Grok Bot replies,", "pushes and photos still work. Per device, in fleet.json."], 14)
k.save("security.svg")

# ---------------- Link lifecycle ----------------
L = Svg(1000, 560, "The link: always on, watched from both ends")
L.text(20, 72, "A. Healthy: the robot pings every 5 s, the bridge answers each ping", 15, "#2f6fdf", weight="700")
for x, name, kind in ((90, "robot", "robot"), (90, "bridge", "bridge")):
    pass
L.box(20, 90, 120, 34, "robot", "Robot", (), 13)
L.box(20, 170, 120, 34, "bridge", "Bridge", (), 13)
L.raw('<line x1="150" y1="107" x2="980" y2="107" stroke="#f08a24" stroke-width="2" stroke-dasharray="4 4"/>')
L.raw('<line x1="150" y1="187" x2="980" y2="187" stroke="#2f6fdf" stroke-width="2" stroke-dasharray="4 4"/>')
for i in range(6):
    x = 190 + i * 140
    L.arrow(x, 110, x + 14, 183, "ping" if i == 0 else "", "#f08a24", -2, "end", dx=-10)
    L.arrow(x + 30, 184, x + 44, 111, "pong" if i == 0 else "", "#2f6fdf", 18, "start", dx=12)
L.text(20, 232, "The robot restarts after 20 s without any frame from the bridge (pong timeout 10 s); the bridge drops a link silent for 75 s.", 12.5, "#444")
L.text(20, 286, "B. The bridge is unreachable (a deploy, the network): the robot retries, then restarts", 15, "#c0392b", weight="700")
x0, sx = 90, 40  # 0 s at x0, 40 px per second
L.raw(f'<line x1="{x0}" y1="380" x2="{x0 + 21 * sx}" y2="380" stroke="#9aa7b8" stroke-width="2" marker-end="url(#a)"/>')
for sec in range(0, 21, 5):
    L.raw(f'<line x1="{x0 + sec * sx}" y1="374" x2="{x0 + sec * sx}" y2="386" stroke="#9aa7b8" stroke-width="2"/>')
    L.text(x0 + sec * sx, 402, f"{sec} s", 12, "#6b7787", "middle")
L.box(x0 - 60, 310, 120, 44, "bridge", "last frame", (), 13)
t, delay = 0, 1
while t + delay < 20:
    t += delay
    L.raw(f'<circle cx="{x0 + t * sx}" cy="380" r="7" fill="#fff4e8" stroke="#f08a24" stroke-width="2"/>')
    L.text(x0 + t * sx, 362, f"retry +{delay} s", 11.5, "#b25f10", "middle")
    delay *= 2
L.box(x0 + 20 * sx - 70, 310, 140, 44, "robot", "esp_restart()", (), 13)
L.text(20, 440, "Back up: boot, Wi-Fi, connect with its own key (or the bootstrap key while it has none), hello. Unspoken messages wait in grok_pending.json.", 12.5, "#444")
L.text(20, 470, "C. What a tap (screen or head) does", 15, "#5b3cc4", weight="700")
taps = [("idle", "talk start"), ("listening · thinking", "talk stop"), ("speaking", "interrupt, silence"), ("offline · locked", "alert face")]
for i, (state, effect) in enumerate(taps):
    L.box(20 + i * 245, 485, 225, 56, "gray", state, [effect], 13)
L.save("link-lifecycle.svg")

# ---------------- Fleet lifecycle ----------------
F = Svg(1000, 420, "A device in the fleet")
steps = [(30, "robot", "unknown", ["bootstrap key"]), (220, "gray", "pending", ["confus face", "firmware, reboot only"]),
         (430, "bridge", "approved", ["fleet_approve", "talks, Grok Bot"]), (640, "store", "enrolled", ["own key in NVS", "bootstrap refused"]),
         (850, "note", "revoked", ["refused 10 min"])]
for x, kind, head, lines in steps:
    F.box(x, 90, 130 if x == 850 else 170, 80, kind, head, lines, 14)
F.arrow(200, 130, 218, 130)
F.arrow(390, 130, 428, 130)
F.arrow(600, 130, 638, 130, "enroll {key, id}|enrolled {id}", dy=-62)
F.arrow(810, 130, 848, 130)
F.text(30, 220, "Rotation, one at a time: a new enroll; both keys open until enrolled or a login with the new key. Offline rotation resets the key.", 13)
F.text(30, 245, "Own key refused (401): the device erases it and comes back with the bootstrap key.", 13)
F.text(30, 270, "The bridge keeps SHA-256 hashes only; no MCP answer and no log line holds a key.", 13)
F.box(30, 300, 460, 90, "bot", "/grok/mcp (Grok Bot)", ["stackchan_devices, say, photo, lock by name"], 14)
F.box(510, 300, 460, 90, "bridge", "/fleet/mcp (admin AI)", ["list, device, approve, rename, profile, lock,", "reboot, rotate key, revoke, firmware, say"], 14)
F.save("fleet-lifecycle.svg")
