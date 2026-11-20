// Comfort focus's matching (src/focus.rs): the strips fs_focus in
// scene.wgsl sampled round the aim, matched for each patch of the 3 x 3
// cluster by BANDS workgroups of 64 threads, one thread per candidate
// disparity: each workgroup takes the next 64 candidates, and loads only
// the part of the strips its shifts reach.
//
// The strips lie on a lattice of angles about the cameras' baseline, whose
// rows are the lines both cameras see a point on, so a disparity is a
// shift along the rows: candidate `m` compares the left
// strip at column `i + off_l` with the right at `i + off_r`, `off_l - off_r = m`, the
// window moving apart symmetrically about the aim, and is a per-eye
// disparity of half `m` lattice steps (`kappa_at` in focus.rs). The first
// few candidates are below zero, for footage shot slightly converged. The
// two cameras are rarely mounted exactly level, so each candidate also
// tries the right strip a few rows higher and lower and keeps its best
// score. The score is a
// structural similarity, sensitive to contrast and brightness mismatches
// (unlike plain normalised correlation), so a bright patch in one eye does
// not match a shadow in the other, weighted to the middle of the window.

// Candidates a workgroup scores, and workgroups a patch: 192 candidates,
// the last a per-eye disparity of 0.327 rad (0.093 m with a 63 mm baseline).
const PER_BAND: u32 = 64u;
const BANDS: u32 = 3u;
// Candidates below zero disparity.
const NEGATIVE: i32 = 4;
// fs_focus's strips (keep in step with scene.wgsl and focus.rs): COLS
// columns for each eye, of which a band's shifts reach BAND_COLS, starting
// BAND_SHIFT further out with every band.
const COLS: u32 = 117u;
const ROWS: u32 = 19u;
const LEFT0: i32 = -12;
const RIGHT0: i32 = -103;
const BAND_COLS: u32 = 53u;
const BAND_SHIFT: i32 = 32;
// The window: 21 x 13 samples, 4.2° by 2.6°, Gaussian-weighted.
const HALF_W: i32 = 10;
const HALF_H: i32 = 6;
const INV_SX2: f32 = 1.0 / 36.0;
const INV_SY2: f32 = 1.0 / 16.0;
// Rows the right strip may be off the left: ±0.6°.
const VERTICAL: i32 = 3;
// Brightness difference (luma) at which a match is down to exp(-0.5).
const LUMA_TOL: f32 = 0.08;

@group(0) @binding(0) var<storage, read_write> scores: array<vec2<f32>>;
@group(0) @binding(1) var samples: texture_2d<f32>;

var<workgroup> left: array<f32, 1007>;
var<workgroup> right: array<f32, 1007>;

@compute @workgroup_size(64)
fn main(
    @builtin(local_invocation_index) idx: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    let cell = wg.x;
    let band = wg.y;
    // The columns this band's shifts reach: the left strip's further right
    // with every band, the right strip's further left.
    let shift = BAND_SHIFT * i32(band);
    let left0 = shift;
    let right0 = i32(COLS - BAND_COLS) - shift;
    for (var k = idx; k < BAND_COLS * ROWS; k += PER_BAND) {
        let at = vec2<i32>(i32(k % BAND_COLS), i32(cell * ROWS + k / BAND_COLS));
        left[k] = textureLoad(samples, at + vec2<i32>(left0, 0), 0).r;
        right[k] = textureLoad(samples, at + vec2<i32>(i32(COLS) + right0, 0), 0).r;
    }
    workgroupBarrier();

    // Half the shift each way, the left taking the odd step.
    let m = i32(band * PER_BAND + idx) - NEGATIVE;
    let off_r = -((m - (m & 1)) / 2);
    let off_l = m + off_r;
    let mid = i32(ROWS / 2u);

    var wx: array<f32, 21>;
    var wy: array<f32, 13>;
    for (var i = -HALF_W; i <= HALF_W; i++) {
        wx[i + HALF_W] = exp(-0.5 * f32(i * i) * INV_SX2);
    }
    for (var j = -HALF_H; j <= HALF_H; j++) {
        wy[j + HALF_H] = exp(-0.5 * f32(j * j) * INV_SY2);
    }

    var best = vec2<f32>(0.0, 0.0);
    var found = false;
    for (var v = -VERTICAL; v <= VERTICAL; v++) {
        var total_w: f32 = 0.0;
        var sw: f32 = 0.0;
        var sl: f32 = 0.0;
        var sr: f32 = 0.0;
        var sll: f32 = 0.0;
        var srr: f32 = 0.0;
        var slr: f32 = 0.0;
        for (var j = -HALF_H; j <= HALF_H; j++) {
            let lrow = u32(mid + j) * BAND_COLS;
            let rrow = u32(mid + j + v) * BAND_COLS;
            for (var i = -HALF_W; i <= HALF_W; i++) {
                let w = wx[i + HALF_W] * wy[j + HALF_H];
                total_w += w;
                let yl = left[lrow + u32(i + off_l - LEFT0 - left0)];
                let yr = right[rrow + u32(i + off_r - RIGHT0 - right0)];
                if (yl >= 0.0 && yr >= 0.0) {
                    sw += w;
                    sl += w * yl;
                    sr += w * yr;
                    sll += w * yl * yl;
                    srr += w * yr * yr;
                    slr += w * yl * yr;
                }
            }
        }
        // Too much of the window falls outside either picture.
        if (sw < total_w * 0.75) {
            continue;
        }
        let ml = sl / sw;
        let mr = sr / sw;
        let var_l = max(sll / sw - ml * ml, 0.0);
        let var_r = max(srr / sw - mr * mr, 0.0);
        let cov = slr / sw - ml * mr;
        let structure = 2.0 * cov / (var_l + var_r + 1e-6);
        let dm = (ml - mr) / LUMA_TOL;
        let score = clamp(structure, -1.0, 1.0) * exp(-0.5 * dm * dm);
        if (!found || score > best.x) {
            best = vec2<f32>(score, sqrt(min(var_l, var_r)));
            found = true;
        }
    }
    scores[(cell * BANDS + band) * PER_BAND + idx] = best;
}
