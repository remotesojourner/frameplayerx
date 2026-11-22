//! Renders reference images through every projection and checks that each
//! view matches the same view rendered from the equirectangular original.
//! The fisheye and EAC references were produced from that original with
//! FFmpeg's v360 filter, so an exact projection implementation matches it.
//!
//! Needs a Vulkan 1.3 device (lavapipe is enough); skipped otherwise.

use fp_core::format::{Projection, StereoLayout, VideoFormat};
use fp_render::capture::OffscreenEyes;
use fp_render::math::{Fov, projection, view};
use fp_render::{EyeView, Gpu, Renderer, VideoParams};
use glam::{EulerRot, Quat, Vec3};
use std::path::PathBuf;
use std::sync::Arc;

const SIZE: u32 = 192;

struct Rig {
    r: Renderer,
    eyes: OffscreenEyes,
}

fn rig() -> Option<Rig> {
    let gpu = match Gpu::headless() {
        Ok(g) => Arc::new(g),
        Err(e) => {
            eprintln!("skipping: no Vulkan device ({e})");
            return None;
        }
    };
    let r = Renderer::new(gpu, ash::vk::Format::R8G8B8A8_SRGB).expect("renderer");
    let eyes = OffscreenEyes::new(&r, SIZE, SIZE).expect("eyes");
    Some(Rig { r, eyes })
}

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/render-tests");
    std::fs::create_dir_all(&d).ok();
    d
}

fn load(name: &str) -> Arc<fp_media::VideoFrame> {
    let decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(data(name)).expect("fixture"),
    ));
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut buf).expect("png data");
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        other => panic!("unexpected PNG colour type {other:?}"),
    };
    Arc::new(fp_media::VideoFrame::from_rgba(info.width, info.height, &rgba).expect("frame"))
}

fn save(name: &str, rgba: &[u8]) {
    let f = std::fs::File::create(out_dir().join(name)).expect("png");
    let mut e = png::Encoder::new(f, SIZE, SIZE);
    e.set_color(png::ColorType::Rgba);
    e.write_header().unwrap().write_image_data(rgba).unwrap();
}

/// Renders eye `eye` looking at (yaw, pitch) degrees with a 90° field of view.
fn render(
    rig: &mut Rig,
    frame: &Arc<fp_media::VideoFrame>,
    params: &VideoParams,
    yaw: f32,
    pitch: f32,
    eye: usize,
) -> Vec<u8> {
    let q = Quat::from_euler(EulerRot::YXZ, yaw.to_radians(), pitch.to_radians(), 0.0);
    let ev = |x: f32| EyeView {
        view: view(Vec3::new(x, 1.6, 0.0), q),
        proj: projection(Fov::symmetric(90.0), 0.05, 100.0),
    };
    let eyes = [ev(-0.032), ev(0.032)];
    rig.r.begin_frame().unwrap();
    rig.r.set_video(Some(frame)).unwrap();
    rig.r.draw(&rig.eyes.targets(), &eyes, params, &[]).unwrap();
    rig.r.end_frame().unwrap();
    rig.eyes.read(&rig.r, eye).unwrap()
}

/// 4x4 box average, so the comparison measures geometry rather than the
/// resampling detail FFmpeg's v360 filter adds when producing references.
fn coarse(img: &[u8]) -> Vec<f64> {
    let s = SIZE as usize;
    let mut out = Vec::new();
    for by in (0..s).step_by(4) {
        for bx in (0..s).step_by(4) {
            for c in 0..3 {
                let mut sum = 0u32;
                for y in by..by + 4 {
                    for x in bx..bx + 4 {
                        sum += img[(y * s + x) * 4 + c] as u32;
                    }
                }
                out.push(sum as f64 / 16.0);
            }
        }
    }
    out
}

fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
    let (a, b) = (coarse(a), coarse(b));
    a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len() as f64
}

fn params(projection: Projection, stereo: StereoLayout) -> VideoParams {
    VideoParams {
        format: VideoFormat::new(projection, stereo),
        ..Default::default()
    }
}

/// (name, frame, params, which views the image covers, tolerance)
type Case = (
    &'static str,
    Arc<fp_media::VideoFrame>,
    VideoParams,
    Box<dyn Fn(f32, f32) -> bool>,
    f64,
);

const VIEWS: &[(f32, f32)] = &[
    (0.0, 0.0),
    (20.0, 0.0),
    (-20.0, 15.0),
    (90.0, 0.0),
    (-90.0, 0.0),
    (180.0, 0.0),
    (0.0, 60.0),
    (0.0, -60.0),
    (45.0, 30.0),
    (-135.0, -20.0),
];

#[test]
fn projections_agree_with_equirect() {
    let Some(mut rig) = rig() else { return };
    let eq = load("equirect360.png");
    let eq_p = params(Projection::EQUIRECT_360, StereoLayout::Mono);
    let cases: Vec<Case> = vec![
        (
            "eac360",
            load("eac360.png"),
            params(Projection::Eac { h_fov: 360.0 }, StereoLayout::Mono),
            Box::new(|_, _| true),
            5.5,
        ),
        // The fisheye image covers 200°: views within ~55° of forward are fully inside.
        (
            "fisheye200",
            load("fisheye200.png"),
            params(Projection::fisheye(200.0), StereoLayout::Mono),
            Box::new(|y: f32, p: f32| y.abs() <= 45.0 && p.abs() <= 30.0),
            3.0,
        ),
        // The 180° crop covers yaw ±90°: views up to 45° off-centre are
        // inside, as long as they do not look over a pole (where longitude
        // jumps to the hemisphere the crop removed).
        (
            "equirect180",
            load("equirect180.png"),
            params(Projection::EQUIRECT_180, StereoLayout::Mono),
            Box::new(|y: f32, p: f32| y.abs() <= 25.0 && p.abs() <= 25.0),
            2.0,
        ),
    ];
    let mut failures = Vec::new();
    for &(yaw, pitch) in VIEWS {
        let reference = render(&mut rig, &eq, &eq_p, yaw, pitch, 0);
        save(&format!("equirect360_{yaw}_{pitch}.png"), &reference);
        for (name, frame, p, in_range, tol) in &cases {
            if !in_range(yaw, pitch) {
                continue;
            }
            let img = render(&mut rig, frame, p, yaw, pitch, 0);
            save(&format!("{name}_{yaw}_{pitch}.png"), &img);
            let d = mean_diff(&reference, &img);
            eprintln!("{name:12} yaw {yaw:6} pitch {pitch:5}: mean diff {d:.2}");
            if d > *tol {
                failures.push(format!(
                    "{name} at yaw {yaw} pitch {pitch}: mean diff {d:.2} > {tol}"
                ));
            }
        }
    }
    rig.eyes.destroy(&rig.r);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn looking_around_finds_the_coloured_markers() {
    let Some(mut rig) = rig() else { return };
    let eq = load("equirect360.png");
    let p = params(Projection::EQUIRECT_360, StereoLayout::Mono);
    let centre = |img: &[u8]| {
        let i = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    };
    // Forward green, left (turning +90° about up) blue, right yellow, up magenta-ish region.
    let fwd = centre(&render(&mut rig, &eq, &p, 0.0, 0.0, 0));
    let left = centre(&render(&mut rig, &eq, &p, 90.0, 0.0, 0));
    let right = centre(&render(&mut rig, &eq, &p, -90.0, 0.0, 0));
    rig.eyes.destroy(&rig.r);
    assert!(
        fwd[1] > 200 && fwd[0] < 120 && fwd[2] < 120,
        "forward should be green: {fwd:?}"
    );
    assert!(
        left[2] > 200 && left[0] < 120,
        "left should be blue: {left:?}"
    );
    assert!(
        right[0] > 200 && right[1] > 200 && right[2] < 120,
        "right should be yellow: {right:?}"
    );
}

/// A side-by-side 180° stereo frame: a bright textured disc 20° in radius
/// straight ahead at per-eye disparity `disc` (radians), in front of a dark
/// textured background at per-eye disparity `back`.
fn stereo_disc(disc: f32, back: f32) -> Arc<fp_media::VideoFrame> {
    use std::f32::consts::PI;
    const EYE: usize = 512;
    let noise = |a: f32, b: f32, seed: u32| {
        // ~1° cells of hashed grey.
        let (i, j) = (
            (a.to_degrees() + 400.0) as u32,
            (b.to_degrees() + 400.0) as u32,
        );
        let mut h = i.wrapping_mul(73_856_093) ^ j.wrapping_mul(19_349_663) ^ seed;
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        (h & 0xff) as f32 / 255.0
    };
    let mut rgba = vec![0u8; EYE * 2 * EYE * 4];
    for eye in 0..2 {
        // The left camera sees nearer things further right than the
        // cyclopean eye does, the right camera further left.
        let side = if eye == 0 { 1.0 } else { -1.0 };
        for y in 0..EYE {
            for x in 0..EYE {
                let lon = ((x as f32 + 0.5) / EYE as f32 - 0.5) * PI;
                let lat = (0.5 - (y as f32 + 0.5) / EYE as f32) * PI;
                let dl = lon - side * disc;
                let g = if (dl * dl + lat * lat).sqrt() < 20f32.to_radians() {
                    140.0 + 100.0 * noise(dl, lat, 7)
                } else {
                    20.0 + 80.0 * noise(lon - side * back, lat, 3)
                };
                let i = (y * EYE * 2 + eye * EYE + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[g as u8, g as u8, g as u8, 255]);
            }
        }
    }
    Arc::new(fp_media::VideoFrame::from_rgba((EYE * 2) as u32, EYE as u32, &rgba).expect("frame"))
}

/// Horizontal centroid (pixels from the image centre) of the bright disc.
fn disc_centroid(img: &[u8]) -> f32 {
    let s = SIZE as usize;
    let (mut sum, mut n) = (0.0f32, 0.0f32);
    for y in 0..s {
        for x in 0..s {
            if img[(y * s + x) * 4] > 120 {
                sum += x as f32 + 0.5;
                n += 1.0;
            }
        }
    }
    sum / n - s as f32 / 2.0
}

/// Both eyes' views from a head turned by `q`, its eyes 64 mm apart along
/// its own sideways axis.
fn render_both(
    rig: &mut Rig,
    frame: &Arc<fp_media::VideoFrame>,
    params: &VideoParams,
    q: Quat,
) -> [Vec<u8>; 2] {
    let ev = |x: f32| EyeView {
        view: view(Vec3::new(0.0, 1.6, 0.0) + q * Vec3::new(x, 0.0, 0.0), q),
        proj: projection(Fov::symmetric(90.0), 0.05, 100.0),
    };
    let eyes = [ev(-0.032), ev(0.032)];
    rig.r.begin_frame().unwrap();
    rig.r.set_video(Some(frame)).unwrap();
    rig.r.draw(&rig.eyes.targets(), &eyes, params, &[]).unwrap();
    rig.r.end_frame().unwrap();
    [0, 1].map(|eye| rig.eyes.read(&rig.r, eye).unwrap())
}

/// Where (pixels across, down) the bright disc's middle is in an eye's view.
fn disc_centre(img: &[u8]) -> (f32, f32) {
    let s = SIZE as usize;
    let (mut sx, mut sy, mut n) = (0.0f32, 0.0f32, 0.0f32);
    for y in 0..s {
        for x in 0..s {
            if img[(y * s + x) * 4] > 120 {
                sx += x as f32 + 0.5;
                sy += y as f32 + 0.5;
                n += 1.0;
            }
        }
    }
    (sx / n, sy / n)
}

/// Draws frames until comfort focus has had 2.5 s to settle, and returns the
/// last. Its tracker takes at most 0.1 s from any one frame, so a stall (a
/// shader compiled on first use, a busy machine) must not use up the wait.
fn settle<T>(mut frame: impl FnMut() -> T) -> T {
    let mut last = std::time::Instant::now();
    let mut settled = 0.0;
    loop {
        let out = frame();
        let now = std::time::Instant::now();
        settled += (now - last).as_secs_f32().min(0.1);
        last = now;
        if settled >= 2.5 {
            return out;
        }
    }
}

#[test]
fn comfort_focus_keeps_what_is_looked_at_in_one_piece_with_the_head_on_its_side() {
    let Some(mut rig) = rig() else { return };
    let k = |d: f32| (0.0325 / d).atan();
    let frame = stereo_disc(k(0.4), k(3.0));
    // Lying on the right side: the left eye above the right.
    let side = Quat::from_rotation_z(-90f32.to_radians());
    rig.r.set_gaze(None);
    rig.r.set_foveation(fp_render::Foveation::Off);
    let mut p = params(Projection::EQUIRECT_180, StereoLayout::SideBySide);
    // How far apart the eyes see the disc's middle, across and up/down
    // their own views: a real thing is only ever apart across them.
    let apart = |views: &[Vec<u8>; 2]| {
        let (l, r) = (disc_centre(&views[0]), disc_centre(&views[1]));
        (l.0 - r.0, l.1 - r.1)
    };
    let off = apart(&render_both(&mut rig, &frame, &p, side));
    assert_eq!(rig.r.focus_line(), None, "nothing to log without it");
    p.settings.comfort_focus = true;
    let on = apart(&settle(|| render_both(&mut rig, &frame, &p, side)));
    let line = rig.r.focus_line().expect("a line for the log");
    rig.eyes.destroy(&rig.r);

    // 0.4 m away is 9.3° of disparity: ~20 pixels, up/down the eyes' views
    // when the video is drawn as filmed.
    eprintln!("disc apart (across, up/down): off {off:?}, on {on:?} px");
    // The log's line says it read the disc and settled on it: 0.39 m, as
    // the disc's 65 mm baseline reads with the 63 mm assumed.
    eprintln!("{line}");
    assert!(line.starts_with("comfort focus: "), "{line}");
    assert!(line.contains(" read back, 100% gave a distance"), "{line}");
    assert!(line.contains("now 0.39 m, eased "), "{line}");
    assert!(
        off.1.abs() > 12.0,
        "as filmed, apart up/down by {} px",
        off.1
    );
    assert!(
        on.1.abs() < 1.5,
        "with comfort focus, still apart up/down by {} px",
        on.1
    );
    assert!(
        on.0.abs() > 5.0,
        "with comfort focus, no depth left: {} px",
        on.0
    );
}

/// How far (pixels) comfort focus moves the disc towards the centre of the
/// left eye's view once settled (~1.5 s) with the head turned `yaw` degrees
/// and the eyes on `gaze` (play space), and where it was without.
fn comfort_shift(
    rig: &mut Rig,
    frame: &Arc<fp_media::VideoFrame>,
    yaw: f32,
    gaze: Option<Vec3>,
) -> (f32, f32) {
    comfort_shift_pitched(rig, frame, yaw, 0.0, gaze)
}

/// [`comfort_shift`] with the video pitched `pitch` degrees by its Position
/// setting, and the head pitched as much to look at it.
fn comfort_shift_pitched(
    rig: &mut Rig,
    frame: &Arc<fp_media::VideoFrame>,
    yaw: f32,
    pitch: f32,
    gaze: Option<Vec3>,
) -> (f32, f32) {
    let mut p = params(Projection::EQUIRECT_180, StereoLayout::SideBySide);
    p.settings.pitch = pitch;
    rig.r.set_gaze(gaze);
    // The eyes are followed with foveated rendering off too.
    rig.r.set_foveation(fp_render::Foveation::Off);
    let off = render(rig, frame, &p, yaw, pitch, 0);
    p.settings.comfort_focus = true;
    let on = settle(|| render(rig, frame, &p, yaw, pitch, 0));
    let c_off = disc_centroid(&off);
    (c_off - disc_centroid(&on), c_off)
}

#[test]
fn comfort_focus_eases_what_is_looked_at_only_when_it_is_close() {
    let Some(mut rig) = rig() else { return };
    // Per-eye disparity of something `d` metres away (65 mm baseline).
    let k = |d: f32| (0.0325 / d).atan();
    let near = stereo_disc(k(0.4), k(3.0));
    // 35° right of ahead: the back wall, past the disc's 20° edge.
    let aside = Quat::from_rotation_y(-35f32.to_radians()) * Vec3::NEG_Z;

    // A disc 1.6 m away in a room (3 m), looked at: nothing changes.
    let (far, _) = comfort_shift(&mut rig, &stereo_disc(k(1.6), k(3.0)), 0.0, None);
    // 0.4 m away in a room, looked at: eased in full.
    let (room, at) = comfort_shift(&mut rig, &near, 0.0, None);
    // 0.4 m away against the sky: the same, as the sky isn't looked at.
    let (sky, _) = comfort_shift(&mut rig, &stereo_disc(k(0.4), 0.0), 0.0, None);
    // The head turned to the wall beside it: left alone.
    let (turned, _) = comfort_shift(&mut rig, &near, 35.0, None);
    // The head on the disc but the eyes on the wall: left alone.
    let (glance, _) = comfort_shift(&mut rig, &near, 0.0, Some(aside));
    // The video pitched 40° up and looked at there: measured in its own
    // frame, eased as much as level.
    let (pitched, _) = comfort_shift_pitched(&mut rig, &near, 0.0, 40.0, None);
    rig.eyes.destroy(&rig.r);

    // 90° across 192 pixels: ~2.1 px per degree near the centre, and 0.4 m
    // is eased by 1.7° per eye.
    eprintln!(
        "disc at {at:.2} px; shifts: far {far:.2}, room {room:.2}, sky {sky:.2}, \
         turned {turned:.2}, glance {glance:.2}, pitched {pitched:.2} px"
    );
    assert!(
        (pitched - room).abs() < 0.5,
        "pitched shift {pitched} px, room {room} px"
    );
    assert!(at > 5.0, "disc should start right of centre: {at}");
    assert!(far.abs() < 0.2, "far scene moved {far} px");
    assert!(room > 2.8 && room < 4.4, "room shift {room} px");
    assert!(
        (sky - room).abs() < 0.5,
        "sky shift {sky} px, room {room} px"
    );
    assert!(turned.abs() < 0.2, "turned to the wall, moved {turned} px");
    assert!(glance.abs() < 0.2, "looking at the wall, moved {glance} px");
}

/// A side-by-side 180° stereo frame of a real scene, ray-traced from two
/// cameras 65 mm apart: a bright textured round plate `radius` metres
/// across round `centre` (the cameras' frame, metres, the origin between
/// them), facing them, its texture as fine to the cameras at any distance,
/// in a dark textured room 3 m round. Unlike
/// [`stereo_disc`], whose two pictures are only turned apart, each camera
/// sees from where it is, so the disparity is as a camera rig films it
/// wherever the plate is.
fn stereo_plate(centre: Vec3, radius: f32) -> Arc<fp_media::VideoFrame> {
    use std::f32::consts::PI;
    const EYE: usize = 512;
    // Grey of hashed cubes `cell` metres across, at point `p`.
    let noise = |p: Vec3, cell: f32, seed: u32| {
        let c = (p / cell).floor();
        let mut h = (c.x as i32 as u32).wrapping_mul(73_856_093)
            ^ (c.y as i32 as u32).wrapping_mul(19_349_663)
            ^ (c.z as i32 as u32).wrapping_mul(83_492_791)
            ^ seed;
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        (h & 0xff) as f32 / 255.0
    };
    let mut rgba = vec![0u8; EYE * 2 * EYE * 4];
    for eye in 0..2 {
        let o = Vec3::new(if eye == 0 { -0.0325 } else { 0.0325 }, 0.0, 0.0);
        for y in 0..EYE {
            for x in 0..EYE {
                let lon = ((x as f32 + 0.5) / EYE as f32 - 0.5) * PI;
                let lat = (0.5 - (y as f32 + 0.5) / EYE as f32) * PI;
                let d = Vec3::new(lat.cos() * lon.sin(), lat.sin(), -lat.cos() * lon.cos());
                let facing = -centre.normalize();
                let hit = o + d * ((centre - o).dot(facing) / d.dot(facing));
                let g = if d.dot(facing) < 0.0 && hit.distance(centre) < radius {
                    140.0 + 100.0 * noise(hit, 0.02 * centre.length(), 7)
                } else {
                    let b = d.dot(o);
                    let t = -b + (b * b - o.length_squared() + 9.0).sqrt();
                    20.0 + 80.0 * noise(o + d * t, 0.05, 3)
                };
                let i = (y * EYE * 2 + eye * EYE + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[g as u8, g as u8, g as u8, 255]);
            }
        }
    }
    Arc::new(fp_media::VideoFrame::from_rgba((EYE * 2) as u32, EYE as u32, &rgba).expect("frame"))
}

#[test]
fn comfort_focus_keeps_a_close_thing_looked_at_off_to_the_side_in_one_piece_lying_down() {
    let Some(mut rig) = rig() else { return };
    rig.r.set_gaze(None);
    rig.r.set_foveation(fp_render::Foveation::Off);
    // Looking right and down at a plate half a metre away, lying on the
    // right side: the left eye above the right. Off to the side, the
    // cameras' baseline is seen at a slant; looking both ways at once, each
    // camera sees the plate at another height in a picture's longitude and
    // latitude. Only a reading of its real distance lines it up.
    let mut found = Vec::new();
    for (right, down) in [(50.0f32, 0.0f32), (35.0, 35.0)] {
        let look = Quat::from_euler(EulerRot::YXZ, -right.to_radians(), -down.to_radians(), 0.0);
        let frame = stereo_plate(look * Vec3::NEG_Z * 0.5, 0.1);
        let head = look * Quat::from_rotation_z(-90f32.to_radians());
        let mut p = params(Projection::EQUIRECT_180, StereoLayout::SideBySide);
        let apart = |views: &[Vec<u8>; 2]| {
            let (l, r) = (disc_centre(&views[0]), disc_centre(&views[1]));
            (l.0 - r.0, l.1 - r.1)
        };
        let off = apart(&render_both(&mut rig, &frame, &p, head));
        p.settings.comfort_focus = true;
        let on = apart(&settle(|| render_both(&mut rig, &frame, &p, head)));
        eprintln!(
            "{right}° right, {down}° down: plate apart (across, up/down) \
             off {off:?}, on {on:?} px"
        );
        found.push((right, down, off, on));
    }
    rig.eyes.destroy(&rig.r);
    for (right, down, off, on) in found {
        assert!(
            off.1.abs() > 8.0,
            "{right}° right, {down}° down: as filmed, apart up/down by {} px",
            off.1
        );
        assert!(
            on.1.abs() < 1.5,
            "{right}° right, {down}° down: with comfort focus, still apart up/down by {} px",
            on.1
        );
    }
}

#[test]
fn comfort_focus_reads_things_nearer_than_30_cm() {
    let Some(mut rig) = rig() else { return };
    rig.r.set_gaze(None);
    rig.r.set_foveation(fp_render::Foveation::Off);
    // A plate straight ahead, 20, 15 and 11 cm away, looked at lying on the
    // right side. That near, the cameras see it 18°, 24° and 33° apart: only
    // its real distance lines the eyes' pictures of it up.
    let side = Quat::from_rotation_z(-90f32.to_radians());
    let mut found = Vec::new();
    for d in [0.2f32, 0.15, 0.11] {
        let frame = stereo_plate(Vec3::NEG_Z * d, 0.2 * d);
        let mut p = params(Projection::EQUIRECT_180, StereoLayout::SideBySide);
        let apart = |views: &[Vec<u8>; 2]| {
            let (l, r) = (disc_centre(&views[0]), disc_centre(&views[1]));
            (l.0 - r.0, l.1 - r.1)
        };
        let off = apart(&render_both(&mut rig, &frame, &p, side));
        rig.r.focus_line();
        p.settings.comfort_focus = true;
        let on = apart(&settle(|| render_both(&mut rig, &frame, &p, side)));
        let line = rig.r.focus_line().expect("a line for the log");
        eprintln!("{d} m: plate apart (across, up/down) off {off:?}, on {on:?} px\n{line}");
        found.push((d, off, on, line));
    }
    rig.eyes.destroy(&rig.r);
    for (d, off, on, line) in found {
        assert!(
            off.1.abs() > 20.0,
            "{d} m: as filmed, apart up/down by {} px",
            off.1
        );
        // What is left is the plate's 65 mm baseline against the 63 mm
        // assumed, which shows the more the nearer it is.
        assert!(
            on.1.abs() < 0.08 * off.1.abs(),
            "{d} m: with comfort focus, still apart up/down by {} px of {}",
            on.1,
            off.1
        );
        // The log's line has its distance, 3% nearer for the same reason.
        let read: f32 = line
            .split("now ")
            .nth(1)
            .and_then(|rest| rest.split(" m").next())
            .and_then(|m| m.parse().ok())
            .unwrap_or_else(|| panic!("{d} m: no distance in {line}"));
        assert!(
            (read / (d * 0.063 / 0.065) - 1.0).abs() < 0.05,
            "{d} m read as {read} m"
        );
    }
}
