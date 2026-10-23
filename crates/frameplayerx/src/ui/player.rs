//! Playback UI: the control bar, the adjustments panel and subtitles.

use super::theme::Weight;
use super::widgets::{Kind, Tip};
use super::{Action, View, depth, fmt_time, icons, premium, theme, widgets};
use crate::subtitles::Pick;
use egui::{Align, Color32, Layout, RichText, Sense, Vec2};
use fp_core::format::{Projection, StereoLayout, VideoFormat};
use fp_core::view::ViewSettings;
use fp_media::PlayerState;

/// Format presets offered for overrides.
pub fn format_choices() -> Vec<(&'static str, VideoFormat)> {
    use StereoLayout::*;
    let f = |p, s| VideoFormat::new(p, s);
    vec![
        ("Flat 2D", f(Projection::Flat, Mono)),
        ("Flat 3D Half SBS", f(Projection::Flat, HalfSideBySide)),
        ("Flat 3D SBS", f(Projection::Flat, SideBySide)),
        ("Flat 3D Half TB", f(Projection::Flat, HalfTopBottom)),
        ("Flat 3D TB", f(Projection::Flat, TopBottom)),
        ("180° SBS", f(Projection::EQUIRECT_180, SideBySide)),
        ("180° TB", f(Projection::EQUIRECT_180, TopBottom)),
        ("180° 2D", f(Projection::EQUIRECT_180, Mono)),
        ("360° 2D", f(Projection::EQUIRECT_360, Mono)),
        ("360° TB", f(Projection::EQUIRECT_360, TopBottom)),
        ("360° SBS", f(Projection::EQUIRECT_360, SideBySide)),
        (
            "Fisheye 190° SBS",
            f(Projection::fisheye(190.0), SideBySide),
        ),
        (
            "Fisheye 200° SBS",
            f(Projection::fisheye(200.0), SideBySide),
        ),
        (
            "Fisheye 220° SBS",
            f(Projection::fisheye(220.0), SideBySide),
        ),
        (
            "EAC 360° (YouTube)",
            f(Projection::Eac { h_fov: 360.0 }, Mono),
        ),
        (
            "EAC 360° TB",
            f(Projection::Eac { h_fov: 360.0 }, TopBottom),
        ),
        (
            "EAC 180° SBS",
            f(Projection::Eac { h_fov: 180.0 }, SideBySide),
        ),
    ]
}

/// The floating transport bar: title and format, the seek bar, and three
/// groups of controls (browse and sound left, transport centred, view
/// right).
pub fn control_bar(ctx: &egui::Context, v: &mut View) {
    // The bar floats in its panel: the outer margin leaves room for its
    // shadow on the video.
    widgets::panel_slab(ctx, 28.0);
    let frame = egui::Frame::new()
        .outer_margin(widgets::SHADOW_ROOM)
        .inner_margin(egui::Margin::symmetric(28, 18));
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let Some(pb) = v.playback.as_deref_mut() else {
            return;
        };
        title_row(ui, pb, v.state, v.settings.hardware_decoding);
        ui.add_space(6.0);
        seek_row(ui, pb, v.state, v.actions);
        ui.add_space(8.0);

        // Controls: three groups on one row, transport always centred.
        let (row, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 72.0), Sense::hover());
        let mut left = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(Layout::left_to_right(Align::Center)),
        );
        widgets::well(&mut left, |ui| browse_and_sound(ui, pb, v.state, v.actions));
        transport(ui, row, pb, v.settings.seek_step, v.actions);
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(Layout::right_to_left(Align::Center)),
        );
        widgets::well(&mut right, |ui| {
            view_keys(
                ui,
                pb,
                v.state,
                v.settings.passthrough,
                v.passthrough_available,
                v.settings.follow,
                v.actions,
            );
        });
    });
}

/// The format and stream pills open the adjust panel's Format tab (again
/// closes it).
fn toggle_format_tab(state: &mut super::UiState) {
    let showing = state.adjust_open && state.adjust_tab == 0;
    state.adjust_open = !showing;
    state.adjust_tab = 0;
}

/// Title, status and format.
fn title_row(
    ui: &mut egui::Ui,
    pb: &crate::playback::Playback,
    state: &mut super::UiState,
    hardware_decoding: bool,
) {
    ui.horizontal(|ui| {
        ui.set_height(30.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if format_pill(ui, &widgets::format_short(&pb.format))
                .tip("Change the format")
                .clicked()
            {
                toggle_format_tab(state);
            }
            if let Some(video) = pb.player.info().video_stream() {
                let st = pb.player.stats();
                let (badge_text, tooltip) = stream_info_labels(video, &st, hardware_decoding);
                if stream_pill(ui, &badge_text, st.hardware)
                    .tip(&tooltip)
                    .clicked()
                {
                    toggle_format_tab(state);
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let message = match pb.player.error() {
                    Some(e) => Some((e, theme::ERROR)),
                    None => pb.notice.clone().map(|n| (n, theme::WARN)),
                };
                // With a message, the title leaves it most of the row;
                // both truncate rather than run under the format pill.
                let title_width = if message.is_some() {
                    ui.available_width() * 0.35
                } else {
                    ui.available_width()
                };
                ui.scope(|ui| {
                    ui.set_max_width(title_width);
                    ui.add(
                        egui::Label::new(
                            RichText::new(widgets::display_title(&pb.title))
                                .font(theme::font(Weight::SemiBold, 21.0))
                                .color(theme::TEXT),
                        )
                        .truncate(),
                    );
                });
                // Sized to fit the 30-point row: the default spinner
                // (44) grew the row, so every stutter bounced the seek
                // bar and buttons up and down.
                if pb.player.state() == PlayerState::Buffering {
                    ui.add(egui::Spinner::new().size(20.0));
                }
                if let Some((text, color)) = message {
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .font(theme::font(Weight::Regular, 15.0))
                                .color(color),
                        )
                        .truncate(),
                    );
                }
            });
        });
    });
}

/// Times either side of the seek bar.
fn seek_row(
    ui: &mut egui::Ui,
    pb: &mut crate::playback::Playback,
    state: &mut super::UiState,
    actions: &mut Vec<Action>,
) {
    let duration = pb.player.duration();
    let position = state.scrub.unwrap_or_else(|| pb.player.position());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let time = |ui: &mut egui::Ui, t: f64, align: Align| {
            ui.allocate_ui_with_layout(
                Vec2::new(68.0, 36.0),
                Layout::left_to_right(Align::Center).with_main_align(align),
                |ui| {
                    ui.label(
                        RichText::new(fmt_time(t))
                            .font(theme::font(Weight::Medium, 16.0))
                            .color(theme::TEXT_2),
                    )
                },
            );
        };
        time(ui, position, Align::Min);
        let w = ui.available_width() - 68.0 - 14.0;
        seek_bar(ui, w, state, pb, actions);
        time(ui, duration, Align::Max);
    });
}

/// Browse, speed and sound.
fn browse_and_sound(
    ui: &mut egui::Ui,
    pb: &crate::playback::Playback,
    state: &mut super::UiState,
    actions: &mut Vec<Action>,
) {
    if widgets::key(ui, icons::SQUARES_FOUR, 52.0, false)
        .tip("Browse")
        .clicked()
    {
        actions.push(Action::ShowBrowser(true));
    }
    if widgets::key(ui, icons::QUEUE, 52.0, false)
        .tip("Up next")
        .clicked()
    {
        actions.push(Action::ShowUpNext);
    }
    let speed = pb.player.speed();
    if widgets::pill_key(ui, &fmt_speed(speed), (speed - 1.0).abs() > 1e-3)
        .tip("Speed")
        .clicked()
    {
        actions.push(Action::SetSpeed(next_speed(speed)));
    }
    let vol = pb.player.volume();
    let speaker = if vol <= 0.001 {
        icons::SPEAKER_X
    } else if vol < 0.5 {
        icons::SPEAKER_LOW
    } else {
        icons::SPEAKER_HIGH
    };
    if widgets::key(ui, speaker, 52.0, false)
        .tip(if vol <= 0.001 { "Unmute" } else { "Mute" })
        .clicked()
    {
        if vol <= 0.001 {
            actions.push(Action::SetVolume(state.unmute_volume.take().unwrap_or(1.0)));
        } else {
            state.unmute_volume = Some(vol);
            actions.push(Action::SetVolume(0.0));
        }
    }
    let mut vol_edit = vol;
    if widgets::slider(ui, &mut vol_edit, 0.0..=1.5, 130.0).changed() {
        actions.push(Action::SetVolume(vol_edit));
    }
    ui.add_space(6.0);
}

/// Rewind, play and forward, centred on the row.
fn transport(
    ui: &mut egui::Ui,
    row: egui::Rect,
    pb: &crate::playback::Playback,
    step: f64,
    actions: &mut Vec<Action>,
) {
    let paused = pb.player.is_paused() || pb.player.state() == PlayerState::Ended;
    let centre_w = 56.0 + 16.0 + 72.0 + 16.0 + 56.0;
    let centre = egui::Rect::from_center_size(row.center(), Vec2::new(centre_w, row.height()));
    let mut mid = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(centre)
            .layout(Layout::left_to_right(Align::Center)),
    );
    mid.spacing_mut().item_spacing.x = 16.0;
    if skip_button(&mut mid, -step).tip("Rewind").clicked() {
        actions.push(Action::SeekRelative(-step));
    }
    if play_button(&mut mid, paused)
        .tip(if paused { "Play" } else { "Pause" })
        .clicked()
    {
        actions.push(Action::TogglePause);
    }
    if skip_button(&mut mid, step).tip("Forward").clicked() {
        actions.push(Action::SeekRelative(step));
    }
}

/// Close, hide, room, follow, bookmark and adjust.
fn view_keys(
    ui: &mut egui::Ui,
    pb: &crate::playback::Playback,
    state: &mut super::UiState,
    passthrough: bool,
    passthrough_available: bool,
    follow: bool,
    actions: &mut Vec<Action>,
) {
    if widgets::key(ui, icons::X, 52.0, false)
        .tip("Close")
        .clicked()
    {
        actions.push(Action::ClosePlayback);
    }
    if widgets::key(ui, icons::EYE_SLASH, 52.0, false)
        .tip("Hide")
        .clicked()
    {
        actions.push(Action::HideControls);
    }
    // Your room shows around a flat video's screen, or through a
    // side-by-side passthrough video's own mask.
    if passthrough_available {
        if pb.format.projection == Projection::Flat {
            if widgets::key(ui, icons::ARMCHAIR, 52.0, passthrough)
                .tip("Room")
                .clicked()
            {
                actions.push(Action::TogglePassthrough);
            }
        } else if let Some(f) = pb.mask_toggled()
            && widgets::key(ui, icons::ARMCHAIR, 52.0, pb.format.alpha_packed)
                .tip("Room, through the video's mask")
                .clicked()
        {
            actions.push(Action::SetFormat(Some(f)));
        }
    }
    // Only a flat screen follows: around 180°/360° video you look about.
    if pb.format.projection == Projection::Flat
        && widgets::key(ui, icons::BED, 52.0, follow)
            .tip("Screen follows you")
            .clicked()
    {
        actions.push(Action::ToggleFollow);
    }
    if widgets::key(ui, icons::BOOKMARK_SIMPLE, 52.0, false)
        .tip("Bookmark")
        .clicked()
    {
        actions.push(Action::AddBookmark);
    }
    if widgets::key(ui, icons::SLIDERS_HORIZONTAL, 52.0, state.adjust_open)
        .tip("Adjust")
        .clicked()
    {
        state.adjust_open = !state.adjust_open;
    }
}

const SPEEDS: [f64; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

/// The next speed in [`SPEEDS`], wrapping round.
fn next_speed(speed: f64) -> f64 {
    SPEEDS
        .iter()
        .copied()
        .find(|s| *s > speed + 1e-3)
        .unwrap_or(SPEEDS[0])
}

fn fmt_speed(speed: f64) -> String {
    let s = format!("{speed:.2}");
    format!("{}×", s.trim_end_matches('0').trim_end_matches('.'))
}

/// A small raised pill naming the video's format.
fn format_pill(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let g = ui.painter().layout_no_wrap(
        text.to_string(),
        theme::font(Weight::SemiBold, 14.0),
        theme::TEXT_2,
    );
    let (rect, resp) = ui.allocate_exact_size(g.size() + Vec2::new(20.0, 10.0), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    depth::raised(
        ui.painter(),
        rect,
        8.0,
        depth::Raised {
            top: Color32::from_rgb(46, 54, 65).lerp_to_gamma(Color32::from_rgb(62, 72, 86), t),
            bottom: Color32::from_rgb(33, 40, 49).lerp_to_gamma(Color32::from_rgb(44, 52, 63), t),
            light: Color32::from_white_alpha(26),
            lift: 0.3,
            glow: None,
        },
    );
    let fg = theme::TEXT_2.lerp_to_gamma(theme::TEXT, t);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, fg);
    resp
}

/// Short summary label and detailed tooltip for a video stream.
/// `hardware_on` is the hardware decoding setting.
pub(crate) fn stream_info_labels(
    v: &fp_media::StreamInfo,
    st: &fp_media::Stats,
    hardware_on: bool,
) -> (String, String) {
    let codec = match v.codec.as_str() {
        "hevc" => "HEVC",
        "h264" => "H.264",
        "av1" => "AV1",
        "vp9" => "VP9",
        c => c,
    };
    let hw = if st.hardware { "HW" } else { "SW" };
    let bits = if v.bit_depth > 8 {
        format!(" {}-bit", v.bit_depth)
    } else {
        String::new()
    };
    let badge_text = format!("{hw} · {codec}{bits}");

    let decoder = decoder_text(v, st, hardware_on);
    let chroma = v.chroma();
    let pixel = if v.pixel_format.is_empty() {
        format!("{}-bit {chroma}", v.bit_depth)
    } else {
        format!("{} ({}-bit {chroma})", v.pixel_format, v.bit_depth)
    };
    let tooltip = format!(
        "Decoder: {decoder}\nFormat: {codec} · {pixel}\nResolution: {}×{} @ {:.2} fps",
        v.width, v.height, v.fps
    );
    (badge_text, tooltip)
}

/// The decoder playing the video and, for software, why not hardware.
fn decoder_text(v: &fp_media::StreamInfo, st: &fp_media::Stats, hardware_on: bool) -> String {
    if st.hardware {
        format!("Hardware ({})", st.video_decoder)
    } else {
        let why = crate::playback::software_reason(v, hardware_on, st.hardware_error.as_deref());
        format!("Software ({}): {why}", st.video_decoder)
    }
}

/// A small raised pill naming the video stream format and decoder (HW / SW).
fn stream_pill(ui: &mut egui::Ui, text: &str, hardware: bool) -> egui::Response {
    let text_color = if hardware {
        theme::TEXT_2
    } else {
        theme::WARN.gamma_multiply(0.9)
    };
    let g = ui.painter().layout_no_wrap(
        text.to_string(),
        theme::font(Weight::SemiBold, 14.0),
        text_color,
    );
    let (rect, resp) = ui.allocate_exact_size(g.size() + Vec2::new(20.0, 10.0), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    depth::raised(
        ui.painter(),
        rect,
        8.0,
        depth::Raised {
            top: Color32::from_rgb(46, 54, 65).lerp_to_gamma(Color32::from_rgb(62, 72, 86), t),
            bottom: Color32::from_rgb(33, 40, 49).lerp_to_gamma(Color32::from_rgb(44, 52, 63), t),
            light: Color32::from_white_alpha(26),
            lift: 0.3,
            glow: None,
        },
    );
    let fg = text_color.lerp_to_gamma(theme::TEXT, t);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, fg);
    resp
}

/// The big round play / pause button: a glossy accent dome that glows on
/// the bar.
fn play_button(ui: &mut egui::Ui, paused: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(72.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let t = ui
            .ctx()
            .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
        let down = resp.is_pointer_button_down_on();
        let r = 34.0 + t * 2.0 - if down { 2.0 } else { 0.0 };
        let c = rect.center() + Vec2::new(0.0, if down { 0.5 } else { -t });
        let dome = egui::Rect::from_center_size(c, Vec2::splat(r * 2.0));
        let lit = |a: Color32, b: Color32| a.lerp_to_gamma(b, t);
        depth::raised(
            ui.painter(),
            dome,
            r,
            depth::Raised {
                top: lit(
                    Color32::from_rgb(104, 198, 255),
                    Color32::from_rgb(140, 214, 255),
                ),
                bottom: lit(
                    Color32::from_rgb(14, 106, 190),
                    Color32::from_rgb(22, 128, 222),
                ),
                light: Color32::from_white_alpha(150),
                lift: if down { 0.3 } else { 0.9 },
                glow: Some(theme::ACCENT.gamma_multiply(0.45 + 0.25 * t)),
            },
        );
        if paused {
            widgets::play_mark(ui.painter(), c, 28.0, Color32::WHITE);
        } else {
            widgets::pause_mark(ui.painter(), c, 26.0, Color32::WHITE);
        }
    }
    resp
}

/// Seek back or forward by `secs`: a raised key with a circular arrow and
/// the seconds in it.
fn skip_button(ui: &mut egui::Ui, secs: f64) -> egui::Response {
    let icon = if secs < 0.0 {
        icons::ARROW_COUNTER_CLOCKWISE
    } else {
        icons::ARROW_CLOCKWISE
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let (face, fg) = widgets::key_face(ui, &resp, 56.0, false);
        let c = face.center();
        let p = ui.painter();
        p.text(c, egui::Align2::CENTER_CENTER, icon, theme::icon(32.0), fg);
        p.text(
            c + Vec2::new(0.0, 1.0),
            egui::Align2::CENTER_CENTER,
            format!("{}", secs.abs().round() as i64),
            theme::font(Weight::Bold, 10.0),
            fg,
        );
    }
    resp
}

fn seek_bar(
    ui: &mut egui::Ui,
    width: f32,
    state: &mut super::UiState,
    pb: &mut crate::playback::Playback,
    actions: &mut Vec<Action>,
) {
    let duration = pb.player.duration().max(0.001);
    // A transcode the server can't start at a time plays straight through.
    let sense = if pb.player.can_seek() {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 36.0), sense);
    let resp = if pb.player.can_seek() {
        resp
    } else {
        resp.on_hover_text("This transcode plays from the start, without seeking")
    };
    let active = resp.hovered() || resp.dragged();
    let t = ui.ctx().animate_bool_with_time(resp.id, active, 0.12);
    let painter = ui.painter();
    let h = 8.0 + t * 4.0;
    let track = egui::Rect::from_center_size(rect.center(), Vec2::new(rect.width(), h));
    let round = egui::CornerRadius::same((h / 2.0) as u8);
    depth::sunken(painter, track, h / 2.0, widgets::WELL);
    // Haptic heatmap just above the track.
    if let Some(hm) = &pb.heatmap {
        let n = hm.colors.len().max(1) as f32;
        let w = track.width() / n;
        for (i, c) in hm.colors.iter().enumerate() {
            let x = track.left() + i as f32 * w;
            let r = egui::Rect::from_min_max(
                egui::pos2(x, track.top() - 7.0),
                egui::pos2(x + w + 0.5, track.top() - 3.0),
            );
            painter.rect_filled(r, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
        }
    }
    let pos = state.scrub.unwrap_or_else(|| pb.player.position());
    let frac = (pos / duration).clamp(0.0, 1.0) as f32;
    let to_x = |f: f32| track.left() + track.width() * f;
    // Where the pointer would seek to, previewed in a lighter tone.
    if let Some(p) = resp.hover_pos()
        && !resp.dragged()
        && pb.player.can_seek()
    {
        let hx = p.x.clamp(track.left(), track.right());
        if hx > to_x(frac) {
            let ahead = egui::Rect::from_min_max(
                egui::pos2(to_x(frac), track.top()),
                egui::pos2(hx, track.bottom()),
            );
            painter.rect_filled(ahead, round, Color32::from_white_alpha(40));
        }
    }
    let done = egui::Rect::from_min_max(track.min, egui::pos2(to_x(frac), track.bottom()));
    widgets::rail_fill(painter, track, done);
    for (m, _) in &pb.markers {
        let x = to_x((*m / duration).clamp(0.0, 1.0) as f32);
        painter.circle_filled(egui::pos2(x, track.center().y), 3.0, theme::WARN);
    }
    let knob = egui::pos2(to_x(frac), track.center().y);
    widgets::knob(painter, knob, 8.0 + t * 4.0);

    let to_time = |x: f32| ((x - track.left()) / track.width()).clamp(0.0, 1.0) as f64 * duration;
    if let Some(p) = resp
        .hover_pos()
        .or(resp.interact_pointer_pos())
        .filter(|_| pb.player.can_seek())
    {
        let tm = to_time(p.x);
        let marker = pb.markers.iter().find(|(m, _)| {
            ((m / duration) as f32 * track.width() - (p.x - track.left())).abs() < 8.0
        });
        let label = match marker {
            Some((_, name)) => format!("{} · {name}", fmt_time(tm)),
            None => fmt_time(tm),
        };
        // A bubble above the track, on the panel's top layer. A marker's
        // name can be longer than the bar is wide.
        let room = ui.max_rect();
        let g = widgets::one_line(
            ui.painter(),
            label,
            theme::font(Weight::SemiBold, 15.0),
            theme::BG,
            room.width() - 16.0,
        );
        let size = g.size() + Vec2::new(16.0, 8.0);
        let x = widgets::fit(p.x - size.x / 2.0, size.x, room.left(), room.right());
        let bubble = egui::Rect::from_min_size(egui::pos2(x, track.top() - size.y - 10.0), size);
        let top = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("seek-time"),
        ));
        top.rect_filled(bubble, egui::CornerRadius::same(8), theme::TEXT);
        top.galley(bubble.min + Vec2::new(8.0, 4.0), g, theme::BG);
    }
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
    {
        state.scrub = Some(to_time(p.x));
    }
    if resp.drag_stopped() {
        if let Some(t) = state.scrub.take() {
            actions.push(Action::Seek(t));
        }
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        // Snap to a marker when clicking right on it.
        let t = to_time(p.x);
        let snap = pb
            .markers
            .iter()
            .map(|m| m.0)
            .find(|m| ((m - t) / duration).abs() * (track.width() as f64) < 8.0);
        actions.push(Action::Seek(snap.unwrap_or(t)));
    }
}

const TABS: [(&str, &str); 7] = [
    (icons::FRAME_CORNERS, "Format"),
    (icons::ARROWS_OUT_CARDINAL, "Position"),
    (icons::CUBE, "Stereo"),
    (icons::SUN, "Picture"),
    (icons::EYEGLASSES, "Passthrough"),
    (icons::SUBTITLES, "Audio & subs"),
    (icons::VIBRATE, "Haptics"),
];

/// Format, view adjustments, tracks and haptics for the open video.
pub fn adjust_panel(ctx: &egui::Context, v: &mut View) {
    widgets::panel_slab(ctx, 28.0);
    let frame = egui::Frame::new()
        .outer_margin(widgets::SHADOW_ROOM)
        .inner_margin(egui::Margin::same(22));
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let Some(pb) = v.playback.as_deref_mut() else {
            return;
        };
        ui.label(
            RichText::new("Adjust")
                .font(theme::font(Weight::Bold, 26.0))
                .color(theme::TEXT),
        );
        ui.add_space(4.0);
        // Haptics is the last tab, left out while haptics are off.
        let tabs = &TABS[..TABS.len() - usize::from(!v.settings.haptics_enabled)];
        if v.state.adjust_tab >= tabs.len() {
            v.state.adjust_tab = 0;
        }
        if let Some(i) = widgets::segmented(ui, tabs, v.state.adjust_tab) {
            v.state.adjust_tab = i;
        }
        ui.add_space(10.0);
        // Save and reset stay at the bottom while the page scrolls.
        let footer = v.state.adjust_tab <= 4;
        let footer_h = if footer { 64.0 } else { 0.0 };
        let page = ui.available_height() - footer_h;
        egui::ScrollArea::vertical()
            .max_height(page)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                let before = pb.settings;
                match v.state.adjust_tab {
                    0 => format_tab(ui, pb, v.settings.hardware_decoding, v.actions),
                    1 => position_tab(ui, &mut pb.settings),
                    2 => stereo_tab(ui, &mut pb.settings),
                    3 => picture_tab(ui, &mut pb.settings),
                    4 => {
                        widgets::section_label(ui, "Passthrough videos");
                        ui.label(
                            RichText::new("This video's settings, saved with its adjustments.")
                                .color(theme::TEXT_2),
                        );
                        ui.add_space(6.0);
                        chroma_tab(
                            ui,
                            &mut pb.settings,
                            pb.format,
                            &v.settings.default_view,
                            v.passthrough_available,
                            v.actions,
                        );
                    }
                    5 => audio_and_text(
                        ui,
                        pb,
                        &v.settings.subtitle_language,
                        &mut v.state.foreign_subtitles_open,
                        v.actions,
                    ),
                    _ => haptics_tab(ui, pb.script_count, v.settings, v.devices),
                }
                if pb.settings != before {
                    pb.settings_dirty = true;
                }
            });
        if footer {
            adjust_footer(ui, pb, v.actions);
        }
    });
}

/// Format tab: the detected format and overrides, lens or screen, and
/// what is decoding the video.
fn format_tab(
    ui: &mut egui::Ui,
    pb: &mut crate::playback::Playback,
    hardware_decoding: bool,
    actions: &mut Vec<Action>,
) {
    let s = &mut pb.settings;
    let d = ViewSettings::default();
    let user = pb.evidence == fp_core::format::Evidence::User;
    widgets::section_label(ui, "Format");
    ui.label(
        RichText::new(format!(
            "Detected {} ({})",
            pb.detected.format.label(),
            pb.detected.evidence.label()
        ))
        .color(theme::TEXT_2),
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
        if widgets::chip_icon(ui, Some(icons::MAGIC_WAND), "Automatic", !user).clicked() {
            actions.push(Action::SetFormat(None));
        }
        for (label, f) in format_choices() {
            // The chips set the layout; a packed mask stays.
            if widgets::chip(ui, label, user && pb.format.same_layout(&f)).clicked() {
                actions.push(Action::SetFormat(Some(VideoFormat {
                    alpha_packed: pb.format.alpha_packed,
                    ..f
                })));
            }
        }
    });
    if let Projection::Fisheye { fov } = pb.format.projection {
        ui.add_space(6.0);
        widgets::section_label(ui, "Fisheye lens");
        widgets::rows(ui, |r| {
            let mut f = fov;
            if r.slider("Field of view", None, &mut f, 120.0..=300.0, fov, "°", 0) {
                actions.push(Action::SetFormat(Some(VideoFormat {
                    projection: Projection::Fisheye { fov: f.round() },
                    ..pb.format
                })));
            }
            r.slider("Lens k1", None, &mut s.lens_k1, -0.5..=0.5, 0.0, "", 2);
            r.slider("Lens k2", None, &mut s.lens_k2, -0.5..=0.5, 0.0, "", 2);
        });
        ui.add_space(6.0);
        widgets::section_label(ui, "Lens centre");
        let images: &[[&str; 2]] = match pb.format.stereo {
            StereoLayout::Mono => &[["Across", "Up"]],
            StereoLayout::SideBySide | StereoLayout::HalfSideBySide => {
                &[["Left, across", "Left, up"], ["Right, across", "Right, up"]]
            }
            StereoLayout::TopBottom | StereoLayout::HalfTopBottom => {
                &[["Top, across", "Top, up"], ["Bottom, across", "Bottom, up"]]
            }
        };
        widgets::rows(ui, |r| {
            for (i, titles) in images.iter().enumerate() {
                for (j, title) in titles.iter().enumerate() {
                    // Shown in percent of the image.
                    let mut v = s.lens_centre[i][j] * 100.0;
                    if r.slider(title, None, &mut v, -10.0..=10.0, 0.0, "%", 1) {
                        s.lens_centre[i][j] = v / 100.0;
                    }
                }
            }
        });
        ui.label(
            RichText::new(
                "Where each image circle's centre is, if not in the middle. \
                 Point at a slider and push the thumbstick left or right \
                 for fine steps.",
            )
            .color(theme::TEXT_3),
        );
    }
    if pb.format.projection == Projection::Flat {
        ui.add_space(6.0);
        widgets::section_label(ui, "Screen");
        widgets::rows(ui, |r| {
            r.slider(
                "Distance",
                None,
                &mut s.screen_distance,
                1.0..=20.0,
                d.screen_distance,
                " m",
                1,
            );
            r.slider(
                "Width",
                Some("Its longer side: a portrait video's height."),
                &mut s.screen_width,
                0.5..=30.0,
                d.screen_width,
                " m",
                1,
            );
            r.slider(
                "Curvature",
                None,
                &mut s.screen_curvature,
                0.0..=1.0,
                0.0,
                "",
                2,
            );
        });
    }
    if let Some(video) = pb.player.info().video_stream() {
        ui.add_space(6.0);
        widgets::section_label(ui, "Video stream");
        widgets::rows(ui, |r| {
            let st = pb.player.stats();
            let decoder = decoder_text(video, &st, hardware_decoding);
            r.row("Decoder", None, |ui| {
                ui.label(RichText::new(decoder).color(if st.hardware {
                    theme::TEXT
                } else {
                    theme::WARN
                }));
            });
            let codec_name = match video.codec.as_str() {
                "hevc" => "HEVC (H.265)".to_string(),
                "h264" => "H.264 (AVC)".to_string(),
                "av1" => "AV1".to_string(),
                "vp9" => "VP9".to_string(),
                c => c.to_uppercase(),
            };
            let format_desc = if video.bit_depth > 8 {
                format!("{codec_name} {}-bit ({})", video.bit_depth, video.chroma())
            } else {
                format!("{codec_name} 8-bit ({})", video.chroma())
            };
            r.row("Format", None, |ui| {
                ui.label(RichText::new(format_desc).color(theme::TEXT_2));
            });
            r.row("Resolution", None, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} × {} @ {:.2} fps",
                        video.width, video.height, video.fps
                    ))
                    .color(theme::TEXT_2),
                );
            });
            if !video.pixel_format.is_empty() {
                r.row("Pixel format", None, |ui| {
                    ui.label(RichText::new(&video.pixel_format).color(theme::TEXT_2));
                });
            }
        });
    }
}

fn position_tab(ui: &mut egui::Ui, s: &mut ViewSettings) {
    let d = ViewSettings::default();
    widgets::rows(ui, |r| {
        r.slider("Yaw", None, &mut s.yaw, -180.0..=180.0, 0.0, "°", 0);
        r.slider("Pitch", None, &mut s.pitch, -90.0..=90.0, 0.0, "°", 0);
        r.slider("Roll", None, &mut s.roll, -45.0..=45.0, 0.0, "°", 0);
        r.slider("Zoom", None, &mut s.zoom, 0.5..=2.5, 1.0, "×", 2);
        r.slider(
            "Height",
            Some("Raise or lower yourself in the scene."),
            &mut s.height,
            -1.0..=1.0,
            d.height,
            " m",
            2,
        );
        r.slider(
            "Forward",
            Some("Move closer to or back from the scene."),
            &mut s.forward,
            -1.0..=1.0,
            d.forward,
            " m",
            2,
        );
    });
    ui.label(
        RichText::new(
            "Or hold a grip and the trigger and drag the picture; \
             press the thumbstick to undo. Right grip and thumbstick \
             up / down changes your height.",
        )
        .color(theme::TEXT_3),
    );
}

fn stereo_tab(ui: &mut egui::Ui, s: &mut ViewSettings) {
    let d = ViewSettings::default();
    widgets::rows(ui, |r| {
        r.slider(
            "Depth",
            Some("Eye separation; lower if it's hard to fuse."),
            &mut s.ipd_offset,
            -5.0..=5.0,
            0.0,
            "°",
            1,
        );
        r.slider(
            "Vertical align",
            None,
            &mut s.vertical_align,
            -3.0..=3.0,
            0.0,
            "°",
            1,
        );
        r.slider(
            "Rotation align",
            None,
            &mut s.rotation_align,
            -3.0..=3.0,
            0.0,
            "°",
            1,
        );
        r.switch(
            "Swap eyes",
            Some("For left/right swapped files."),
            &mut s.swap_eyes,
        );
        r.slider(
            "Subtitle depth",
            None,
            &mut s.subtitle_distance,
            0.8..=10.0,
            d.subtitle_distance,
            " m",
            1,
        );
    });
}

fn picture_tab(ui: &mut egui::Ui, s: &mut ViewSettings) {
    widgets::rows(ui, |r| {
        r.slider(
            "Brightness",
            None,
            &mut s.brightness,
            -0.5..=0.5,
            0.0,
            "",
            2,
        );
        r.slider("Contrast", None, &mut s.contrast, 0.5..=1.5, 1.0, "", 2);
        r.slider("Saturation", None, &mut s.saturation, 0.0..=2.0, 1.0, "", 2);
        r.slider("Gamma", None, &mut s.gamma, 0.5..=2.0, 1.0, "", 2);
        r.slider("Sharpen", None, &mut s.sharpen, 0.0..=1.0, 0.0, "", 2);
    });
}

/// Save, reset and keyframes, below the page.
fn adjust_footer(ui: &mut egui::Ui, pb: &crate::playback::Playback, actions: &mut Vec<Action>) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let label = if pb.record_id.is_some() {
            "Save for this video"
        } else {
            "Keep for this session"
        };
        if ui
            .add_enabled_ui(pb.settings_dirty, |ui| {
                widgets::button(ui, Some(icons::CHECK), label, Kind::Primary)
            })
            .inner
            .clicked()
        {
            actions.push(Action::SaveView);
        }
        if widgets::button(
            ui,
            Some(icons::ARROW_COUNTER_CLOCKWISE),
            "Reset",
            Kind::Secondary,
        )
        .clicked()
        {
            actions.push(Action::ResetView);
        }
        if widgets::icon_button(ui, icons::TIMER, 48.0, false)
            .tip("Keyframe")
            .clicked()
        {
            actions.push(Action::AddKeyframe);
        }
        if !pb.keyframes.frames.is_empty()
            && widgets::button(
                ui,
                None,
                &format!("Clear {} keyframes", pb.keyframes.frames.len()),
                Kind::Ghost,
            )
            .clicked()
        {
            actions.push(Action::ClearKeyframes);
        }
    });
}

/// Chroma key colours offered as one tap.
const KEY_COLORS: [(&str, [f32; 3]); 3] = [
    ("Green screen", [0.0, 1.0, 0.0]),
    ("Blue screen", [0.08, 0.04, 1.0]),
    ("Magenta", [1.0, 0.0, 1.0]),
];

/// Passthrough tab: the video's own packed mask (`_alpha` videos) or a
/// chroma key, so the room shows where the video's background was. The key
/// follows Settings > Passthrough until it is changed here for this video.
fn chroma_tab(
    ui: &mut egui::Ui,
    s: &mut ViewSettings,
    format: VideoFormat,
    global: &ViewSettings,
    passthrough_available: bool,
    actions: &mut Vec<Action>,
) {
    widgets::rows(ui, |r| {
        let mut packed = format.alpha_packed;
        let desc = if packed && format.alpha_pack_scale().is_none() {
            "Only side-by-side videos can carry a mask: set the format to a side-by-side one."
        } else {
            "For videos with a built-in see-through mask. On by itself for _alpha files."
        };
        if r.switch("Use the video's mask", Some(desc), &mut packed) {
            actions.push(Action::SetFormat(Some(VideoFormat {
                alpha_packed: packed,
                ..format
            })));
        }
        let mut own = s.own_key();
        if r.switch(
            "Own settings for this video",
            Some(if own {
                "Saved with this video. Turn off to use the global settings."
            } else {
                "Using the global settings. Change anything below to adjust this video."
            }),
            &mut own,
        ) {
            if own {
                s.set_key(global);
            }
            s.key_own = Some(own);
            actions.push(Action::SaveKey);
        }
    });
    let mut shown = s.with_global_key(global);
    if chroma_controls(ui, &mut shown, passthrough_available) {
        s.set_key(&shown);
        s.key_own = Some(true);
        actions.push(Action::SaveKey);
    }
}

/// "Remove the background" and its colour and fine-tune controls, for one
/// video or the global default. True when anything changed.
pub(super) fn chroma_controls(
    ui: &mut egui::Ui,
    s: &mut ViewSettings,
    passthrough_available: bool,
) -> bool {
    let before = s.key();
    let d = ViewSettings::default();
    widgets::rows(ui, |r| {
        r.switch(
            "Remove the background",
            Some(if passthrough_available {
                "Makes one colour of the video see-through, so your room shows behind it."
            } else {
                "Makes one colour of the video see-through. This headset doesn't share \
                 passthrough with apps, so it turns dark instead."
            }),
            &mut s.chroma_key,
        );
    });
    if s.chroma_key {
        premium::label(ui, "Background colour");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for (label, c) in KEY_COLORS {
                let on = s.key_color.iter().zip(c).all(|(a, b)| (a - b).abs() < 0.02);
                if premium::swatch(ui, label, c, on).clicked() {
                    s.key_color = c;
                }
            }
        });
        widgets::rows(ui, |r| {
            r.row("Colour", None, |ui| {
                let [cr, cg, cb] = s.key_color.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
                let (rect, _) = ui.allocate_exact_size(Vec2::new(64.0, 32.0), Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(8),
                    Color32::from_rgb(cr, cg, cb),
                );
            });
            r.slider(
                "Red",
                None,
                &mut s.key_color[0],
                0.0..=1.0,
                d.key_color[0],
                "",
                2,
            );
            r.slider(
                "Green",
                None,
                &mut s.key_color[1],
                0.0..=1.0,
                d.key_color[1],
                "",
                2,
            );
            r.slider(
                "Blue",
                None,
                &mut s.key_color[2],
                0.0..=1.0,
                d.key_color[2],
                "",
                2,
            );
        });
        premium::label(ui, "Fine-tune");
        widgets::rows(ui, |r| {
            r.slider(
                "Similarity",
                Some("How close to the colour a pixel can be and still disappear."),
                &mut s.key_similarity,
                0.0..=1.0,
                d.key_similarity,
                "",
                2,
            );
            r.slider(
                "Edge softness",
                None,
                &mut s.key_smoothness,
                0.0..=0.5,
                d.key_smoothness,
                "",
                2,
            );
            r.slider(
                "Spill removal",
                Some("Takes the background's tint off hair and edges."),
                &mut s.key_spill,
                0.0..=1.0,
                d.key_spill,
                "",
                2,
            );
        });
    }
    s.key() != before
}

/// Videos with more subtitles than this list those in other languages only
/// when asked.
const FOLD_SUBTITLES: usize = 6;

fn audio_and_text(
    ui: &mut egui::Ui,
    pb: &crate::playback::Playback,
    language: &str,
    foreign_open: &mut bool,
    actions: &mut Vec<Action>,
) {
    if !pb.transcodes.is_empty() {
        widgets::section_label(ui, "Quality");
        let playing = pb.transcode.as_ref().map(|t| t.id.as_str());
        widgets::rows(ui, |r| {
            r.row("Original", Some("The file as it is"), |ui| {
                if radio(ui, playing.is_none()).clicked() {
                    actions.push(Action::SelectTranscode(None));
                }
            });
            for t in &pb.transcodes {
                let detail = if t.seekable {
                    "Transcoded by the server"
                } else {
                    "Transcoded by the server · plays from the start, without seeking"
                };
                r.row(&t.label, Some(detail), |ui| {
                    if radio(ui, playing == Some(t.id.as_str())).clicked() {
                        actions.push(Action::SelectTranscode(Some(t.id.clone())));
                    }
                });
            }
        });
    }
    let info = pb.player.info();
    widgets::section_label(ui, "Audio");
    let current = pb.player.audio_stream();
    let streams: Vec<_> = info.audio_streams().collect();
    if streams.is_empty() {
        ui.label(RichText::new("No audio").color(theme::TEXT_3));
    } else {
        widgets::rows(ui, |r| {
            for s in &streams {
                let mut title = crate::subtitles::track_name(s);
                if s.ambisonic {
                    title += " · spatial";
                }
                let desc = format!("{} · {} ch", s.codec, s.channels);
                r.row(&title, Some(desc.as_str()), |ui| {
                    if radio(ui, current == Some(s.index)).clicked() {
                        actions.push(Action::SelectAudio(s.index));
                    }
                });
            }
        });
    }
    widgets::section_label(ui, "Subtitles");
    let sub_stream = pb.player.subtitle_stream();
    let file = pb.active_subtitle_file;
    let on = |pick: Pick| match pick {
        Pick::Off => sub_stream.is_none() && file.is_none(),
        Pick::File(i) => file == Some(i),
        Pick::Stream(s) => sub_stream == Some(s) && file.is_none(),
    };
    let rows = crate::subtitles::rows(language, info, &pb.subtitle_files);
    let fold = rows.len() > FOLD_SUBTITLES;
    let (shown, folded): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|row| !fold || !row.foreign || on(row.pick));
    let mut track = |r: &mut widgets::Rows, title: &str, detail: &str, pick: Pick| {
        let detail = Some(detail).filter(|d| !d.is_empty());
        r.row(title, detail, |ui| {
            if radio(ui, on(pick)).clicked() {
                if let Pick::File(i) = pick {
                    actions.push(Action::SelectSubtitleFile(Some(i)));
                } else {
                    actions.push(Action::SelectSubtitleFile(None));
                    actions.push(Action::SelectSubtitleStream(pick.stream()));
                }
            }
        });
    };
    widgets::rows(ui, |r| {
        track(r, "Off", "", Pick::Off);
        for row in &shown {
            track(r, &row.title, &row.detail, row.pick);
        }
        if !folded.is_empty() {
            let more = format!("{} more", folded.len());
            r.row("Other languages", Some(more.as_str()), |ui| {
                let label = if *foreign_open { "Hide" } else { "Show" };
                if widgets::button(ui, None, label, Kind::Secondary).clicked() {
                    *foreign_open = !*foreign_open;
                }
            });
            if *foreign_open {
                for row in &folded {
                    track(r, &row.title, &row.detail, row.pick);
                }
            }
        }
    });
    if !pb.markers.is_empty() {
        widgets::section_label(ui, "Chapters and bookmarks");
        widgets::rows(ui, |r| {
            for (t, name) in &pb.markers {
                r.row(name, Some(fmt_time(*t).as_str()), |ui| {
                    if widgets::icon_button(ui, icons::PLAY, 40.0, false)
                        .tip("Go")
                        .clicked()
                    {
                        actions.push(Action::Seek(*t));
                    }
                });
            }
        });
    }
    let st = pb.player.stats();
    ui.label(
        RichText::new(format!(
            "Video: {}{} · dropped {} of {} · stalled {} · Audio: {} via {}",
            st.video_decoder,
            if st.hardware { " (hardware)" } else { "" },
            st.frames_dropped,
            st.frames_shown + st.frames_dropped,
            st.stalls,
            st.audio_decoder,
            st.audio_sink
        ))
        .font(theme::font(Weight::Regular, 14.0))
        .color(theme::TEXT_3),
    );
}

/// A round radio mark, filled when chosen; the whole row reads as the choice.
fn radio(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let p = ui.painter();
    p.circle_filled(
        rect.center(),
        18.0,
        Color32::from_white_alpha((t * 18.0) as u8),
    );
    if on {
        p.circle_filled(rect.center(), 11.0, theme::ACCENT);
        p.circle_filled(rect.center(), 4.5, Color32::WHITE);
    } else {
        p.circle_stroke(
            rect.center(),
            10.0,
            egui::Stroke::new(2.0_f32, theme::TEXT_3.lerp_to_gamma(Color32::WHITE, t)),
        );
    }
    resp
}

fn haptics_tab(
    ui: &mut egui::Ui,
    scripts: usize,
    settings: &mut crate::settings::Settings,
    devices: &[fp_haptics::DeviceStatus],
) {
    ui.label(
        RichText::new(if scripts == 0 {
            "No haptic script for this video.".to_string()
        } else {
            format!("{scripts} script axis(es) loaded.")
        })
        .color(theme::TEXT_2),
    );
    let mut offset = settings.haptics.offset_ms as f32;
    let l0 = settings
        .haptics
        .axes
        .entry(fp_haptics::Axis::L0)
        .or_default();
    let (mut lo, mut hi) = (l0.min * 100.0, l0.max * 100.0);
    let (mut a, mut b, mut o) = (false, false, false);
    let mut invert = l0.invert;
    widgets::rows(ui, |r| {
        o = r.slider(
            "Timing offset",
            None,
            &mut offset,
            -500.0..=500.0,
            0.0,
            " ms",
            0,
        );
        a = r.slider("Stroke bottom", None, &mut lo, 0.0..=100.0, 0.0, "%", 0);
        b = r.slider("Stroke top", None, &mut hi, 0.0..=100.0, 100.0, "%", 0);
        r.switch("Invert stroke", None, &mut invert);
    });
    let l0 = settings
        .haptics
        .axes
        .entry(fp_haptics::Axis::L0)
        .or_default();
    l0.invert = invert;
    if a || b {
        l0.min = (lo / 100.0).min(hi / 100.0 - 0.05).max(0.0);
        l0.max = (hi / 100.0).max(l0.min + 0.05).min(1.0);
    }
    if o {
        settings.haptics.offset_ms = offset.round() as i64;
    }
    widgets::section_label(ui, "Devices");
    if devices.is_empty() {
        ui.label(
            RichText::new("None connected. Add one in Settings › Haptics.").color(theme::TEXT_3),
        );
    } else {
        widgets::rows(ui, |r| {
            for d in devices {
                let desc = if d.connected {
                    "Connected".to_string()
                } else {
                    d.last_error
                        .clone()
                        .unwrap_or_else(|| "Not connected".into())
                };
                r.row(&d.name, Some(desc.as_str()), |ui| {
                    let c = if d.connected { theme::OK } else { theme::ERROR };
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, c);
                });
            }
        });
    }
}

/// Subtitle cues, drawn large on a dark plate on a transparent panel. Text
/// is set by `shaper` once the fonts have loaded, by egui (Latin, Greek and
/// Cyrillic only) before. With `stereo`, the cues are a 3D video's pictures
/// (see [`stereo_pictures`]). Pictures are widened by the video's
/// `pixel_aspect`, as the video is.
pub fn subtitles(
    ctx: &egui::Context,
    cues: &[fp_media::subtitle::Cue],
    textures: &mut Vec<egui::TextureHandle>,
    shaper: Option<&mut super::subtitle_text::SubtitleText>,
    stereo: Option<&crate::subtitles::StereoSubs>,
    pixel_aspect: f32,
) {
    if let Some(s) = stereo {
        stereo_pictures(ctx, cues, textures, s);
        return;
    }
    let frame = egui::Frame::new().fill(Color32::TRANSPARENT);
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let text: Vec<&str> = cues
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.trim().is_empty())
            .collect();
        textures.clear();
        for c in cues {
            for (i, b) in c.bitmaps.iter().enumerate() {
                if b.width == 0 || b.height == 0 || b.rgba.len() < (b.width * b.height * 4) as usize
                {
                    continue;
                }
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [b.width as usize, b.height as usize],
                    &b.rgba,
                );
                textures.push(ctx.load_texture(
                    format!("sub{i}"),
                    img,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
            for t in textures.iter() {
                let size = t.size_vec2() * Vec2::new(pixel_aspect, 1.0);
                let scale = (ui.available_width() / size.x).min(1.0);
                ui.image((t.id(), size * scale));
            }
            if text.is_empty() {
                return;
            }
            let joined = text.join("\n");
            let (size, width) = (54.0, ui.available_width() - 40.0);
            let pad = Vec2::new(18.0, 8.0);
            if let Some(shaper) = shaper {
                let ppp = ctx.pixels_per_point();
                let img = shaper.render(&joined, size * ppp, width * ppp);
                if img.size[0] == 0 {
                    return;
                }
                let lines = Vec2::new(img.size[0] as f32, img.size[1] as f32) / ppp;
                let tex = ctx.load_texture("sub-text", img, egui::TextureOptions::LINEAR);
                let (rect, _) = ui.allocate_exact_size(lines + pad * 2.0, Sense::hover());
                ui.painter()
                    .rect_filled(rect, 12.0, Color32::from_black_alpha(150));
                ui.painter().image(
                    tex.id(),
                    egui::Rect::from_center_size(rect.center(), lines),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                textures.push(tex);
                return;
            }
            let font = egui::FontId::proportional(size);
            let galley = ui.painter().layout(joined, font, Color32::WHITE, width);
            let (rect, _) = ui.allocate_exact_size(galley.size() + pad * 2.0, Sense::hover());
            ui.painter()
                .rect_filled(rect, 12.0, Color32::from_black_alpha(150));
            ui.painter().galley(rect.min + pad, galley, Color32::WHITE);
        });
    });
}

/// A 3D video's picture subtitles: each eye's parts on its half of the
/// panel, squeezed to half width as in a side-by-side frame, since each eye
/// is shown its half stretched across the panel. They keep their places in
/// the frame, so the two copies keep the depth they were made with.
fn stereo_pictures(
    ctx: &egui::Context,
    cues: &[fp_media::subtitle::Cue],
    textures: &mut Vec<egui::TextureHandle>,
    s: &crate::subtitles::StereoSubs,
) {
    let frame = egui::Frame::new().fill(Color32::TRANSPARENT);
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        textures.clear();
        let panel = ui.max_rect();
        let ((half_width, _), (sx, sy)) = s.half();
        // A point per pixel of the eye's picture as shown, as 2D pictures
        // get, with the frame centred on the panel.
        let g = 1.0;
        let left = (panel.width() - half_width as f32 * sx * g) / 2.0;
        let parts: Vec<_> = (0..2)
            .flat_map(|eye| {
                cues.iter()
                    .flat_map(|c| &c.bitmaps)
                    .filter_map(move |b| s.part(b, eye).map(|p| (eye, p)))
            })
            .collect();
        // The lowest picture rests on the panel's bottom edge.
        let Some(lowest) = parts
            .iter()
            .map(|(_, p)| (p.y + p.height as i32) as f32 * sy * g)
            .reduce(f32::max)
        else {
            return;
        };
        for (i, (eye, p)) in parts.iter().enumerate() {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [p.width as usize, p.height as usize],
                &p.rgba,
            );
            let tex = ctx.load_texture(format!("sub{i}"), img, egui::TextureOptions::LINEAR);
            let min = egui::pos2(
                panel.left() + (*eye as f32 * panel.width() + left + p.x as f32 * sx * g) / 2.0,
                panel.bottom() - lowest + p.y as f32 * sy * g,
            );
            let size = Vec2::new(p.width as f32 * sx * g / 2.0, p.height as f32 * sy * g);
            ui.painter().image(
                tex.id(),
                egui::Rect::from_min_size(min, size),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            textures.push(tex);
        }
    });
}
