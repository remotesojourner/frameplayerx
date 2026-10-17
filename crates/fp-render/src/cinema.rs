//! The cinema around flat screens: the hall's size, and the light the
//! picture throws into it. The hall itself is ray traced in
//! shaders/scene.wgsl, whose constants these must match.

use crate::params::{FrameDesc, yuv_matrix};
use fp_core::format::StereoLayout;
use fp_media::VideoFrame;
use fp_media::frame::Plane;
use fp_media::info::Transfer;
use glam::{Vec3, Vec4, Vec4Swizzles};

/// The picture lights the hall as a grid of this many lights across and down.
pub(crate) const COLS: usize = 4;
pub(crate) const ROWS: usize = 3;
pub(crate) const LIGHTS: usize = COLS * ROWS;

/// Frames are averaged over a grid twice as fine each way, so that one eye
/// of a side-by-side or top-bottom frame still covers a COLS x ROWS grid.
const GRID_W: usize = COLS * 2;
const GRID_H: usize = ROWS * 2;
/// Samples per cell, each way: enough for light, cheap on 8K frames.
const TAPS: usize = 4;

/// Your eyes above the floor of your row, seated.
const EYE_HEIGHT: f32 = 1.15;
const ROW_PITCH: f32 = 1.0;
const ROWS_BEHIND: i32 = 3;
const BACK_RISE: f32 = 0.35;
const SEAT_PITCH: f32 = 0.6;
const SEAT_TOP: f32 = 0.95;
/// From your eyes to the back of your seat.
const SEAT_BACK: f32 = 0.35;

/// A frame's mean Y, U and V in each cell of the grid, scaled as the shader
/// samples them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cells([Vec3; GRID_W * GRID_H]);

impl Cells {
    pub fn sample(frame: &VideoFrame) -> Option<Cells> {
        let planes = frame.planes();
        let (luma, chroma) = (planes.first()?, planes.get(1)?);
        let read = |p: &Plane, x: u32, y: u32, c: u32| -> f32 {
            let x = x.min(p.width.saturating_sub(1));
            let y = y.min(p.height.saturating_sub(1));
            let i = y as usize * p.stride + ((x * p.components + c) * p.bytes_per_sample) as usize;
            match p.bytes_per_sample {
                1 => p.data.get(i).map_or(0.0, |&v| f32::from(v) / 255.0),
                _ => p.data.get(i..i + 2).map_or(0.0, |b| {
                    f32::from(u16::from_le_bytes([b[0], b[1]])) / 65535.0
                }),
            }
        };
        let taps = (GRID_W * TAPS, GRID_H * TAPS);
        let mut cells = [Vec3::ZERO; GRID_W * GRID_H];
        for (i, cell) in cells.iter_mut().enumerate() {
            let (cx, cy) = (i % GRID_W, i / GRID_W);
            for t in 0..TAPS * TAPS {
                let u = ((cx * TAPS + t % TAPS) as f32 + 0.5) / taps.0 as f32;
                let v = ((cy * TAPS + t / TAPS) as f32 + 0.5) / taps.1 as f32;
                let (x, y) = (
                    (u * luma.width as f32) as u32,
                    (v * luma.height as f32) as u32,
                );
                let (u, v) = match planes.get(2) {
                    Some(cr) => (read(chroma, x / 2, y / 2, 0), read(cr, x / 2, y / 2, 0)),
                    None => (read(chroma, x / 2, y / 2, 0), read(chroma, x / 2, y / 2, 1)),
                };
                *cell += Vec3::new(read(luma, x, y, 0), u, v);
            }
            *cell /= (TAPS * TAPS) as f32;
        }
        Some(Cells(cells))
    }

    /// Linear light of each region of one eye's picture, top row first.
    pub fn regions(&self, stereo: StereoLayout, desc: &FrameDesc) -> [Vec3; LIGHTS] {
        // Cells per region, across and down: an eye is half the frame.
        let (sx, sy) = if stereo.is_side_by_side() {
            (1, 2)
        } else if stereo.is_top_bottom() {
            (2, 1)
        } else {
            (2, 2)
        };
        let (rows, off) = yuv_matrix(&desc.color, desc.layout);
        std::array::from_fn(|i| {
            let (c, r) = (i % COLS, i / COLS);
            let mut yuv = Vec3::ZERO;
            for y in 0..sy {
                for x in 0..sx {
                    yuv += self.0[(r * sy + y) * GRID_W + c * sx + x];
                }
            }
            let v = yuv / (sx * sy) as f32 * off.w - off.xyz();
            let rgb = Vec3::new(
                rows[0].xyz().dot(v),
                rows[1].xyz().dot(v),
                rows[2].xyz().dot(v),
            );
            to_linear(
                rgb.clamp(Vec3::ZERO, Vec3::ONE),
                desc.color.transfer,
                desc.color.wide_gamut,
            )
        })
    }
}

/// Displayable linear light of a video's RGB, as the shader makes it.
fn to_linear(rgb: Vec3, transfer: Transfer, wide: bool) -> Vec3 {
    let gamut = |c: Vec3| {
        if wide {
            Vec3::new(
                Vec3::new(1.6605, -0.5876, -0.0728).dot(c),
                Vec3::new(-0.1246, 1.1329, -0.0083).dot(c),
                Vec3::new(-0.0182, -0.1006, 1.1187).dot(c),
            )
        } else {
            c
        }
        .max(Vec3::ZERO)
    };
    let peak = 1000.0 / 203.0;
    match transfer {
        Transfer::Sdr => rgb.powf(2.2),
        Transfer::Pq => {
            let (m1, m2) = (0.159_301_76, 78.843_75);
            let (c1, c2, c3) = (0.835_937_5, 18.851_563, 18.6875);
            let p = rgb.powf(1.0 / m2);
            let nits = ((p - c1).max(Vec3::ZERO) / (c2 - c3 * p)).powf(1.0 / m1) * 10000.0;
            tonemap(gamut(nits) / 203.0, peak)
        }
        Transfer::Hlg => {
            let (a, b, c) = (0.178_832_77, 0.284_668_92, 0.559_910_7);
            let scene = gamut(rgb.map(|x| {
                if x <= 0.5 {
                    x * x / 3.0
                } else {
                    (((x - c) / a).exp() + b) / 12.0
                }
            }));
            let ys = Vec3::new(0.2627, 0.6780, 0.0593).dot(scene).max(1e-6);
            tonemap(scene * ys.powf(0.2) * peak, peak)
        }
    }
}

fn tonemap(c: Vec3, peak: f32) -> Vec3 {
    let m = c.max_element();
    if m <= 1e-6 {
        return c;
    }
    c * ((1.0 + m / (peak * peak)) / (1.0 + m))
}

/// The floor of row `k` (0 yours, counting towards the screen) ahead of
/// you: low enough that its seat backs stay under the line from your eyes
/// to the bottom of the screen.
fn front_row_floor(k: i32, screen_h: f32, distance: f32) -> f32 {
    let z = distance + SEAT_BACK - k as f32 * ROW_PITCH - 0.07;
    (-EYE_HEIGHT).min(-screen_h / 2.0 * (distance - z) / distance - SEAT_TOP - 0.1)
}

/// The hall around a `w` x `h` metre screen curved by `arc` radians, seen
/// from `distance` metres: the shader's `env`, `room` and `seats`.
pub(crate) fn hall(w: f32, h: f32, arc: f32, distance: f32, house_lights: f32) -> [Vec4; 3] {
    // A curved screen's edges come forward, and in towards the middle.
    let (edge_x, edge_z) = if arc < 1e-3 {
        (w / 2.0, 0.0)
    } else {
        let r = w / arc;
        (r * (arc / 2.0).sin(), r * (1.0 - (arc / 2.0).cos()))
    };
    let half_width = (edge_x + 1.2).max(4.0);
    // Seats centred on yours, an aisle each side.
    let seats = ((half_width - 1.6) / SEAT_PITCH).floor().max(1.0);
    let seats_half = (seats + 0.5) * SEAT_PITCH;
    let row0 = distance + SEAT_BACK;
    let nearest = 1.2_f32.max(0.35 * h).max(edge_z + 0.8);
    let front = (((row0 + 0.07 - nearest) / ROW_PITCH).floor() as i32 - 1).max(0);
    let row_floor = if front > 0 {
        front_row_floor(front, h, distance)
    } else {
        -EYE_HEIGHT
    };
    let stage = (-h / 2.0 - 0.35).min(row_floor);
    let ceiling = (h / 2.0 + 1.0).max(-EYE_HEIGHT + 3.0 + ROWS_BEHIND as f32 * BACK_RISE);
    let back_wall = row0 + ROWS_BEHIND as f32 * ROW_PITCH + 0.07 + 0.9;
    [
        Vec4::new(1.0, house_lights, front as f32, -ROWS_BEHIND as f32),
        Vec4::new(half_width, ceiling, back_wall, stage),
        Vec4::new(-EYE_HEIGHT, row0, seats_half, -h / 2.0),
    ]
}

/// Where the picture's regions sit on the screen and which way they face
/// (xyz; w: their area), and their light.
pub(crate) fn lights(
    w: f32,
    h: f32,
    arc: f32,
    light: &[Vec3; LIGHTS],
) -> ([Vec4; LIGHTS], [Vec4; LIGHTS], [Vec4; LIGHTS]) {
    let area = w * h / LIGHTS as f32;
    let at = |i: usize| {
        let u = ((i % COLS) as f32 + 0.5) / COLS as f32 - 0.5;
        let y = (0.5 - ((i / COLS) as f32 + 0.5) / ROWS as f32) * h;
        if arc < 1e-3 {
            (Vec3::new(u * w, y, 0.0), Vec3::Z)
        } else {
            let (r, a) = (w / arc, u * arc);
            (
                Vec3::new(r * a.sin(), y, r * (1.0 - a.cos())),
                Vec3::new(-a.sin(), 0.0, a.cos()),
            )
        }
    };
    (
        std::array::from_fn(|i| at(i).0.extend(area)),
        std::array::from_fn(|i| at(i).1.extend(0.0)),
        std::array::from_fn(|i| light[i].extend(0.0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_ahead_stay_under_the_screen() {
        // The default 6 m screen 4 m away: two rows ahead, each lower.
        let [env, room, seats] = hall(6.0, 3.375, 0.0, 4.0, 0.0);
        assert_eq!(env.z, 2.0);
        let (a, b) = (
            front_row_floor(1, 3.375, 4.0),
            front_row_floor(2, 3.375, 4.0),
        );
        assert!(-EYE_HEIGHT > a && a > b, "{a} {b}");
        assert!(room.w <= b && room.w < seats.w, "the stage is lowest");
        // Close up there is no room for any.
        assert_eq!(hall(6.0, 3.375, 0.0, 1.5, 0.0)[0].z, 0.0);
    }

    #[test]
    fn walls_clear_a_curved_screen() {
        let w = 12.0;
        let arc = std::f32::consts::TAU / 3.0;
        let [_, room, _] = hall(w, w * 9.0 / 16.0, arc, 4.0, 0.0);
        let (pos, dir, _) = lights(w, w * 9.0 / 16.0, arc, &[Vec3::ONE; LIGHTS]);
        for (p, d) in pos.iter().zip(dir) {
            assert!(p.x.abs() < room.x - 1.0 && p.z >= 0.0, "{p}");
            // Facing the axis of the screen's cylinder.
            assert!(d.z > 0.0 && (d.x * p.x) <= 0.0, "{d}");
        }
    }

    #[test]
    fn regions_read_one_eye() {
        // Side by side: red left eye, blue right eye.
        let (w, h) = (64, 32);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                if i % w < w / 2 {
                    [230, 20, 20, 255]
                } else {
                    [20, 20, 230, 255]
                }
            })
            .collect();
        let frame = VideoFrame::from_rgba(w, h, &rgba).unwrap();
        let desc = FrameDesc {
            width: w,
            height: h,
            layout: frame.layout,
            color: frame.color,
        };
        let cells = Cells::sample(&frame).unwrap();
        for light in cells.regions(StereoLayout::SideBySide, &desc) {
            assert!(light.x > 0.5 && light.z < 0.05, "{light}");
        }
        let mono = cells.regions(StereoLayout::Mono, &desc);
        assert!(mono[0].x > 0.5 && mono[COLS - 1].z > 0.5);
    }
}
