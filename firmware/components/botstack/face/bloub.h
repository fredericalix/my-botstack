// SPDX-License-Identifier: MIT
// Expression poses, animation timings and the wander/blink model are derived from bloub
// (https://github.com/jeremy-prt/bloub), Copyright (c) 2026 Jérémy Perret, MIT; see
// THIRD_PARTY_NOTICES.md.
//
// The bloub face: an orange disc with two white eyes placed on a sphere.
//
// The model comes from bloub (bloub.vercel.app), which recreates the x.ai bot
// avatar: each expression is a gaze (yaw, pitch, roll on the sphere), an eye
// split angle, and per eye a width, height, tilt and openness. Eyes are
// rounded rectangles projected onto the sphere's local axes; an eye fades as
// it turns away. Expressions blend with an ease-in-out; at rest the gaze
// wanders slowly and the eyes blink.
//
// LVGL has no skewed rounded shape and this board avoids the image transform
// path, so the eyes are rasterised into one fixed ARGB canvas over the disc,
// with analytic anti-aliasing from a signed distance. Only the boxes the eyes
// leave and enter are redrawn: blending ARGB onto the RGB565 panel is the
// costly part, and with audio running the LVGL core has no time to spare.
#pragma once

#include <atomic>
#include <cstdint>

#include "lvgl.h"

namespace bloub {

struct Eye {
    float w;     // width, fraction of the disc radius
    float h;     // height, fraction of the disc radius
    float tilt;  // degrees, in the eye's own plane
    float open;  // 1 open, 0 closed
};

struct Pose {
    float yaw;    // degrees, gaze on the sphere
    float pitch;  // degrees, up is positive
    float roll;   // degrees
    float split;  // degrees between the eyes on the sphere
    Eye eyes[2];
};

struct Expression {
    const char* id;
    Pose pose;
};

constexpr int kExpressionCount = 16;
extern const Expression kExpressions[kExpressionCount];

/// Index of an expression id ("neutre", "heureux", ...), or -1.
int find_expression(const char* id);

enum class State : uint8_t {
    Idle,       // wandering gaze, blinks
    Listening,  // calmer gaze, attentive
    Thinking,   // looks up, an arc turns around the disc
    Speaking,   // eyes bob with the voice level
    Sleeping,   // eyes closed, slow breath
};

/// One-shot animated states from bloub, played on top of the current face.
enum class Animation : uint8_t {
    None,
    Notify,   // looks down-left, a blue badge pops on the upper-right edge
    Wink,     // one eye flattens
    Wide,     // big eyes, looking up
    Alert,    // the disc becomes a swaying "!"
    Exclaim,  // the disc becomes a "!"
    Sleep,    // eyes close, the disc floats
    Wake,     // a short wide-eyed look
};

/// Animation for a protocol name ("notify", "wink", ...), or None.
Animation find_animation(const char* name);

class Face {
public:
    /// Build the face under `parent`, its disc `disc_size` pixels across; every
    /// other size follows (the layout is drawn for 200). On a `round` screen the
    /// thinking dots, the notify badge and the microphone icon stay inside the
    /// circle the panel shows. Call with the LVGL lock held.
    void init(lv_obj_t* parent, int disc_size = 200, bool round = false);

    // Thread-safe: they only store a target, read by the LVGL timer.
    void set_expression(int index);
    void set_state(State state);
    void set_speech_level(float level);  // 0..1
    void play(Animation animation);
    /// Show the microphone icon (top right).
    void set_microphone(bool on) { microphone_.store(on); }

    int expression() const { return target_.load(); }
    State state() const { return state_.load(); }

private:
    static void on_timer(lv_timer_t* timer);
    void step(float now, float dt);
    struct EyeDraw {
        float cx, cy;      // centre, canvas pixels
        float a, b, c, d;  // eye axes on screen
        float hw, hh;      // half size, pixels
        float alpha;
    };
    lv_area_t draw_eye(const EyeDraw& eye);
    void clear_box(const lv_area_t& box);
    void invalidate_box(const lv_area_t& box);
    float blink_lid(float now);
    void shape(float bar, float sway, int cx, int cy);
    void place_badge(float scale, int cx, int cy);
    void build_mic(lv_obj_t* parent, float screen_radius);
    /// A length of the 200-pixel layout, in this face's pixels.
    int px(float value) const;

    lv_obj_t* disc_ = nullptr;
    lv_obj_t* canvas_ = nullptr;
    lv_draw_buf_t* buf_ = nullptr;
    lv_area_t dirty_[2] = {};
    lv_obj_t* dots_[3] = {nullptr, nullptr, nullptr};
    lv_obj_t* badge_ = nullptr;  // notify
    lv_obj_t* point_ = nullptr;  // the dot of "!"
    lv_obj_t* mic_ = nullptr;    // top right while the microphone streams
    bool mic_shown_ = false;
    int disc_w_ = 0, disc_h_ = 0, disc_x_ = 0, disc_y_ = 0;
    int badge_d_ = -1, point_d_ = -1;
    // Sizes in pixels, set by init from the disc diameter.
    int disc_size_ = 200;
    float radius_ = 100.0f;
    float scale_ = 1.0f;  // disc_size_ / 200
    float dot_orbit_ = 114.0f;
    float badge_distance_ = 100.3f;  // badge centre from the disc centre
    int dot_size_ = 12;
    int bar_width_ = 26, bar_height_ = 108, bar_centre_ = -24;
    int point_size_ = 23, point_centre_ = 53;

    std::atomic<int> target_{0};
    std::atomic<State> state_{State::Idle};
    std::atomic<float> speech_{0.0f};
    std::atomic<bool> microphone_{false};
    std::atomic<uint32_t> anim_request_{0};  // sequence << 8 | animation
    uint32_t anim_seen_ = 0;
    Animation anim_ = Animation::None;
    float anim_start_ = 0.0f;

    int shown_ = -1;
    Pose from_{};
    Pose current_{};
    float blend_start_ = 0.0f;

    float next_blink_ = 2.0f;
    float blink_start_ = -10.0f;
    bool double_blink_ = false;
    float wander_ = 1.0f;
    float think_ = 0.0f;
};

}  // namespace bloub
