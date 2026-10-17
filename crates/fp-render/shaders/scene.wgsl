// Per-pixel VR video projection.
//
// Every fragment builds its view ray from the eye's inverse view-projection,
// maps the ray to a video coordinate for the active projection, picks the
// eye's half of a stereo frame, converts YUV to RGB, applies HDR tone mapping
// and picture adjustments, and writes linear colour to an sRGB target.

struct Params {
    inv_view_proj: array<mat4x4<f32>, 2>,
    // Rotation applied to world rays before projection (inverse of the
    // video's yaw/pitch/roll correction).
    correction: mat4x4<f32>,
    // World -> screen-local transform for flat screens.
    screen_inv: mat4x4<f32>,
    // x: projection kind (0 flat, 1 equirect, 2 fisheye, 3 eac)
    // y: stereo (0 mono, 1 sbs, 2 tb), z: swap eyes, w: plane layout (0 I420, 1 NV12/P010)
    mode: vec4<u32>,
    // x: h_fov, y: v_fov (radians), z: zoom, w: ipd offset (radians)
    proj: vec4<f32>,
    // x: lens k1, y: lens k2, z: vertical align, w: rotation align (radians)
    lens: vec4<f32>,
    // x: screen width, y: screen height, z: curvature arc (radians), w: has video
    screen: vec4<f32>,
    // YUV -> RGB: rgb = m * (yuv * scale - offset)
    yuv_r: vec4<f32>,
    yuv_g: vec4<f32>,
    yuv_b: vec4<f32>,
    // xyz: offsets, w: sample scale (10-bit-in-16 = 65535/1023)
    yuv_offset: vec4<f32>,
    // x brightness, y contrast, z saturation, w gamma
    picture: vec4<f32>,
    // x sharpen, y transfer (0 sdr, 1 pq, 2 hlg), z wide gamut, w background alpha
    extra: vec4<f32>,
    // x width, y height, z 1/width, w 1/height (luma texels)
    tex_size: vec4<f32>,
    // Premultiplied; alpha 0 lets passthrough show through.
    bg_color: vec4<f32>,
    // Chroma key: x, y the key colour's Cb, Cr; z similarity; w smoothness
    key: vec4<f32>,
    // x spill, y on, z alpha-packed mask scale (0 off)
    key2: vec4<f32>,
    // xyz: the viewer's offset (height, forward) in world space, metres, for
    // spherical video; w: radius of the sphere it is drawn on when offset
    viewer: vec4<f32>,
    // Cinema around flat screens (see cinema()): x on, y house lights 0..1,
    // z the last row ahead of the viewer's, w the last row behind (negative)
    env: vec4<f32>,
    // x half width, y ceiling, z back wall, w front floor
    room: vec4<f32>,
    // x the viewer's row's floor, y its seat backs' z, z half the width of
    // the seats, w the screen's bottom edge
    seats: vec4<f32>,
    // The picture's mean light, for what the hall bounces back
    glow: vec4<f32>,
    // The picture as a grid of lights: centre (w: area), facing, light
    light_pos: array<vec4<f32>, 12>,
    light_dir: array<vec4<f32>, 12>,
    light_rgb: array<vec4<f32>, 12>,
};

struct PushConstants {
    eye: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var plane0: texture_2d<f32>;
@group(0) @binding(3) var plane1: texture_2d<f32>;
@group(0) @binding(4) var plane2: texture_2d<f32>;
var<immediate> pc: PushConstants;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    // One triangle covering the screen.
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - 1.0;
    var o: VsOut;
    o.pos = vec4<f32>(xy, 0.0, 1.0);
    o.ndc = xy;
    return o;
}

const PI: f32 = 3.14159265358979;
const INVALID: vec2<f32> = vec2<f32>(-1.0, -1.0);

fn rot_y(a: f32) -> mat3x3<f32> {
    let c = cos(a); let s = sin(a);
    return mat3x3<f32>(vec3<f32>(c, 0.0, -s), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(s, 0.0, c));
}
fn rot_x(a: f32) -> mat3x3<f32> {
    let c = cos(a); let s = sin(a);
    return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, c, s), vec3<f32>(0.0, -s, c));
}
fn rot_z(a: f32) -> mat3x3<f32> {
    let c = cos(a); let s = sin(a);
    return mat3x3<f32>(vec3<f32>(c, s, 0.0), vec3<f32>(-s, c, 0.0), vec3<f32>(0.0, 0.0, 1.0));
}

// Angle-preserving zoom: scales the angle from the forward axis.
fn zoom_dir(d: vec3<f32>, zoom: f32) -> vec3<f32> {
    if (abs(zoom - 1.0) < 1e-4) { return d; }
    let theta = acos(clamp(-d.z, -1.0, 1.0));
    let r = length(d.xy);
    if (r < 1e-6) { return d; }
    let t2 = min(theta / zoom, PI);
    let phi = d.xy / r;
    return vec3<f32>(phi * sin(t2), -cos(t2));
}

// The direction from the sphere's centre to where a ray from `p` (inside a
// sphere of radius `r`) along `d` meets it: seen from off the centre, near
// parts of the picture grow and slide against far ones. Both eyes use the
// same `p`, so the video's own stereo depth is kept.
fn off_centre(d: vec3<f32>, p: vec3<f32>, r: f32) -> vec3<f32> {
    if (dot(p, p) < 1e-8) { return d; }
    let q = p * min(1.0, 0.9 * r / length(p));
    let b = dot(q, d);
    let t = -b + sqrt(b * b - dot(q, q) + r * r);
    return normalize(q + t * d);
}

// Ray (forward = -Z, right = +X, up = +Y) to normalised video coordinates
// (0..1, origin top-left) for spherical projections.
fn equirect_uv(d: vec3<f32>, h_fov: f32, v_fov: f32) -> vec2<f32> {
    let lon = atan2(d.x, -d.z);
    let lat = asin(clamp(d.y, -1.0, 1.0));
    var u = 0.5 + lon / h_fov;
    let v = 0.5 - lat / v_fov;
    if (h_fov >= 2.0 * PI - 1e-3) {
        u = fract(u);
    } else if (u < 0.0 || u > 1.0) {
        return INVALID;
    }
    if (v < 0.0 || v > 1.0) { return INVALID; }
    return vec2<f32>(u, v);
}

fn fisheye_uv(d: vec3<f32>, fov: f32, k1: f32, k2: f32) -> vec2<f32> {
    let theta = acos(clamp(-d.z, -1.0, 1.0));
    var r = theta / fov; // 0.5 at the edge of the image circle
    if (r > 0.5) { return INVALID; }
    let r2 = r * r * 4.0;
    r = r * (1.0 + k1 * r2 + k2 * r2 * r2);
    let len = length(d.xy);
    var dir = vec2<f32>(0.0, 0.0);
    if (len > 1e-6) { dir = d.xy / len; }
    let uv = vec2<f32>(0.5 + r * dir.x, 0.5 - r * dir.y);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) { return INVALID; }
    return uv;
}

// YouTube equi-angular cubemap, 3x2 layout as FFmpeg's v360 "eac" output:
// top row left|front|right, bottom row down|back|up, bottom faces rotated.
fn eac_face_uv(a: f32, b: f32) -> vec2<f32> {
    // Equi-angular warp of cube-face coordinates in [-1, 1].
    return vec2<f32>(atan(a) * 4.0 / PI, atan(b) * 4.0 / PI) * 0.5 + 0.5;
}

fn eac_uv(d: vec3<f32>, h_fov: f32) -> vec2<f32> {
    let ax = abs(d.x); let ay = abs(d.y); let az = abs(d.z);
    var face: i32;
    var a: f32; var b: f32; // face-local: a right, b down
    if (az >= ax && az >= ay) {
        if (d.z < 0.0) { face = 1; a = d.x / az; b = -d.y / az; }      // front
        else { face = 4; a = -d.x / az; b = -d.y / az; }               // back
    } else if (ax >= ay) {
        if (d.x > 0.0) { face = 2; a = d.z / ax; b = -d.y / ax; }      // right
        else { face = 0; a = -d.z / ax; b = -d.y / ax; }               // left
    } else {
        if (d.y > 0.0) { face = 5; a = d.x / ay; b = d.z / ay; }       // up
        else { face = 3; a = d.x / ay; b = -d.z / ay; }                // down
    }
    var f = eac_face_uv(a, b);
    var col: f32; var row: f32;
    switch face {
        case 0: { col = 0.0; row = 0.0; }
        case 1: { col = 1.0; row = 0.0; }
        case 2: { col = 2.0; row = 0.0; }
        // Orientations verified against FFmpeg's v360 EAC output
        // (tests/projections.rs): down and up are anti-transposed, back is
        // rotated 90 degrees.
        case 3: { col = 0.0; row = 1.0; f = vec2<f32>(1.0 - f.y, 1.0 - f.x); }
        case 4: { col = 1.0; row = 1.0; f = vec2<f32>(1.0 - f.y, f.x); }
        default: { col = 2.0; row = 1.0; f = vec2<f32>(1.0 - f.y, 1.0 - f.x); }
    }
    let uv = vec2<f32>((col + f.x) / 3.0, (row + f.y) / 2.0);
    if (h_fov < 2.0 * PI - 1e-3) {
        // EAC 180: only the front half of the layout is stored (columns
        // mapped onto the full width).
        if (d.z > 0.0) { return INVALID; }
    }
    return uv;
}

fn flat_uv(origin: vec3<f32>, d: vec3<f32>) -> vec2<f32> {
    let o = (params.screen_inv * vec4<f32>(origin, 1.0)).xyz;
    let dir = normalize((params.screen_inv * vec4<f32>(d, 0.0)).xyz);
    let w = params.screen.x; let h = params.screen.y; let arc = params.screen.z;
    var p: vec3<f32>;
    var u: f32;
    if (arc < 1e-3) {
        if (dir.z >= -1e-6) { return INVALID; }
        let t = -o.z / dir.z;
        if (t <= 0.0) { return INVALID; }
        p = o + t * dir;
        u = p.x / w + 0.5;
    } else {
        // Concave cylinder of radius R centred at (0, 0, R), screen centre at origin.
        let r = w / arc;
        let oc = vec2<f32>(o.x, o.z - r);
        let dd = vec2<f32>(dir.x, dir.z);
        let qa = dot(dd, dd);
        let qb = 2.0 * dot(oc, dd);
        let qc = dot(oc, oc) - r * r;
        let disc = qb * qb - 4.0 * qa * qc;
        if (disc < 0.0) { return INVALID; }
        let t = (-qb + sqrt(disc)) / (2.0 * qa);
        if (t <= 0.0) { return INVALID; }
        p = o + t * dir;
        let phi = atan2(p.x, r - p.z);
        u = 0.5 + phi * r / w;
    }
    let v = 0.5 - p.y / h;
    if (u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0) { return INVALID; }
    return vec2<f32>(u, v);
}

fn to_eye_rect(uv: vec2<f32>, eye: u32) -> vec2<f32> {
    let slot = f32(eye ^ params.mode.z);
    switch params.mode.y {
        case 1u: { return vec2<f32>(uv.x * 0.5 + slot * 0.5, uv.y); }
        case 2u: { return vec2<f32>(uv.x, uv.y * 0.5 + slot * 0.5); }
        default: { return uv; }
    }
}

fn sample_yuv(uv: vec2<f32>) -> vec3<f32> {
    let y = textureSampleLevel(plane0, samp, uv, 0.0).r;
    var c: vec2<f32>;
    if (params.mode.w == 0u) {
        c = vec2<f32>(textureSampleLevel(plane1, samp, uv, 0.0).r, textureSampleLevel(plane2, samp, uv, 0.0).r);
    } else {
        c = textureSampleLevel(plane1, samp, uv, 0.0).rg;
    }
    let yuv = vec3<f32>(y, c) * params.yuv_offset.w - params.yuv_offset.xyz;
    return vec3<f32>(dot(params.yuv_r.xyz, yuv), dot(params.yuv_g.xyz, yuv), dot(params.yuv_b.xyz, yuv));
}

fn luma_at(uv: vec2<f32>) -> f32 {
    return textureSampleLevel(plane0, samp, uv, 0.0).r * params.yuv_offset.w;
}

fn pq_eotf(e: vec3<f32>) -> vec3<f32> {
    let m1 = 0.1593017578125; let m2 = 78.84375;
    let c1 = 0.8359375; let c2 = 18.8515625; let c3 = 18.6875;
    let p = pow(clamp(e, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / m2));
    return pow(max(p - c1, vec3<f32>(0.0)) / (c2 - c3 * p), vec3<f32>(1.0 / m1)) * 10000.0;
}

fn hlg_inverse_oetf(e: vec3<f32>) -> vec3<f32> {
    let a = 0.17883277; let b = 0.28466892; let c = 0.55991073;
    var o: vec3<f32>;
    for (var i = 0; i < 3; i++) {
        let x = e[i];
        if (x <= 0.5) { o[i] = x * x / 3.0; } else { o[i] = (exp((x - c) / a) + b) / 12.0; }
    }
    return o;
}

fn bt2020_to_709(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(vec3<f32>(1.6605, -0.5876, -0.0728), c),
        dot(vec3<f32>(-0.1246, 1.1329, -0.0083), c),
        dot(vec3<f32>(-0.0182, -0.1006, 1.1187), c));
}

// Linear light (1.0 = SDR reference white) -> displayable linear 0..1.
fn tonemap(c: vec3<f32>, peak: f32) -> vec3<f32> {
    let m = max(max(c.r, c.g), c.b);
    if (m <= 1e-6) { return c; }
    let mapped = m * (1.0 + m / (peak * peak)) / (1.0 + m);
    return c * (mapped / m);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
}

// Cinema: a hall around the flat screen, ray traced in the screen's space
// (its centre at the origin facing +Z, the viewer at (0, 0, distance),
// metres) and lit by the picture. Row k of seats counts from the viewer's
// (0) towards the screen; rows ahead step down so none hides the screen.
// The sizes match cinema.rs.
const ROW_PITCH: f32 = 1.0;
const BACK_RISE: f32 = 0.35;
const SEAT_PITCH: f32 = 0.6;
const SEAT_TOP: f32 = 0.95;
// Seat backs stand apart above this.
const NOTCH: f32 = 0.8;
const FRONT_WALL: f32 = -0.2;
// How much brighter than physical the picture lights the hall, and how
// much of its light the hall bounces back.
const SCREEN_GAIN: f32 = 7.0;
const BOUNCE: f32 = 0.6;
const WARM: vec3<f32> = vec3<f32>(1.0, 0.62, 0.32);

struct Hit {
    t: f32,
    n: vec3<f32>,
    // 0 side wall, 1 seat, 2 floor, 3 step, 4 ceiling, 5 front wall, 6 back wall
    mat: u32,
    // A step's top
    top: f32,
};

fn row_z(k: i32) -> f32 {
    return params.seats.y - f32(k) * ROW_PITCH;
}

fn row_floor(k: i32) -> f32 {
    if (k > i32(params.env.z)) { return params.room.w; }
    if (k <= 0) { return params.seats.x - f32(k) * BACK_RISE; }
    // Under the line from the viewer's eyes to the bottom of the screen.
    let d = params.seats.y - 0.35;
    return min(params.seats.x, params.seats.w * (d - row_z(k) + 0.07) / d - SEAT_TOP - 0.1);
}

// Where a ray from outside enters a box: xyz the face's normal, w the
// distance (negative when it misses).
fn box_hit(o: vec3<f32>, inv: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> vec4<f32> {
    let a = (lo - o) * inv;
    let b = (hi - o) * inv;
    let near = min(a, b);
    let far = max(a, b);
    let t = max(max(near.x, near.y), near.z);
    if (t > min(min(far.x, far.y), far.z) || t <= 0.0) { return vec4<f32>(-1.0); }
    if (t == near.x) { return vec4<f32>(-sign(inv.x), 0.0, 0.0, t); }
    if (t == near.y) { return vec4<f32>(0.0, -sign(inv.y), 0.0, t); }
    return vec4<f32>(0.0, 0.0, -sign(inv.z), t);
}

// The gaps between the tops of the seat backs.
fn seat_gap(x: f32, above: f32) -> bool {
    let edge = abs(fract(x / SEAT_PITCH + 0.5) - 0.5) * SEAT_PITCH;
    return above > NOTCH && edge > 0.5 * SEAT_PITCH - 0.02;
}

fn seat_hit(o: vec3<f32>, d: vec3<f32>, inv: vec3<f32>, k: i32) -> vec4<f32> {
    let z = row_z(k);
    let y = row_floor(k);
    let w = params.seats.z;
    var hit = vec4<f32>(-1.0);
    let back = box_hit(o, inv, vec3<f32>(-w, y, z - 0.07), vec3<f32>(w, y + SEAT_TOP, z + 0.07));
    if (back.w > 0.0) {
        let p = o + d * back.w;
        if (!seat_gap(p.x, p.y - y)) { hit = back; }
    }
    let seat = box_hit(o, inv, vec3<f32>(-w, y + 0.38, z - 0.6), vec3<f32>(w, y + 0.48, z - 0.07));
    if (seat.w > 0.0 && (hit.w < 0.0 || seat.w < hit.w)) { hit = seat; }
    return hit;
}

// The nearest seat, floor or step closer than `far`, row by row from the
// viewer's: each row stands on its own strip of floor ROW_PITCH deep.
fn trace_rows(o: vec3<f32>, d: vec3<f32>, inv: vec3<f32>, far: f32) -> Hit {
    var hit = Hit(far, vec3<f32>(0.0), 0u, 0.0);
    let front = i32(params.env.z);
    let back = i32(params.env.w);
    let stage = front + 1;
    let step = select(-1, 1, d.z < 0.0);
    var k = clamp(i32(floor((params.seats.y + 0.07 - o.z) / ROW_PITCH)), back, stage);
    for (var i = 0; i < 64; i++) {
        let edge = row_z(k) + 0.07;
        let ta = (select(edge - ROW_PITCH, -1e9, k == stage) - o.z) * inv.z;
        let tb = (select(edge, 1e9, k == back) - o.z) * inv.z;
        let t0 = max(min(ta, tb), 0.0);
        let t1 = min(max(ta, tb), hit.t);
        if (t0 >= t1) { break; }
        let y = row_floor(k);
        if (k < stage && min(o.y + d.y * t0, o.y + d.y * t1) < y + SEAT_TOP) {
            let s = seat_hit(o, d, inv, k);
            if (s.w > 0.0 && s.w < hit.t) { hit = Hit(s.w, s.xyz, 1u, 0.0); }
        }
        let tf = (y - o.y) * inv.y;
        if (d.y < 0.0 && tf >= t0 && tf < hit.t) { hit = Hit(tf, vec3<f32>(0.0, 1.0, 0.0), 2u, 0.0); }
        if (hit.t <= t1) { break; }
        let next = k + step;
        if (next < back || next > stage) { break; }
        // Into a higher row: the step up.
        let yn = row_floor(next);
        if (o.y + d.y * t1 < yn) {
            hit = Hit(t1, vec3<f32>(0.0, 0.0, -sign(d.z)), 3u, yn);
            break;
        }
        k = next;
    }
    return hit;
}

// Where the ray leaves the hall through a wall or the ceiling.
fn room_hit(o: vec3<f32>, d: vec3<f32>, inv: vec3<f32>) -> Hit {
    let tx = (select(-params.room.x, params.room.x, d.x > 0.0) - o.x) * inv.x;
    var hit = Hit(tx, vec3<f32>(-sign(d.x), 0.0, 0.0), 0u, 0.0);
    let tz = (select(FRONT_WALL, params.room.z, d.z > 0.0) - o.z) * inv.z;
    if (tz < hit.t) { hit = Hit(tz, vec3<f32>(0.0, 0.0, -sign(d.z)), select(5u, 6u, d.z > 0.0), 0.0); }
    let ty = (params.room.y - o.y) * inv.y;
    if (d.y > 0.0 && ty < hit.t) { hit = Hit(ty, vec3<f32>(0.0, -1.0, 0.0), 4u, 0.0); }
    return hit;
}

// Light from the picture falling on `p` facing `n`: each region as a small
// disc, so that it doesn't blow out what is close to it.
fn screen_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    var e = vec3<f32>(0.0);
    for (var i = 0; i < 12; i++) {
        let c = params.light_pos[i];
        let v = c.xyz - p;
        let r2 = dot(v, v);
        let l = v * inverseSqrt(r2);
        let facing = max(dot(n, l), 0.0) * max(-dot(params.light_dir[i].xyz, l), 0.0);
        e += params.light_rgb[i].rgb * (c.w * facing / (r2 + c.w / PI));
    }
    return e * SCREEN_GAIN;
}

// Seams `width` wide every `period`, darker than between them; they fade
// out where a pixel covers more than a seam, rather than shimmer.
fn seams(x: f32, period: f32, width: f32, pixel: f32) -> f32 {
    let dist = abs(fract(x / period + 0.5) - 0.5) * period;
    let line = 1.0 - smoothstep(0.0, width, dist);
    return 1.0 - 0.5 * line * (1.0 - smoothstep(width, 4.0 * width, pixel));
}

fn cinema(origin: vec3<f32>, world_dir: vec3<f32>, frag: vec2<f32>) -> vec4<f32> {
    let o = (params.screen_inv * vec4<f32>(origin, 1.0)).xyz;
    let d0 = normalize((params.screen_inv * vec4<f32>(world_dir, 0.0)).xyz);
    let d = select(d0, vec3<f32>(1e-6), abs(d0) < vec3<f32>(1e-6));
    let inv = 1.0 / d;
    var hit = room_hit(o, d, inv);
    let rows = trace_rows(o, d, inv, hit.t);
    if (rows.t < hit.t) { hit = rows; }
    let p = o + d * hit.t;
    var n = hit.n;
    // Roughly what one pixel covers there.
    let pixel = hit.t * 0.0015 / max(abs(dot(n, d)), 0.2);
    let house = params.env.y;
    let floor_y = params.seats.x;
    var albedo = vec3<f32>(0.03);
    var glow = vec3<f32>(0.0);
    switch hit.mat {
        case 1u: {
            // Red seats, darker towards the gaps between them.
            let edge = abs(fract(p.x / SEAT_PITCH + 0.5) - 0.5) * 2.0;
            albedo = vec3<f32>(0.16, 0.022, 0.03) * (1.0 - 0.45 * smoothstep(0.7, 1.0, edge));
        }
        case 2u: {
            // Carpet, in shadow under the seats.
            albedo = vec3<f32>(0.035, 0.028, 0.032);
            let k = i32(floor((params.seats.y + 0.07 - p.z) / ROW_PITCH));
            if (k >= i32(params.env.w) && k <= i32(params.env.z) && abs(p.x) < params.seats.z) {
                let z = row_z(k);
                let under = max(max(z - 0.6 - p.z, p.z - z - 0.07), 0.0);
                albedo *= 0.35 + 0.65 * smoothstep(0.0, 0.3, under);
            }
        }
        case 3u: {
            // A step, lit along the aisles.
            albedo = vec3<f32>(0.03, 0.024, 0.028);
            if (abs(abs(p.x) - params.seats.z - 0.35) < 0.07 && p.y > hit.top - 0.06) {
                glow = WARM * 0.35;
            }
        }
        case 4u: {
            // Ceiling tiles and downlights, bright with the house lights up.
            albedo = vec3<f32>(0.02) * seams(p.x, 1.2, 0.01, pixel) * seams(p.z, 1.2, 0.01, pixel);
            let cell = (fract(p.xz / 2.4) - 0.5) * 2.4;
            if (length(cell) < 0.08) { glow = WARM * mix(0.006, 1.5, house); }
        }
        case 5u: {
            // Black masking round the screen, red curtains beside it.
            albedo = vec3<f32>(0.006);
            if (abs(p.x) > 0.5 * params.screen.x + 0.3) {
                let fold = sin(p.x * 2.0 * PI / 0.32) * (1.0 - smoothstep(0.02, 0.08, pixel));
                n = normalize(vec3<f32>(0.6 * fold, 0.0, 1.0));
                albedo = vec3<f32>(0.2, 0.02, 0.03) * (0.75 + 0.25 * fold);
            }
        }
        case 6u: {
            // The projection booth's window.
            albedo = vec3<f32>(0.03);
            if (abs(p.x) < 0.6 && abs(p.y - params.room.y + 0.8) < 0.22) {
                glow = vec3<f32>(0.05, 0.05, 0.06) + params.glow.rgb * 0.15;
            }
        }
        default: {
            // Panelled walls, with sconces washing light up them.
            albedo = vec3<f32>(0.04, 0.034, 0.034) * seams(p.z, 1.5, 0.012, pixel);
            if (p.y < floor_y + 1.0) { albedo *= 0.7; }
            let sz = clamp(round((p.z - 1.5) / 3.0), 0.0, floor((params.room.z - 2.5) / 3.0)) * 3.0 + 1.5;
            let s = vec2<f32>(p.z - sz, p.y - floor_y - 2.0);
            glow = WARM * mix(0.006, 0.05, house) * exp(-dot(s, s * vec2<f32>(4.0, 2.5)));
            if (abs(s.x) < 0.07 && abs(s.y) < 0.14) { glow += WARM * mix(0.15, 0.6, house); }
        }
    }
    let e = screen_light(p, n) + params.glow.rgb * BOUNCE + vec3<f32>(mix(0.004, 0.08, house));
    var col = albedo / PI * e + glow;
    // Dithered: dark gradients band in 8 bits.
    let noise = fract(sin(dot(frag, vec2<f32>(12.9898, 78.233))) * 43758.5453) - 0.5;
    col = pow(max(pow(col, vec3<f32>(1.0 / 2.2)) + noise / 255.0, vec3<f32>(0.0)), vec3<f32>(2.2));
    return vec4<f32>(col, 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let eye = pc.eye;
    let near = params.inv_view_proj[eye] * vec4<f32>(in.ndc, 0.0, 1.0);
    let far = params.inv_view_proj[eye] * vec4<f32>(in.ndc, 1.0, 1.0);
    let origin = near.xyz / near.w;
    let world_dir = normalize(far.xyz / far.w - origin);

    if (params.screen.w < 0.5) {
        return params.bg_color;
    }

    var uv: vec2<f32>;
    let kind = params.mode.x;
    if (kind == 0u) {
        uv = flat_uv(origin, world_dir);
    } else {
        // Per-eye stereo corrections, then the global correction rotation.
        let s = select(-0.5, 0.5, eye == 1u);
        var d = rot_y(params.proj.w * s) * world_dir;
        d = rot_x(params.lens.z * s) * d;
        d = rot_z(params.lens.w * s) * d;
        d = normalize((params.correction * vec4<f32>(d, 0.0)).xyz);
        d = off_centre(d, (params.correction * vec4<f32>(params.viewer.xyz, 0.0)).xyz,
                       params.viewer.w);
        d = zoom_dir(d, params.proj.z);
        if (kind == 1u) {
            uv = equirect_uv(d, params.proj.x, params.proj.y);
        } else if (kind == 2u) {
            uv = fisheye_uv(d, params.proj.x, params.lens.x, params.lens.y);
        } else {
            uv = eac_uv(d, params.proj.x);
        }
    }
    if (uv.x < 0.0) {
        if (params.env.x > 0.5) { return cinema(origin, world_dir, in.pos.xy); }
        return params.bg_color;
    }
    let tuv = to_eye_rect(uv, eye);
    var rgb = sample_yuv(tuv);

    // Chroma key (as OBS does it): pixels whose chroma is near the key
    // colour's turn see-through, with a soft edge, and the key colour's
    // fringe is desaturated out of what stays.
    var alpha = 1.0;
    if (params.key2.y > 0.5) {
        let c = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        let cbcr = vec2<f32>(dot(c, vec3<f32>(-0.1146, -0.3854, 0.5)), dot(c, vec3<f32>(0.5, -0.4542, -0.0458)));
        let d = distance(cbcr, params.key.xy) - params.key.z;
        alpha = pow(clamp(d / max(params.key.w, 1e-4), 0.0, 1.0), 1.5);
        let spill = pow(clamp(d / max(params.key2.x, 1e-4), 0.0, 1.0), 1.5);
        rgb = mix(vec3<f32>(dot(c, vec3<f32>(0.2126, 0.7152, 0.0722))), rgb, spill);
    }

    // Alpha-packed passthrough video (`_alpha` names): each eye's mask, scaled
    // down, sits in the red channel outside the fisheye circles, centred on
    // the frame's top middle for the left half and on its corner for the
    // right half, wrapping round the frame's edges.
    if (params.key2.z > 0.0) {
        let slot = f32(eye ^ params.mode.z);
        let k = params.key2.z;
        let m = fract(vec2<f32>((1.0 - slot) * 0.5 + (uv.x - 0.5) * k * 0.5, (uv.y - 0.5) * k));
        alpha *= clamp((sample_yuv(m).r - 0.08) / 0.84, 0.0, 1.0);
    }

    // Sharpen: unsharp mask on luma.
    if (params.extra.x > 0.0) {
        let dx = vec2<f32>(params.tex_size.z, 0.0);
        let dy = vec2<f32>(0.0, params.tex_size.w);
        let yc = luma_at(tuv);
        let blur = (luma_at(tuv + dx) + luma_at(tuv - dx) + luma_at(tuv + dy) + luma_at(tuv - dy)) * 0.25;
        rgb += vec3<f32>((yc - blur) * params.extra.x * 2.0);
    }

    var lin: vec3<f32>;
    let transfer = u32(params.extra.y + 0.5);
    if (transfer == 1u) {
        var nits = pq_eotf(rgb);
        if (params.extra.z > 0.5) { nits = bt2020_to_709(nits); }
        lin = tonemap(max(nits, vec3<f32>(0.0)) / 203.0, 1000.0 / 203.0);
    } else if (transfer == 2u) {
        var scene = hlg_inverse_oetf(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
        if (params.extra.z > 0.5) { scene = bt2020_to_709(scene); }
        // HLG OOTF for a 1000-nit display, normalised to 203-nit white.
        let ys = dot(vec3<f32>(0.2627, 0.6780, 0.0593), max(scene, vec3<f32>(0.0)));
        lin = tonemap(max(scene, vec3<f32>(0.0)) * pow(max(ys, 1e-6), 0.2) * (1000.0 / 203.0), 1000.0 / 203.0);
    } else {
        lin = srgb_to_linear(rgb);
    }

    // Picture adjustments, applied in a perceptual (gamma) space.
    var g = pow(max(lin, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    g = (g - 0.5) * params.picture.y + 0.5 + params.picture.x;
    let luma = dot(g, vec3<f32>(0.2126, 0.7152, 0.0722));
    g = mix(vec3<f32>(luma), g, params.picture.z);
    g = pow(max(g, vec3<f32>(0.0)), vec3<f32>(1.0 / max(params.picture.w, 0.05)));
    let col = srgb_to_linear(clamp(g, vec3<f32>(0.0), vec3<f32>(1.0)));
    // Over the (premultiplied) background: keyed-out pixels show it.
    return vec4<f32>(col * alpha + params.bg_color.rgb * (1.0 - alpha), alpha + params.bg_color.a * (1.0 - alpha));
}
