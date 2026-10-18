// SPDX-License-Identifier: MIT
// Expression poses, animation timings and the wander/blink model are derived from bloub
// (https://github.com/jeremy-prt/bloub), Copyright (c) 2026 Jérémy Perret, MIT; see
// THIRD_PARTY_NOTICES.md.
#include "face/bloub.h"

#include <algorithm>
#include <cmath>
#include <cstring>

#include "esp_log.h"
#include "esp_random.h"
#include "esp_timer.h"

namespace bloub {
namespace {

constexpr float kPi = 3.14159265f;
constexpr float kTau = 6.2831853f;

// Layout for a 200-pixel disc (the StackChan's 320x240 panel). Face::init
// scales the pixel lengths to its disc; fractions of the radius and angles stay.
constexpr int kDiscSize = 200;
constexpr float kDotOrbit = 114.0f;
constexpr uint32_t kBadge = 0x2496E8;  // bloub's notification blue
constexpr float kBadgeRadius = 0.15f;  // of the disc radius
constexpr float kBadgePop = 1.14f;
constexpr float kNotch = 0.054f;
constexpr float kBadgeDistance = 1.003f;
constexpr float kBadgeAngle = -42.0f;  // degrees, screen y down: upper right
constexpr int kMicMargin = 8;  // from the screen's top right corner
constexpr int kRoundMargin = 4;  // round screen: from its edge
// The "!" the disc turns into for exclaim and alert, in pixels from the centre.
constexpr int kBarWidth = 26;
constexpr int kBarHeight = 108;
constexpr int kBarCentre = -24;
constexpr int kPointSize = 23;
constexpr int kPointCentre = 53;
constexpr int kDotSize = 12;
constexpr uint32_t kBackground = 0x0B0B0C;
constexpr uint32_t kDisc = 0xF08A24;
constexpr uint8_t kEyeShade = 0xF9;

constexpr uint32_t kFrameMs = 33;
constexpr float kBlendSeconds = 0.35f;
constexpr float kBlinkSeconds = 0.18f;

constexpr Eye eye(float w, float h, float tilt = 0, float open = 1) { return {w, h, tilt, open}; }

}  // namespace

// From bloub's expression table. Its vs(w, h, tilt, open) mirrors the tilt on
// the second eye, written out here.
const Expression kExpressions[kExpressionCount] = {
    {"neutre", {28.49f, 28.62f, -13.0f, 15.46f, {eye(0.186f, 0.412f), eye(0.186f, 0.412f)}}},
    {"attentif", {4, 5, -4, 16, {eye(0.21f, 0.44f), eye(0.21f, 0.44f)}}},
    {"surpris", {3, -3, 0, 19, {eye(0.45f, 0.47f), eye(0.45f, 0.47f)}}},
    {"excite", {6, -14, 0, 19.5f, {eye(0.4f, 0.56f, -10), eye(0.4f, 0.56f, 10)}}},
    {"heureux", {5, 9, 0, 17, {eye(0.27f, 0.17f, 14), eye(0.27f, 0.17f, -14)}}},
    {"hilare", {4, 14, 0, 18, {eye(0.34f, 0.13f, 20), eye(0.34f, 0.13f, -20)}}},
    {"colere", {3, 7, 0, 17, {eye(0.34f, 0.15f, 30), eye(0.34f, 0.15f, -30)}}},
    {"triste", {3, -13, 0, 16, {eye(0.22f, 0.4f, -28), eye(0.22f, 0.4f, 28)}}},
    {"effraye", {2, -20, 0, 20.5f, {eye(0.4f, 0.6f), eye(0.4f, 0.6f)}}},
    {"mefiant", {12, 6, -6, 16, {eye(0.21f, 0.4f), eye(0.22f, 0.15f)}}},
    {"confus", {-14, 3, 8, 16.5f, {eye(0.2f, 0.44f, -18), eye(0.28f, 0.17f, 14)}}},
    {"curieux", {16, -9, -15, 16.5f, {eye(0.24f, 0.46f, -8), eye(0.2f, 0.38f, -8)}}},
    {"fier", {5, 17, 0, 17, {eye(0.3f, 0.15f, 18), eye(0.3f, 0.15f, -18)}}},
    {"timide", {-19, -14, -7, 14, {eye(0.17f, 0.3f), eye(0.17f, 0.3f)}}},
    {"blase", {-22, 2, 0, 16, {eye(0.3f, 0.12f), eye(0.3f, 0.12f)}}},
    {"somnolent", {6, -9, -3, 16, {eye(0.2f, 0.42f, 0, 0.42f), eye(0.2f, 0.42f, 0, 0.42f)}}},
};

namespace {

struct AnimationSpec {
    const char* name;
    float duration;
    float morph;      // fade in and out, seconds
    bool has_pose;
    Pose pose;
    float eye_alpha;  // 0 hides the eyes
    bool badge;
    bool exclaim;
    bool sway;
    bool sleep;
};

// From bloub's animated states (duration, morph and poses are bloub's).
const AnimationSpec kAnimations[] = {
    {"none", 0, 0.1f, false, {}, 1, false, false, false, false},
    {"notify", 2.2f, 0.5f, true, {-21.94f, -5.82f, -12.2f, 18.89f, {eye(0.505f, 0.498f), eye(0.505f, 0.498f)}}, 1, true, false, false, false},
    {"wink", 1.6f, 0.3f, true, {-5.37f, 4.55f, 6.7f, 16.25f, {eye(0.236f, 0.464f), eye(0.447f, 0.089f)}}, 1, false, false, false, false},
    {"wide", 1.8f, 0.55f, true, {6.92f, -21.96f, 11.6f, 18.43f, {eye(0.356f, 0.875f), eye(0.356f, 0.875f)}}, 1, false, false, false, false},
    {"alert", 2.4f, 0.45f, false, {}, 0, false, true, true, false},
    {"exclaim", 2.0f, 0.45f, false, {}, 0, false, true, false, false},
    {"sleep", 2.4f, 0.5f, false, {}, 1, false, false, false, true},
    {"wake", 1.0f, 0.3f, true, {6.92f, -21.96f, 11.6f, 18.43f, {eye(0.356f, 0.875f), eye(0.356f, 0.875f)}}, 1, false, false, false, false},
};

}  // namespace

Animation find_animation(const char* name) {
    for (int i = 1; i < (int)(sizeof(kAnimations) / sizeof(kAnimations[0])); i++) {
        if (std::strcmp(kAnimations[i].name, name) == 0) {
            return static_cast<Animation>(i);
        }
    }
    return Animation::None;
}

int find_expression(const char* id) {
    for (int i = 0; i < kExpressionCount; i++) {
        if (std::strcmp(kExpressions[i].id, id) == 0) {
            return i;
        }
    }
    return -1;
}

namespace {

float clamp01(float x) { return x < 0 ? 0 : (x > 1 ? 1 : x); }
float lerp(float a, float b, float t) { return a + (b - a) * t; }
float ease_in_out_cubic(float t) { return t < 0.5f ? 4 * t * t * t : 1 - std::pow(-2 * t + 2, 3.0f) / 2; }

// bloub's wander: three harmonics of one period.
float wave(float t, float period, float phase) {
    float r = t / period * kTau;
    return 0.55f * std::sin(r + phase) + 0.3f * std::sin(2 * r + phase * 1.7f + 1.1f) +
           0.15f * std::sin(3 * r + phase * 2.3f + 2.4f);
}

Pose blend(const Pose& a, const Pose& b, float t) {
    Pose p{};
    p.yaw = lerp(a.yaw, b.yaw, t);
    p.pitch = lerp(a.pitch, b.pitch, t);
    p.roll = lerp(a.roll, b.roll, t);
    p.split = lerp(a.split, b.split, t);
    for (int i = 0; i < 2; i++) {
        p.eyes[i] = {lerp(a.eyes[i].w, b.eyes[i].w, t), lerp(a.eyes[i].h, b.eyes[i].h, t),
                     lerp(a.eyes[i].tilt, b.eyes[i].tilt, t), lerp(a.eyes[i].open, b.eyes[i].open, t)};
    }
    return p;
}

struct Vec3 {
    float x, y, z;
};

// bloub's ls(): rotate the pair (e, t) by n radians.
void rotate(Vec3& e, Vec3& t, float n) {
    float c = std::cos(n), s = std::sin(n);
    Vec3 e2{e.x * c + t.x * s, e.y * c + t.y * s, e.z * c + t.z * s};
    Vec3 t2{t.x * c - e.x * s, t.y * c - e.y * s, t.z * c - e.z * s};
    e = e2;
    t = t2;
}

struct Projected {
    float x, y;        // centre, in radii; bloub's coordinates are already y-down
    float a, b, c, d;  // local axes on screen: x' = a*x + c*y, y' = b*x + d*y
    float depth;
};

// bloub's us(): both eyes on the sphere for a gaze and a split.
void project(float yaw, float pitch, float roll, float split, Projected out[2]) {
    auto rad = [](float deg) { return deg * kPi / 180.0f; };
    Vec3 forward{0, 0, 1}, right{1, 0, 0}, up{0, 1, 0};
    rotate(forward, right, rad(yaw));
    rotate(up, forward, rad(pitch));
    rotate(right, up, rad(roll));
    for (int i = 0; i < 2; i++) {
        Vec3 o = forward, s = right;
        rotate(o, s, rad(split * (i == 0 ? -1.0f : 1.0f)));
        out[i] = {o.x, o.y, s.x, s.y, up.x, up.y, o.z};
    }
}

// Signed distance to a rounded rectangle with corner radius min(hw, hh).
float rounded_rect(float x, float y, float hw, float hh) {
    float r = std::min(hw, hh);
    float qx = std::fabs(x) - hw + r;
    float qy = std::fabs(y) - hh + r;
    float ox = std::max(qx, 0.0f), oy = std::max(qy, 0.0f);
    return std::sqrt(ox * ox + oy * oy) + std::min(std::max(qx, qy), 0.0f) - r;
}

bool empty(const lv_area_t& a) { return a.x2 < a.x1 || a.y2 < a.y1; }

}  // namespace

int Face::px(float value) const { return (int)std::lround(value * scale_); }

void Face::init(lv_obj_t* parent, int disc_size, bool round) {
    disc_size_ = disc_size;
    radius_ = disc_size_ / 2.0f;
    scale_ = disc_size_ / (float)kDiscSize;
    dot_orbit_ = kDotOrbit * scale_;
    dot_size_ = px(kDotSize);
    badge_distance_ = kBadgeDistance * radius_;
    // A round panel shows only the circle inscribed in the screen: the dots
    // and the badge at its largest stay inside it, less a margin.
    float screen_radius = 0.0f;  // 0: a rectangular screen
    if (round) {
        screen_radius = std::min(lv_obj_get_width(parent), lv_obj_get_height(parent)) / 2.0f - px(kRoundMargin);
        dot_orbit_ = std::min(dot_orbit_, screen_radius - dot_size_ / 2.0f);
        int badge_max = 2 * ((int)std::lround(kBadgeRadius * radius_ * kBadgePop) + (int)(kNotch * radius_));
        badge_distance_ = std::min(badge_distance_, screen_radius - badge_max / 2.0f);
    }
    bar_width_ = px(kBarWidth);
    bar_height_ = px(kBarHeight);
    bar_centre_ = px(kBarCentre);
    point_size_ = px(kPointSize);
    point_centre_ = px(kPointCentre);

    lv_obj_set_style_bg_color(parent, lv_color_hex(kBackground), 0);
    lv_obj_set_style_bg_opa(parent, LV_OPA_COVER, 0);
    lv_obj_remove_flag(parent, LV_OBJ_FLAG_SCROLLABLE);

    disc_ = lv_obj_create(parent);
    lv_obj_set_size(disc_, disc_size_, disc_size_);
    lv_obj_set_style_radius(disc_, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_bg_color(disc_, lv_color_hex(kDisc), 0);
    lv_obj_set_style_bg_opa(disc_, LV_OPA_COVER, 0);
    lv_obj_set_style_border_width(disc_, 0, 0);
    lv_obj_set_style_pad_all(disc_, 0, 0);
    lv_obj_remove_flag(disc_, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_remove_flag(disc_, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_center(disc_);

    // One canvas over the disc for both eyes. It never moves.
    buf_ = lv_draw_buf_create(disc_size_, disc_size_, LV_COLOR_FORMAT_ARGB8888, LV_STRIDE_AUTO);
    if (buf_ == nullptr) {
        // LVGL asserts (and spins) on a null buffer; say why instead.
        ESP_LOGE("bloub", "no memory for the eye canvas");
        return;
    }
    lv_draw_buf_clear(buf_, nullptr);
    canvas_ = lv_canvas_create(disc_);
    lv_canvas_set_draw_buf(canvas_, buf_);
    lv_obj_set_pos(canvas_, 0, 0);
    lv_obj_remove_flag(canvas_, LV_OBJ_FLAG_CLICKABLE);
    dirty_[0] = dirty_[1] = {0, 0, -1, -1};

    // Thinking: three small dots orbit the disc. Small objects keep the
    // redraw small (a rotating arc invalidated the whole face every frame).
    for (auto& dot : dots_) {
        dot = lv_obj_create(parent);
        lv_obj_set_size(dot, dot_size_, dot_size_);
        lv_obj_set_style_radius(dot, LV_RADIUS_CIRCLE, 0);
        lv_obj_set_style_bg_color(dot, lv_color_hex(kDisc), 0);
        lv_obj_set_style_border_width(dot, 0, 0);
        lv_obj_remove_flag(dot, LV_OBJ_FLAG_CLICKABLE);
        lv_obj_remove_flag(dot, LV_OBJ_FLAG_SCROLLABLE);
        lv_obj_add_flag(dot, LV_OBJ_FLAG_HIDDEN);
    }

    // notify: a blue badge on the edge, ringed with the background colour so it
    // looks notched into the disc, as in bloub.
    badge_ = lv_obj_create(parent);
    lv_obj_set_style_radius(badge_, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_bg_color(badge_, lv_color_hex(kBadge), 0);
    lv_obj_set_style_border_color(badge_, lv_color_hex(kBackground), 0);
    lv_obj_set_style_border_width(badge_, (int)(kNotch * radius_), 0);
    lv_obj_remove_flag(badge_, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_remove_flag(badge_, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(badge_, LV_OBJ_FLAG_HIDDEN);
    // exclaim / alert: the dot under the bar.
    point_ = lv_obj_create(parent);
    lv_obj_set_style_radius(point_, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_bg_color(point_, lv_color_hex(kDisc), 0);
    lv_obj_set_style_border_width(point_, 0, 0);
    lv_obj_remove_flag(point_, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_remove_flag(point_, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(point_, LV_OBJ_FLAG_HIDDEN);
    disc_w_ = disc_h_ = disc_size_;
    build_mic(parent, screen_radius);

    current_ = kExpressions[0].pose;
    from_ = current_;
    shown_ = 0;
    lv_timer_create(on_timer, kFrameMs, this);
}

void Face::set_expression(int index) {
    if (index >= 0 && index < kExpressionCount) {
        target_.store(index);
    }
}

void Face::set_state(State state) { state_.store(state); }

void Face::set_speech_level(float level) { speech_.store(clamp01(level)); }

void Face::play(Animation animation) {
    uint32_t sequence = (anim_request_.load() >> 8) + 1;
    anim_request_.store(sequence << 8 | static_cast<uint8_t>(animation));
}

void Face::on_timer(lv_timer_t* timer) {
    auto* face = static_cast<Face*>(lv_timer_get_user_data(timer));
    static float last = 0;
    float now = esp_timer_get_time() / 1e6f;
    float dt = last == 0 ? kFrameMs / 1000.0f : now - last;
    last = now;
    face->step(now, dt);
}

float Face::blink_lid(float now) {
    if (now >= next_blink_) {
        blink_start_ = now;
        // 1.9-4.6 s apart, with a quick double blink one time in six, as bloub.
        double_blink_ = (esp_random() % 100) < 18;
        next_blink_ = now + 1.9f + (esp_random() % 1000) / 1000.0f * 2.7f;
    }
    auto profile = [](float r) { return r < 0.45f ? 1 - r / 0.45f : (r - 0.45f) / 0.55f; };
    float r = (now - blink_start_) / kBlinkSeconds;
    if (r >= 0 && r <= 1) {
        return profile(r);
    }
    if (double_blink_) {
        float r2 = (now - blink_start_ - kBlinkSeconds - 0.06f) / kBlinkSeconds;
        if (r2 >= 0 && r2 <= 1) {
            return profile(r2);
        }
    }
    return 1;
}

void Face::step(float now, float dt) {
    // Expression blend.
    int target = target_.load();
    if (target != shown_) {
        from_ = current_;
        shown_ = target;
        blend_start_ = now;
    }
    float t = clamp01((now - blend_start_) / kBlendSeconds);
    current_ = blend(from_, kExpressions[shown_].pose, ease_in_out_cubic(t));

    // State shaping, eased so a state change never jumps.
    // One-shot animation over the expression, faded in and out.
    uint32_t request = anim_request_.load();
    if (request != anim_seen_) {
        anim_seen_ = request;
        anim_ = static_cast<Animation>(request & 0xFF);
        anim_start_ = now;
    }
    const AnimationSpec& anim = kAnimations[static_cast<int>(anim_)];
    float elapsed = now - anim_start_;
    float w = 0;
    if (anim_ != Animation::None) {
        if (elapsed >= anim.duration) {
            anim_ = Animation::None;
        } else {
            w = ease_in_out_cubic(clamp01(std::min(elapsed, anim.duration - elapsed) / anim.morph));
        }
    }
    Pose pose = anim.has_pose && w > 0 ? blend(current_, anim.pose, w) : current_;
    // "!": the eyes fade during the first third, then the disc reshapes.
    float eyes_shown = anim.eye_alpha < 1 ? clamp01(1 - w / 0.3f) : 1.0f;
    float bar = anim.exclaim ? clamp01((w - 0.3f) / 0.7f) : 0.0f;
    float sway = anim.sway ? std::sin(elapsed * kTau / 0.9f) * 10.0f * scale_ * bar : 0.0f;
    float pop = elapsed < 0.45f ? (elapsed < 0.3f ? kBadgePop * elapsed / 0.3f : lerp(kBadgePop, 1, (elapsed - 0.3f) / 0.15f)) : 1.0f;
    float badge = anim.badge ? std::min(pop, w) : 0.0f;
    float sleepy = anim.sleep ? w : 0.0f;

    State state = state_.load();
    float want_wander = state == State::Idle ? 1.0f : state == State::Sleeping ? 0.2f : 0.5f;
    float want_think = state == State::Thinking ? 1.0f : 0.0f;
    float k = clamp01(dt * 4);
    wander_ = lerp(wander_, want_wander, k);
    think_ = lerp(think_, want_think, k);

    float yaw = pose.yaw + (wave(now, 11.3f, 0.4f) * 5.5f + wave(now, 3.7f, 2.1f) * 1.6f) * wander_ - 8 * think_;
    float pitch = pose.pitch + (wave(now, 9.1f, 1.3f) * 4.2f + wave(now, 4.3f, 0.7f) * 1.3f) * wander_ + 14 * think_ -
                  10 * sleepy;
    float roll = pose.roll + wave(now, 13.7f, 3.2f) * 2.2f * wander_;
    float drift_x = wave(now, 7.9f, 1.9f) * 0.006f * radius_;
    float drift_y = wave(now, 5.3f, 0.3f) * 0.007f * radius_;
    float breath = 1 + std::sin(now / 3.4f * kTau) * (state == State::Sleeping ? 0.02f : 0.005f);
    float bob = state == State::Speaking ? -speech_.load() * 5.0f * scale_ : 0.0f;
    bob += std::sin(elapsed * kTau / 1.2f) * 4.0f * scale_ * sleepy;  // sleep: the face floats

    float lid = state == State::Sleeping ? 0.0f : blink_lid(now);
    lid = lerp(lid, 0, sleepy);

    if (buf_ == nullptr) {
        return;
    }
    Projected eyes[2];
    project(yaw, pitch, roll, pose.split, eyes);
    EyeDraw draws[2];
    for (int i = 0; i < 2; i++) {
        const Projected& p = eyes[i];
        const Eye& e = pose.eyes[i];
        float alpha = (p.depth <= 0.02f ? 0.0f : clamp01(p.depth / 0.12f)) * eyes_shown;
        float tilt = e.tilt * kPi / 180.0f;
        float tc = std::cos(tilt), ts = std::sin(tilt);
        float open = 0.06f + 0.94f * clamp01(std::min(lid, e.open));
        draws[i] = {radius_ + p.x * radius_ * breath + drift_x,
                    radius_ + p.y * radius_ * breath + drift_y + bob,
                    p.a * tc + p.c * ts,
                    (p.b * tc + p.d * ts) * open,
                    -p.a * ts + p.c * tc,
                    (-p.b * ts + p.d * tc) * open,
                    e.w * radius_ / 2,
                    e.h * radius_ / 2,
                    alpha};
    }

    // Clear both old boxes first, then draw both eyes (they may overlap).
    lv_area_t old[2] = {dirty_[0], dirty_[1]};
    for (const auto& box : old) {
        clear_box(box);
    }
    for (int i = 0; i < 2; i++) {
        dirty_[i] = draw_eye(draws[i]);
    }
    for (int i = 0; i < 2; i++) {
        lv_area_t both = old[i];
        if (empty(both)) {
            both = dirty_[i];
        } else if (!empty(dirty_[i])) {
            both.x1 = std::min(both.x1, dirty_[i].x1);
            both.y1 = std::min(both.y1, dirty_[i].y1);
            both.x2 = std::max(both.x2, dirty_[i].x2);
            both.y2 = std::max(both.y2, dirty_[i].y2);
        }
        invalidate_box(both);
    }

    lv_obj_t* screen = lv_obj_get_parent(disc_);
    int cx = lv_obj_get_width(screen) / 2, cy = lv_obj_get_height(screen) / 2;
    shape(bar, sway, cx, cy);
    place_badge(badge, cx, cy);
    bool mic = microphone_.load();
    if (mic != mic_shown_) {
        mic_shown_ = mic;
        if (mic) {
            lv_obj_remove_flag(mic_, LV_OBJ_FLAG_HIDDEN);
        } else {
            lv_obj_add_flag(mic_, LV_OBJ_FLAG_HIDDEN);
        }
    }
    for (int k = 0; k < 3; k++) {
        if (think_ > 0.05f) {
            float angle = now * 3.2f + k * kTau / 3;
            lv_obj_remove_flag(dots_[k], LV_OBJ_FLAG_HIDDEN);
            lv_obj_set_style_bg_opa(dots_[k], (lv_opa_t)(think_ * (255 - k * 60)), 0);
            lv_obj_set_pos(dots_[k], cx + (int)(dot_orbit_ * std::cos(angle)) - dot_size_ / 2,
                           cy + (int)(dot_orbit_ * std::sin(angle)) - dot_size_ / 2);
        } else if (!lv_obj_has_flag(dots_[k], LV_OBJ_FLAG_HIDDEN)) {
            lv_obj_add_flag(dots_[k], LV_OBJ_FLAG_HIDDEN);
        }
    }
}

// Morph the disc towards the "!" bar (0 = disc, 1 = bar). The eye canvas is
// hidden meanwhile: it is a child of the disc and would move with it, and
// redrawing the whole ARGB canvas each frame is what starves the LVGL core.
void Face::shape(float bar, float sway, int cx, int cy) {
    float e = ease_in_out_cubic(bar);
    int w = (int)std::lround(lerp(disc_size_, bar_width_, e));
    int h = (int)std::lround(lerp(disc_size_, bar_height_, e));
    int x = (int)std::lround(sway);
    int y = (int)std::lround(bar_centre_ * e);
    if (w != disc_w_ || h != disc_h_ || x != disc_x_ || y != disc_y_) {
        disc_w_ = w, disc_h_ = h, disc_x_ = x, disc_y_ = y;
        lv_obj_set_size(disc_, w, h);
        lv_obj_align(disc_, LV_ALIGN_CENTER, x, y);
    }
    bool reshaped = w != disc_size_ || h != disc_size_ || x != 0 || y != 0;
    if (canvas_ != nullptr && reshaped != lv_obj_has_flag(canvas_, LV_OBJ_FLAG_HIDDEN)) {
        if (reshaped) {
            lv_obj_add_flag(canvas_, LV_OBJ_FLAG_HIDDEN);
        } else {
            lv_obj_remove_flag(canvas_, LV_OBJ_FLAG_HIDDEN);
        }
    }

    int d = (int)std::lround(point_size_ * clamp01((bar - 0.5f) / 0.5f));
    if (d != point_d_) {
        point_d_ = d;
        if (d < 2) {
            lv_obj_add_flag(point_, LV_OBJ_FLAG_HIDDEN);
        } else {
            lv_obj_set_size(point_, d, d);
            lv_obj_remove_flag(point_, LV_OBJ_FLAG_HIDDEN);
        }
    }
    if (d >= 2) {
        lv_obj_set_pos(point_, cx + x - d / 2, cy + point_centre_ - d / 2);
    }
}

// A small microphone in bloub's blue: a capsule in a U-shaped cradle on a stand.
// Plain LVGL objects, drawn only when it appears or disappears. In the top right
// corner; on a round screen (`screen_radius` > 0), at -45 degrees from the
// centre, as far out as its box stays inside that radius.
void Face::build_mic(lv_obj_t* parent, float screen_radius) {
    auto part = [](lv_obj_t* owner, int x, int y, int w, int h) {
        lv_obj_t* o = lv_obj_create(owner);
        lv_obj_set_pos(o, x, y);
        lv_obj_set_size(o, w, h);
        lv_obj_set_style_pad_all(o, 0, 0);
        lv_obj_set_style_border_width(o, 0, 0);
        lv_obj_set_style_bg_opa(o, LV_OPA_TRANSP, 0);
        lv_obj_remove_flag(o, LV_OBJ_FLAG_CLICKABLE);
        lv_obj_remove_flag(o, LV_OBJ_FLAG_SCROLLABLE);
        return o;
    };
    auto filled = [](lv_obj_t* o, int radius) {
        lv_obj_set_style_radius(o, radius, 0);
        lv_obj_set_style_bg_color(o, lv_color_hex(kBadge), 0);
        lv_obj_set_style_bg_opa(o, LV_OPA_COVER, 0);
    };
    // Laid out for a 200-pixel disc, scaled with it.
    constexpr int kWidth = 22, kHeight = 30;
    mic_ = part(parent, 0, 0, px(kWidth), px(kHeight));
    if (screen_radius > 0) {
        // Centre at (a, -a): the far corner (a + w/2, a + h/2) lies on the
        // circle when 2a^2 + 2a(w/2 + h/2) + (w/2)^2 + (h/2)^2 = r^2.
        float hw = px(kWidth) / 2.0f, hh = px(kHeight) / 2.0f;
        float sum = hw + hh;
        float a = (-sum + std::sqrt(sum * sum - 2 * (hw * hw + hh * hh - screen_radius * screen_radius))) / 2;
        int offset = (int)std::floor(a);
        lv_obj_align(mic_, LV_ALIGN_CENTER, offset, -offset);
    } else {
        lv_obj_align(mic_, LV_ALIGN_TOP_RIGHT, -px(kMicMargin), px(kMicMargin));
    }
    filled(part(mic_, px(6), 0, px(10), px(17)), LV_RADIUS_CIRCLE);  // capsule
    lv_obj_t* cradle = part(mic_, px(2), px(7), px(18), px(15));
    lv_obj_set_style_radius(cradle, px(9), 0);
    lv_obj_set_style_border_width(cradle, px(2), 0);
    lv_obj_set_style_border_color(cradle, lv_color_hex(kBadge), 0);
    lv_obj_set_style_border_side(cradle, (lv_border_side_t)(LV_BORDER_SIDE_LEFT | LV_BORDER_SIDE_RIGHT | LV_BORDER_SIDE_BOTTOM), 0);
    filled(part(mic_, px(10), px(22), px(2), px(5)), 0);  // stand
    filled(part(mic_, px(6), px(27), px(10), px(2)), px(1));  // base
    lv_obj_add_flag(mic_, LV_OBJ_FLAG_HIDDEN);
}

// notify: the badge pops on the upper-right edge (scale 0 hides it).
void Face::place_badge(float scale, int cx, int cy) {
    int ring = (int)(kNotch * radius_);
    int d = scale <= 0.05f ? 0 : 2 * ((int)std::lround(kBadgeRadius * radius_ * scale) + ring);
    if (d == badge_d_) {
        return;
    }
    badge_d_ = d;
    if (d == 0) {
        lv_obj_add_flag(badge_, LV_OBJ_FLAG_HIDDEN);
        return;
    }
    float angle = kBadgeAngle * kPi / 180.0f;
    lv_obj_set_size(badge_, d, d);
    lv_obj_set_pos(badge_, cx + (int)std::lround(badge_distance_ * std::cos(angle)) - d / 2,
                   cy + (int)std::lround(badge_distance_ * std::sin(angle)) - d / 2);
    lv_obj_remove_flag(badge_, LV_OBJ_FLAG_HIDDEN);
}

void Face::clear_box(const lv_area_t& box) {
    if (empty(box)) {
        return;
    }
    for (int y = box.y1; y <= box.y2; y++) {
        std::memset(lv_draw_buf_goto_xy(buf_, box.x1, y), 0, (box.x2 - box.x1 + 1) * 4);
    }
}

void Face::invalidate_box(const lv_area_t& box) {
    if (empty(box)) {
        return;
    }
    lv_area_t abs;
    lv_obj_get_coords(canvas_, &abs);
    lv_area_t area = {abs.x1 + box.x1, abs.y1 + box.y1, abs.x1 + box.x2, abs.y1 + box.y2};
    lv_obj_invalidate_area(canvas_, &area);
}

lv_area_t Face::draw_eye(const EyeDraw& e) {
    lv_area_t box = {0, 0, -1, -1};
    float det = e.a * e.d - e.b * e.c;
    if (e.alpha <= 0.004f || std::fabs(det) < 1e-6f) {
        return box;
    }
    // Screen bounding box of the projected rectangle, plus a pixel of margin.
    float ex = std::fabs(e.a) * e.hw + std::fabs(e.c) * e.hh;
    float ey = std::fabs(e.b) * e.hw + std::fabs(e.d) * e.hh;
    box.x1 = std::max(0, (int)std::floor(e.cx - ex) - 1);
    box.x2 = std::min(disc_size_ - 1, (int)std::ceil(e.cx + ex) + 1);
    box.y1 = std::max(0, (int)std::floor(e.cy - ey) - 1);
    box.y2 = std::min(disc_size_ - 1, (int)std::ceil(e.cy + ey) + 1);
    if (empty(box)) {
        return box;
    }

    // Inverse map: screen offset -> eye coordinates.
    float ia = e.d / det, ic = -e.c / det, ib = -e.b / det, id = e.a / det;
    auto sdf = [&](float sx, float sy) { return rounded_rect(ia * sx + ic * sy, ib * sx + id * sy, e.hw, e.hh); };
    for (int y = box.y1; y <= box.y2; y++) {
        auto* row = reinterpret_cast<lv_color32_t*>(lv_draw_buf_goto_xy(buf_, 0, y));
        float sy = y + 0.5f - e.cy;
        for (int x = box.x1; x <= box.x2; x++) {
            float sx = x + 0.5f - e.cx;
            float dist = sdf(sx, sy);
            // Distance in screen pixels: divide by the local gradient.
            float gx = sdf(sx + 1, sy) - dist;
            float gy = sdf(sx, sy + 1) - dist;
            float g = std::sqrt(gx * gx + gy * gy);
            float cover = clamp01(0.5f - dist / (g > 1e-4f ? g : 1e-4f));
            uint8_t opa = (uint8_t)(cover * e.alpha * 255);
            // The other eye may already be here: keep the stronger one.
            if (opa > row[x].alpha) {
                row[x] = {kEyeShade, kEyeShade, kEyeShade, opa};
            }
        }
    }
    return box;
}

}  // namespace bloub
