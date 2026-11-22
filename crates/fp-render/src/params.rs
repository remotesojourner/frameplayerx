//! Builds the projection shader's uniform block.

use fp_core::format::{Projection, StereoLayout, VideoFormat};
use fp_core::view::ViewSettings;
use fp_media::PixelLayout;
use fp_media::frame::{ColorInfo, Matrix};
use fp_media::info::Transfer;
use glam::{EulerRot, Mat4, Quat, Vec2, Vec3, Vec4};
use std::sync::atomic::{AtomicBool, Ordering};

/// Matches `struct Params` in shaders/scene.wgsl (std140-compatible: every
/// member is 16-byte aligned).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SceneParams {
    pub inv_view_proj: [Mat4; 2],
    pub correction: Mat4,
    pub anchor_inv: Mat4,
    pub screen_inv: Mat4,
    pub mode: [u32; 4],
    pub proj: Vec4,
    pub lens: Vec4,
    pub circle: Vec4,
    pub centres: Vec4,
    pub screen: Vec4,
    pub yuv_r: Vec4,
    pub yuv_g: Vec4,
    pub yuv_b: Vec4,
    pub yuv_offset: Vec4,
    pub picture: Vec4,
    pub extra: Vec4,
    pub tex_size: Vec4,
    pub bg_color: Vec4,
    pub key: Vec4,
    pub key2: Vec4,
    pub viewer: Vec4,
    pub display: [u32; 4],
    pub hdr: Vec4,
    pub tp10: Vec4,
    pub tp10_uv: Vec4,
    /// Comfort focus (`focus.rs`): x, y the aim's angles about the cameras'
    /// baseline (`focus::aim_angles`); z the eyes' turn apart about the
    /// head's up, which its measurement leaves out; w the distance in focus,
    /// metres (0: none).
    pub focus: Vec4,
    /// Each eye's offset from its camera, in the video's frame.
    pub eyes: [Vec4; 2],
    /// Each eye's turn apart about the head's up, as a quaternion.
    pub relief: [Vec4; 2],
    /// x, y: the picture's share of the screen's width and height; black
    /// fills the rest (a cinema's screen). z, w: in a cinema, the depth
    /// each eye draws it at ([`crate::cinema::screen_depth`]).
    pub fill: Vec4,
    /// Screen glow ([`crate::glow`]): x its brightness beside the edge, as a
    /// share of the edge's light; y how far it reaches, metres.
    pub glow: Vec4,
    /// The picture's edge colours clockwise from its top left, each packed
    /// as 8-bit square roots of linear red, green and blue: as measured,
    /// then spread as the light is halfway out and at its reach, half and a
    /// quarter as many ([`crate::glow::COLOURS`] in all).
    pub glow_colours: [[u32; 4]; crate::glow::COLOURS / 4],
}

/// Sets `p` up for a Q10C picture of `height` rows sampled where it lies
/// (`planes_tp10.wgsl`), read in bands `luma` and `chroma`.
pub(crate) fn sample_tp10(
    p: &mut SceneParams,
    height: u32,
    luma: crate::q10c::SampleBands,
    chroma: crate::q10c::SampleBands,
) {
    let bands = |b: crate::q10c::SampleBands, rows: u32| {
        Vec4::new(b.step as f32, b.rows as f32, rows as f32, b.count as f32)
    };
    // TP10 reads 10-bit values as they are; P010 keeps them at the top of 16.
    p.yuv_offset.w = 1.0;
    p.tp10 = bands(luma, height);
    p.tp10_uv = bands(chroma, height.div_ceil(2));
}

/// The shader's `hdr`: an HDR scene peaking at `peak` nits on the screen's
/// white, both PQ-coded.
pub(crate) fn hdr_levels(peak: f32) -> Vec4 {
    Vec4::new(
        crate::hdr::pq_from_nits(peak),
        crate::hdr::pq_from_nits(crate::hdr::SDR_WHITE),
        0.0,
        0.0,
    )
}

/// Everything about the video that the projection pass needs, other than
/// the eye matrices.
#[derive(Clone, Copy, Debug)]
pub struct VideoParams {
    pub format: VideoFormat,
    pub settings: ViewSettings,
    /// Flat screens: where the screen's centre sits in the world (the screen
    /// faces +Z of this transform, i.e. towards a viewer looking down -Z).
    pub screen_pose: Mat4,
    /// Background around/behind the video, linear premultiplied RGBA.
    /// Alpha 0 lets passthrough show through.
    pub background: [f32; 4],
    /// Optional cinema around a flat video. The screen pose is centred on
    /// [`CinemaRoom::screen_centre`] in the room's local coordinates.
    pub cinema: Option<CinemaParams>,
    /// The room frame the video was centred in (recentering's anchor): a
    /// 180°/360° picture, its per-eye corrections and the viewer's height
    /// and forward turn with it.
    pub anchor: Quat,
    /// Clockwise turn that shows the video upright: 0, 90, 180 or 270
    /// degrees.
    pub rotation: u32,
    /// Width of a video pixel relative to its height: anamorphic video is
    /// shown this much wider than it is stored.
    pub pixel_aspect: f32,
}

impl Default for VideoParams {
    fn default() -> Self {
        VideoParams {
            format: VideoFormat::default(),
            settings: ViewSettings::default(),
            screen_pose: Mat4::from_translation(glam::vec3(0.0, 0.0, -4.0)),
            background: [0.008, 0.008, 0.012, 1.0],
            cinema: None,
            anchor: Quat::IDENTITY,
            rotation: 0,
            pixel_aspect: 1.0,
        }
    }
}

/// The cinema drawn round a flat video.
#[derive(Clone, Copy, Debug)]
pub struct CinemaParams {
    pub room: CinemaRoom,
    /// Room metres to world space, from [`CinemaRoom::pose`].
    pub pose: Mat4,
    /// House lights, from fully dimmed (0) to fully lit (1).
    pub lights: f32,
    /// How much the screen curves round the seats, 1 over its radius in
    /// metres, from [`CinemaRoom::curvature`].
    pub curvature: f32,
}

/// Rooms that failed to load or draw in this run, by [`CinemaRoom::slot`].
static FAILED: [AtomicBool; CinemaRoom::ALL.len()] =
    [const { AtomicBool::new(false) }; CinemaRoom::ALL.len()];

/// A cinema room, built into the player when its files are in
/// `crates/fp-render/rooms` (see docs/DEVELOPMENT.md). Its metres have the
/// eyes of one seat at the origin, looking along -Z. Everything but
/// [`Self::is_available`] needs an available room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CinemaRoom {
    Classic,
    Luxe,
}

impl CinemaRoom {
    pub const ALL: [CinemaRoom; 2] = [CinemaRoom::Classic, CinemaRoom::Luxe];

    pub(crate) const fn slot(self) -> usize {
        self as usize
    }

    /// Whether this build has the room and it hasn't failed to load.
    pub fn is_available(self) -> bool {
        crate::cinema_mesh::asset(self).is_some() && !FAILED[self.slot()].load(Ordering::Relaxed)
    }

    /// Leaves the room out from now on: it couldn't be loaded or drawn.
    pub(crate) fn fail(self) {
        FAILED[self.slot()].store(true, Ordering::Relaxed);
    }

    /// Whether this build has any room: without one the cinema is left out.
    pub fn any_available() -> bool {
        Self::ALL.iter().any(|room| room.is_available())
    }

    fn asset(self) -> crate::cinema_mesh::CinemaAsset {
        crate::cinema_mesh::asset(self).expect("an available cinema room")
    }

    pub fn screen_centre(self) -> Vec3 {
        Vec3::from_array(self.asset().screen_center)
    }

    pub(crate) fn screen_size(self) -> Vec2 {
        let a = self.asset();
        Vec2::new(a.screen_width, a.screen_height)
    }

    /// The picture's width and height on the screen, in metres, for a
    /// picture `aspect` times as wide as it is high: as large as fits.
    pub fn picture(self, aspect: f32) -> Vec2 {
        fit(self.screen_size(), aspect)
    }

    /// The screen's curve for `amount` from 0 (flat) to 1 (as deep as the
    /// stage allows): 1 over its radius in metres.
    pub fn curvature(self, amount: f32) -> f32 {
        amount.clamp(0.0, 1.0) * self.asset().max_curvature
    }

    /// How many rows of seats it has.
    pub fn rows(self) -> u8 {
        self.asset().seats.len().min(u8::MAX as usize) as u8
    }

    /// The eyes of the middle seat in `row`, 1 being the front row.
    pub fn seat(self, row: u8) -> Vec3 {
        row_seat(self.asset().seats, row)
    }

    /// Room metres to world space: `seat`'s eyes at `viewer`'s origin,
    /// turned about the vertical so the screen's centre is straight ahead.
    pub fn pose(self, viewer: Mat4, seat: Vec3) -> Mat4 {
        room_pose(self.screen_centre(), viewer, seat)
    }
}

fn fit(screen: Vec2, aspect: f32) -> Vec2 {
    let aspect = aspect.max(1e-3);
    if aspect >= screen.x / screen.y {
        Vec2::new(screen.x, screen.x / aspect)
    } else {
        Vec2::new(screen.y * aspect, screen.y)
    }
}

/// `seats` run from the front row back; a row past either end is that end.
fn row_seat(seats: &[[f32; 3]], row: u8) -> Vec3 {
    let i = (row.max(1) as usize - 1).min(seats.len() - 1);
    Vec3::from_array(seats[i])
}

fn room_pose(centre: Vec3, viewer: Mat4, seat: Vec3) -> Mat4 {
    let to = centre - seat;
    viewer * Mat4::from_rotation_y(to.x.atan2(-to.z)) * Mat4::from_translation(-seat)
}

/// The picture as shown: unsqueezed, then turned upright. Stereo halves are
/// halves of that.
fn eye_size(stereo: StereoLayout, tex: (u32, u32), pixel_aspect: f32, rotation: u32) -> (f32, f32) {
    let pixel_aspect = stereo.eye_pixel_aspect(pixel_aspect as f64) as f32;
    let turns = rotation / 90 % 4;
    let (pw, ph) = (tex.0 as f32 * pixel_aspect.max(0.01), tex.1 as f32);
    let (pw, ph) = if turns % 2 == 1 { (ph, pw) } else { (pw, ph) };
    match stereo {
        StereoLayout::SideBySide | StereoLayout::FrameSequential => (pw / 2.0, ph),
        StereoLayout::TopBottom => (pw, ph / 2.0),
        StereoLayout::HalfSideBySide | StereoLayout::HalfTopBottom | StereoLayout::Mono => (pw, ph),
    }
}

/// Description of the uploaded frame the parameters refer to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameDesc {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub color: ColorInfo,
}

/// YUV → RGB matrix with range expansion folded in, plus offsets.
pub(crate) fn yuv_matrix(color: &ColorInfo, layout: PixelLayout) -> ([Vec4; 3], Vec4) {
    let (kr, kb) = match color.matrix {
        Matrix::Bt601 => (0.299, 0.114),
        Matrix::Bt709 => (0.2126, 0.0722),
        Matrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    let bits = match layout {
        PixelLayout::I420 { bits } => bits,
        PixelLayout::Nv12 => 8,
        PixelLayout::P010 | PixelLayout::DrmPrime | PixelLayout::Q10c => 10,
    };
    let max = ((1u32 << bits) - 1) as f32;
    // Sample scale: values are normalised by the texture format's range.
    // Q10C frames are converted into P010 planes.
    let scale = match layout {
        PixelLayout::I420 { bits: 10 } => 65535.0 / 1023.0,
        PixelLayout::P010 | PixelLayout::Q10c => 65535.0 / 65472.0,
        _ => 1.0,
    };
    let (y_off, y_mul, c_off, c_mul) = if color.full_range {
        (0.0, 1.0, (1u32 << (bits - 1)) as f32 / max, 1.0)
    } else {
        let s = (1u32 << (bits - 8)) as f32;
        (
            16.0 * s / max,
            max / (219.0 * s),
            128.0 * s / max,
            max / (224.0 * s),
        )
    };
    // rgb = M * [Y', U, V] with U,V in [-0.5, 0.5].
    let m = [
        [1.0, 0.0, 2.0 * (1.0 - kr)],
        [
            1.0,
            -2.0 * kb * (1.0 - kb) / kg,
            -2.0 * kr * (1.0 - kr) / kg,
        ],
        [1.0, 2.0 * (1.0 - kb), 0.0],
    ];
    let rows = m.map(|r| Vec4::new(r[0] * y_mul, r[1] * c_mul, r[2] * c_mul, 0.0));
    (rows, Vec4::new(y_off, c_off, c_off, scale))
}

/// Radius, metres, of the sphere a 180°/360° video is drawn on when the viewer
/// moves off its centre (height, forward). Rays from the centre ignore it.
const SPHERE_RADIUS: f32 = 3.0;

/// Maximum per-eye vergence relief in radians (~2.75° per eye, 5.5° total).
pub(crate) const MAX_COMFORT_CORR_RAD: f32 = 0.048;

/// Comfort focus's view for the eye passes. Each eye's rays start at its
/// offset from its camera, `eyes` being their offsets from the point between
/// them and `up` the head's up (room frame), and meet a sphere `distance`
/// metres round the camera; and the eyes' pictures turn apart by
/// `relief_rad` each about the head's up, so close objects ask for less
/// convergence while the cyclopean scene stays where it is. The part of
/// each offset along `line` (the line through the eyes as the aim sees it,
/// the video's frame, unit or zero) is drawn for `side_distance` instead:
/// stretched by how much nearer that is, which moves what is looked at as
/// a sphere of that radius would, along that line only.
pub(crate) fn set_comfort_focus(
    p: &mut SceneParams,
    relief_rad: f32,
    distance: f32,
    eyes: [Vec3; 2],
    up: Vec3,
    (line, side_distance): (Vec3, f32),
) {
    let relief = relief_rad.clamp(0.0, MAX_COMFORT_CORR_RAD);
    p.focus.z = 2.0 * relief;
    p.focus.w = distance.max(0.0);
    let up = up.normalize_or(Vec3::Y);
    // The left eye's camera is half the lens spacing to the left, the right's
    // to the right; the left eye turns by -relief, the right by +relief.
    for (e, side) in [-1.0f32, 1.0].into_iter().enumerate() {
        let camera = Vec3::X * (side * 0.5 * crate::focus::LENS_SPACING);
        let off = p.correction.transform_vector3(eyes[e]) - camera;
        let stretch = if side_distance > 0.0 {
            (distance / side_distance).clamp(0.0, 4.0)
        } else {
            1.0
        };
        p.eyes[e] = (off + (stretch - 1.0) * off.dot(line) * line).extend(0.0);
        p.relief[e] = Vec4::from(Quat::from_axis_angle(up, side * relief));
    }
}

pub(crate) fn build(
    video: &VideoParams,
    frame: Option<&FrameDesc>,
    inv_view_proj: [Mat4; 2],
) -> SceneParams {
    let s = &video.settings;
    let deg = |d: f32| d.to_radians();
    let (kind, h_fov, v_fov) = match video.format.projection {
        Projection::Flat => (0u32, 0.0, 0.0),
        Projection::Equirect { h_fov, v_fov } => (1, deg(h_fov), deg(v_fov)),
        Projection::Fisheye { fov } => (2, deg(fov), deg(fov)),
        Projection::Eac { h_fov } => (3, deg(h_fov), deg(180.0)),
    };
    let stereo = match video.format.stereo {
        StereoLayout::Mono => 0u32,
        StereoLayout::SideBySide | StereoLayout::HalfSideBySide | StereoLayout::FrameSequential => {
            1
        }
        StereoLayout::TopBottom | StereoLayout::HalfTopBottom => 2,
    };
    let swap = (video.format.eyes_swapped ^ s.swap_eyes) as u32;
    let (rows, offset, layout_code, tex) = match frame {
        Some(f) => {
            let (rows, off) = yuv_matrix(&f.color, f.layout);
            let lc = match f.layout {
                PixelLayout::I420 { .. } => 0u32,
                _ => 1,
            };
            (rows, off, lc, (f.width.max(1), f.height.max(1)))
        }
        None => (
            [Vec4::X, Vec4::Y, Vec4::Z],
            Vec4::new(0.0, 0.0, 0.0, 1.0),
            0,
            (1, 1),
        ),
    };
    let turns = video.rotation / 90 % 4;
    let (ew, eh) = eye_size(video.format.stereo, tex, video.pixel_aspect, video.rotation);
    // A fisheye's image circle fills the eye image's shorter side, and stays
    // round when the image is not square.
    let circle = Vec4::new((eh / ew).min(1.0), (ew / eh).min(1.0), 0.0, 0.0);
    let [c0, c1] = s.lens_centre;
    let cinema = video
        .cinema
        .filter(|cinema| cinema.room.is_available() && video.format.projection == Projection::Flat);
    let mut fill = Vec4::new(1.0, 1.0, 0.0, 0.0);
    let (screen_w, screen_h, arc) = if let Some(cinema) = cinema {
        // The whole screen, bent round the same circle as the room's: black
        // where the picture doesn't reach, like a television's bars, so the
        // room's own screen never shows round it.
        let screen = cinema.room.screen_size();
        let picture = cinema.room.picture(ew / eh.max(1e-3));
        fill = (picture / screen).extend(0.0).extend(0.0);
        (screen.x, screen.y, screen.x * cinema.curvature)
    } else {
        // The floating screen's width setting names its longer side.
        let long = s.screen_width;
        let (w, h) = if ew >= eh {
            (long, long * eh / ew.max(1e-3))
        } else {
            (long * ew / eh.max(1e-3), long)
        };
        (
            w,
            h,
            s.screen_curvature.clamp(0.0, 1.0) * std::f32::consts::TAU / 3.0,
        )
    };
    let correction =
        Mat4::from_euler(EulerRot::YXZ, deg(s.yaw), deg(s.pitch), deg(s.roll)).inverse();
    let kc = chroma(s.key_color);
    let (transfer, wide) = match frame {
        Some(f) => (
            match f.color.transfer {
                Transfer::Sdr => 0.0,
                Transfer::Pq => 1.0,
                Transfer::Hlg => 2.0,
            },
            f.color.wide_gamut as u32 as f32,
        ),
        None => (0.0, 0.0),
    };
    SceneParams {
        inv_view_proj,
        correction,
        anchor_inv: Mat4::from_quat(video.anchor.inverse()),
        screen_inv: video.screen_pose.inverse(),
        mode: [kind, stereo, swap, layout_code],
        proj: Vec4::new(h_fov, v_fov, s.zoom.max(0.05), deg(s.ipd_offset)),
        lens: Vec4::new(
            s.lens_k1,
            s.lens_k2,
            deg(s.vertical_align),
            deg(s.rotation_align),
        ),
        circle,
        centres: Vec4::new(c0[0], -c0[1], c1[0], -c1[1]),
        screen: Vec4::new(screen_w, screen_h, arc, frame.is_some() as u32 as f32),
        yuv_r: rows[0],
        yuv_g: rows[1],
        yuv_b: rows[2],
        yuv_offset: offset,
        picture: Vec4::new(s.brightness, s.contrast, s.saturation, s.gamma),
        extra: Vec4::new(s.sharpen, transfer, wide, video.background[3]),
        tex_size: Vec4::new(
            tex.0 as f32,
            tex.1 as f32,
            1.0 / tex.0 as f32,
            1.0 / tex.1 as f32,
        ),
        bg_color: if cinema.is_some() {
            Vec4::ZERO
        } else {
            Vec4::from(video.background)
        },
        key: Vec4::new(kc.x, kc.y, s.key_similarity, s.key_smoothness),
        key2: Vec4::new(
            s.key_spill,
            s.chroma_key as u32 as f32,
            video.format.alpha_pack_scale().unwrap_or(0.0),
            0.0,
        ),
        viewer: glam::vec3(0.0, s.height, -s.forward).extend(SPHERE_RADIUS),
        display: [turns, 0, 0, 0],
        hdr: hdr_levels(crate::hdr::DEFAULT_PEAK),
        tp10: Vec4::ZERO,
        tp10_uv: Vec4::ZERO,
        focus: Vec4::ZERO,
        eyes: [Vec4::ZERO; 2],
        relief: [Vec4::W; 2],
        fill,
        glow: Vec4::ZERO,
        glow_colours: [[0; 4]; crate::glow::COLOURS / 4],
    }
}

/// Chroma (BT.709 Cb, Cr) of an sRGB-encoded colour, as the shader computes it
/// from the video's samples.
pub(crate) fn chroma(rgb: [f32; 3]) -> Vec2 {
    let [r, g, b] = rgb;
    Vec2::new(
        -0.1146 * r - 0.3854 * g + 0.5 * b,
        0.5 * r - 0.4542 * g - 0.0458 * b,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows 4, 6.5 and 9 m from a screen 1.2 m above the middle row's eyes,
    /// on a 0.3 m rake.
    const CENTRE: Vec3 = Vec3::new(0.0, 1.2, -6.5);
    const SEATS: [[f32; 3]; 3] = [[0.0, -0.3, -2.5], [0.0, 0.0, 0.0], [0.0, 0.3, 2.5]];

    #[test]
    fn the_picture_fills_the_cinema_screen_one_way() {
        let picture = fit(Vec2::new(9.0, 4.0), 16.0 / 9.0);
        assert!(
            (picture.y - 4.0).abs() < 1e-5 && picture.x < 9.0,
            "{picture}"
        );
    }

    #[test]
    fn rows_count_from_the_front() {
        let z = |row| row_seat(&SEATS, row).z;
        assert_eq!(z(1), -2.5);
        assert_eq!(z(2), 0.0);
        assert_eq!(z(3), 2.5);
        assert_eq!((z(0), z(9)), (-2.5, 2.5), "past either end, that end");
    }

    #[test]
    fn the_cinema_screen_is_straight_ahead_of_any_seat() {
        let viewer = Mat4::from_rotation_translation(Quat::from_rotation_y(0.8), Vec3::Y * 1.6);
        // Off the screen's centre line, it still faces the screen's middle.
        for seat in [Vec3::ZERO, Vec3::new(1.4, 0.3, 1.0)] {
            let pose = room_pose(CENTRE, viewer, seat);
            assert!(pose.transform_point3(seat).abs_diff_eq(Vec3::Y * 1.6, 1e-5));
            let ahead = viewer
                .inverse()
                .transform_point3(pose.transform_point3(CENTRE));
            assert!(ahead.x.abs() < 1e-4 && ahead.z < 0.0, "{ahead}");
            // Level: the room's up is the viewer's.
            assert!(pose.transform_vector3(Vec3::Y).abs_diff_eq(Vec3::Y, 1e-5));
        }
        // Recentred lying back, the room lies back too.
        let lying = Mat4::from_rotation_x(1.2);
        let up = room_pose(CENTRE, lying, Vec3::ZERO).transform_vector3(Vec3::Y);
        assert!(up.abs_diff_eq(lying.transform_vector3(Vec3::Y), 1e-5));
    }

    fn apply(rows: &[Vec4; 3], off: Vec4, yuv: [f32; 3]) -> [f32; 3] {
        let v = Vec3::new(yuv[0], yuv[1], yuv[2]) * off.w - off.truncate();
        [
            rows[0].truncate().dot(v),
            rows[1].truncate().dot(v),
            rows[2].truncate().dot(v),
        ]
    }

    #[test]
    fn limited_range_bt709_white_black_red() {
        let c = ColorInfo {
            matrix: Matrix::Bt709,
            full_range: false,
            transfer: Transfer::Sdr,
            wide_gamut: false,
        };
        let (rows, off) = yuv_matrix(&c, PixelLayout::I420 { bits: 8 });
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01);
        assert!(near(
            apply(&rows, off, [16.0 / 255.0, 0.5019, 0.5019]),
            [0.0, 0.0, 0.0]
        ));
        assert!(near(
            apply(&rows, off, [235.0 / 255.0, 0.5019, 0.5019]),
            [1.0, 1.0, 1.0]
        ));
        // BT.709 limited red: Y 63, Cb 102, Cr 240.
        assert!(near(
            apply(&rows, off, [63.0 / 255.0, 102.0 / 255.0, 240.0 / 255.0]),
            [1.0, 0.0, 0.0]
        ));
    }

    #[test]
    fn ten_bit_low_bits_scale() {
        let c = ColorInfo {
            matrix: Matrix::Bt2020,
            full_range: false,
            transfer: Transfer::Pq,
            wide_gamut: true,
        };
        let (rows, off) = yuv_matrix(&c, PixelLayout::I420 { bits: 10 });
        // 10-bit white (940) stored in the low bits of a 16-bit texel.
        let s = 1.0 / 65535.0;
        let out = apply(&rows, off, [940.0 * s, 512.0 * s, 512.0 * s]);
        assert!(out.iter().all(|v| (v - 1.0).abs() < 0.01), "{out:?}");
    }

    #[test]
    fn flat_screen_aspect_ratios_full_vs_half() {
        let s = ViewSettings::default();
        let make_video = |stereo| VideoParams {
            format: VideoFormat::new(Projection::Flat, stereo),
            settings: s,
            ..Default::default()
        };
        let frame = FrameDesc {
            width: 1920,
            height: 1080,
            layout: PixelLayout::Nv12,
            color: ColorInfo {
                matrix: Matrix::Bt709,
                full_range: false,
                transfer: Transfer::Sdr,
                wide_gamut: false,
            },
        };
        let sp_fsbs = build(
            &make_video(StereoLayout::SideBySide),
            Some(&frame),
            [Mat4::IDENTITY; 2],
        );
        let sp_hsbs = build(
            &make_video(StereoLayout::HalfSideBySide),
            Some(&frame),
            [Mat4::IDENTITY; 2],
        );
        let sp_ftb = build(
            &make_video(StereoLayout::TopBottom),
            Some(&frame),
            [Mat4::IDENTITY; 2],
        );
        let sp_htb = build(
            &make_video(StereoLayout::HalfTopBottom),
            Some(&frame),
            [Mat4::IDENTITY; 2],
        );

        // Full SBS expects 960x1080 per eye (8:9), so screen is narrow/tall:
        let ar_fsbs = sp_fsbs.screen.x / sp_fsbs.screen.y;
        assert!((ar_fsbs - 960.0 / 1080.0).abs() < 1e-4);

        // Half SBS restores 16:9 widescreen per eye:
        let ar_hsbs = sp_hsbs.screen.x / sp_hsbs.screen.y;
        assert!((ar_hsbs - 1920.0 / 1080.0).abs() < 1e-4);

        // Full TB expects 1920x540 per eye (32:9), so screen is ultra-wide:
        let ar_ftb = sp_ftb.screen.x / sp_ftb.screen.y;
        assert!((ar_ftb - 1920.0 / 540.0).abs() < 1e-4);

        // Half TB restores 16:9 widescreen per eye:
        let ar_htb = sp_htb.screen.x / sp_htb.screen.y;
        assert!((ar_htb - 1920.0 / 1080.0).abs() < 1e-4);
    }

    #[test]
    fn portrait_and_anamorphic_screens() {
        let frame = FrameDesc {
            width: 1920,
            height: 1080,
            layout: PixelLayout::Nv12,
            color: ColorInfo {
                matrix: Matrix::Bt709,
                full_range: false,
                transfer: Transfer::Sdr,
                wide_gamut: false,
            },
        };
        let screen = |rotation, pixel_aspect| {
            let video = VideoParams {
                rotation,
                pixel_aspect,
                ..Default::default()
            };
            let p = build(&video, Some(&frame), [Mat4::IDENTITY; 2]);
            (p.screen.x, p.screen.y, p.display[0])
        };
        let near = |(w, h, t): (f32, f32, u32), want: (f32, f32, u32)| {
            (w - want.0).abs() < 1e-3 && (h - want.1).abs() < 1e-3 && t == want.2
        };
        // The default 6 m width: a phone clip stored sideways stands 6 m tall.
        assert!(near(screen(0, 1.0), (6.0, 3.375, 0)));
        assert!(near(screen(90, 1.0), (3.375, 6.0, 1)));
        assert!(near(screen(270, 1.0), (3.375, 6.0, 3)));
        assert!(near(screen(180, 1.0), (6.0, 3.375, 2)));
        // 4:3 pixels widen a 16:9 frame to 2.37:1.
        assert!(near(screen(0, 4.0 / 3.0), (6.0, 6.0 * 1080.0 / 2560.0, 0)));
    }

    #[test]
    fn full_side_by_side_marked_1_2_shows_eyes_at_their_shape() {
        let eye = |width, height, stereo, pixel_aspect| {
            let video = VideoParams {
                format: VideoFormat::new(Projection::Flat, stereo),
                pixel_aspect,
                ..Default::default()
            };
            let frame = FrameDesc {
                width,
                height,
                layout: PixelLayout::Nv12,
                color: ColorInfo {
                    matrix: Matrix::Bt709,
                    full_range: false,
                    transfer: Transfer::Sdr,
                    wide_gamut: false,
                },
            };
            let p = build(&video, Some(&frame), [Mat4::IDENTITY; 2]);
            p.screen.x / p.screen.y
        };
        let near = |a: f32, want: f32| (a - want).abs() < 1e-3;
        // A 2.40:1 film as full side by side, marked 1:2 so the frame shows
        // at 2.40:1 in 2D: each eye is 2.40:1, whether picked full or half.
        assert!(near(eye(3840, 800, StereoLayout::SideBySide, 0.5), 2.4));
        assert!(near(eye(3840, 800, StereoLayout::HalfSideBySide, 0.5), 2.4));
        // 2x anamorphic top/bottom is unsqueezed: a 1920x3200 clip of
        // 2.40:1 eyes.
        assert!(near(eye(1920, 3200, StereoLayout::TopBottom, 2.0), 2.4));
        // Other pixel aspects still widen each eye; mono keeps 1:2.
        assert!(near(
            eye(1920, 480, StereoLayout::SideBySide, 32.0 / 27.0),
            960.0 * 32.0 / 27.0 / 480.0
        ));
        assert!(near(eye(1920, 800, StereoLayout::Mono, 0.5), 1.2));
    }

    #[test]
    fn comfort_focus_adds_symmetric_vergence_relief_without_moving_sphere() {
        let video = VideoParams {
            format: VideoFormat::new(Projection::EQUIRECT_180, StereoLayout::SideBySide),
            ..Default::default()
        };
        let mut p = build(&video, None, [Mat4::IDENTITY; 2]);
        let viewer_before = p.viewer;
        assert_eq!(p.focus.z, 0.0);

        let eyes = [Vec3::new(-0.032, 0.0, 0.0), Vec3::new(0.032, 0.0, 0.0)];

        // Far objects (0.0 correction) leave the depth untouched.
        set_comfort_focus(&mut p, 0.0, 3.0, eyes, Vec3::Y, (Vec3::X, 3.0));
        assert_eq!(p.focus.z, 0.0);
        assert_eq!(p.focus.w, 3.0);

        // Near objects add symmetric vergence relief beside the depth setting
        // (`proj.w`), which the measurement keeps reading unchanged, and leave
        // `viewer` (height/forward sphere) as it was.
        set_comfort_focus(&mut p, 0.025, 0.4, eyes, Vec3::Y * 2.0, (Vec3::X, 0.4));
        assert!((p.focus.z - 0.050).abs() < 1e-6);
        assert_eq!(p.proj.w, 0.0);
        assert_eq!(p.viewer, viewer_before);
        // Each eye's offset from its camera: 0.5 mm wider than the lenses.
        assert!((p.eyes[1].x - 0.0005).abs() < 1e-6, "{:?}", p.eyes[1]);
        assert!((p.eyes[0].x + 0.0005).abs() < 1e-6, "{:?}", p.eyes[0]);
        // The part of it along the line through the eyes drawn for a
        // distance of its own, a fifth farther: a fifth less of it, and the
        // rest as it is.
        let tilted = [Vec3::new(-0.030, -0.010, 0.0), Vec3::new(0.030, 0.010, 0.0)];
        let line = (tilted[1] - tilted[0]).normalize();
        set_comfort_focus(&mut p, 0.0, 0.4, tilted, Vec3::Y, (line, 0.4));
        let whole = p.eyes[1].truncate();
        set_comfort_focus(&mut p, 0.0, 0.4, tilted, Vec3::Y, (line, 0.48));
        let split = p.eyes[1].truncate();
        assert!((split.dot(line) - whole.dot(line) / 1.2).abs() < 1e-6);
        let off = |v: Vec3| v - v.dot(line) * line;
        assert!(
            off(split).abs_diff_eq(off(whole), 1e-7),
            "{split} vs {whole}"
        );
        set_comfort_focus(&mut p, 0.025, 0.4, eyes, Vec3::Y * 2.0, (Vec3::X, 0.4));
        // Turned apart about the head's up, the right eye's way positive.
        let right = Quat::from_vec4(p.relief[1]) * Vec3::NEG_Z;
        assert!((right.x + 0.025f32.sin()).abs() < 1e-5, "{right}");
    }
}
