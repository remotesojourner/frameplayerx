// A cinema room's meshes with their textures, darkened as the house lights go
// down.

struct PushConstants {
    view_proj_model: mat4x4<f32>,
    lights: f32,
    // What bends with the screen bends round a circle this curved, 1 over
    // its radius, centred on the seats' side of the screen's centre.
    curvature: f32,
    screen_x: f32,
    // How much of the dimmed light stays on with the lights down, 0 to 1.
    lights_down: f32,
    // The screen's centre (height, depth) and its width and height, room
    // metres.
    screen: vec4<f32>,
};

@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var atlas: texture_2d<f32>;
// Tiling patterns about mid-grey, one a channel (cinema_mesh::Vertex).
@group(1) @binding(0) var tiling: sampler;
@group(1) @binding(1) var detail: texture_2d<f32>;
var<immediate> pc: PushConstants;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // The atlas's coordinates, then the detail texture's.
    @location(0) uv: vec4<f32>,
    // The exposure, then the pattern's strength.
    @location(1) shade: vec2<f32>,
    // The pattern, then bit 6 for plain white.
    @location(2) @interpolate(flat) pattern: u32,
};

// `packed` is cinema_mesh::Vertex's flags, strength and tile; `place` is
// where this copy of the mesh stands, room metres, and its turn about the
// vertical.
@vertex
fn vs_main(
    @location(0) local: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) packed: vec4<u32>,
    @location(3) place: vec4<f32>,
) -> VsOut {
    let c = cos(place.w);
    let s = sin(place.w);
    var pos = vec3<f32>(c * local.x + s * local.z, local.y, c * local.z - s * local.x) + place.xyz;
    let flags = packed.x;
    let k = pc.curvature;
    if ((flags & 128u) != 0u && k > 1e-5) {
        // Along the screen stays along the curve; nearer the seats, nearer
        // its centre.
        let r = 1.0 / k;
        let a = (pos.x - pc.screen_x) * k;
        let d = r - (pos.z - pc.screen.y);
        pos.x = pc.screen_x + d * sin(a);
        pos.z = pc.screen.y + r - d * cos(a);
    }
    var out: VsOut;
    out.pos = pc.view_proj_model * vec4<f32>(pos, 1.0);
    // The pattern lies across the axis the face mostly faces, in the mesh's
    // own metres.
    let axis = (flags >> 3u) & 3u;
    let across = select(select(local.xy, local.xz, axis == 1u), local.yz, axis == 0u);
    out.uv = vec4<f32>(uv, across * (100.0 / max(f32(packed.z), 1.0)));
    out.pattern = ((flags >> 1u) & 3u) | (flags & 64u);
    // With the lights down, the screen's light still reaches what is low and
    // near it. Step lights and the exit signs at the back stay lit; lights by
    // the screen go out. All of it keeps only lights_down of that.
    let sc = pc.screen;
    let spill = (1.0 - smoothstep(sc.x - 0.39 * sc.w, sc.x + 0.53 * sc.w, pos.y))
        * (1.0 - smoothstep(sc.y + 0.18 * sc.z, sc.y + 1.43 * sc.z, pos.z));
    let glow = f32(flags & 1u) * mix(pc.lights_down, 1.0, pc.lights);
    let dimmed = pc.lights_down * select(0.042 + 0.041 * spill, 0.0, (flags & 32u) != 0u);
    let exposure = max(mix(dimmed, 0.95, pc.lights), 0.95 * glow);
    out.shade = vec2<f32>(exposure, f32(packed.y) / 255.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Lettering is finer than the atlas's texels, so it is white by its shape.
    let base = select(textureSample(atlas, samp, in.uv.xy).rgb, vec3<f32>(1.0),
        (in.pattern & 64u) != 0u);
    let patterns = textureSample(detail, tiling, in.uv.zw) - vec4<f32>(0.5);
    let pick = select(vec4<f32>(0.0), vec4<f32>(1.0),
        vec4<u32>(0u, 1u, 2u, 3u) == vec4<u32>(in.pattern & 3u));
    let grain = max(1.0 + 2.0 * in.shade.y * dot(patterns, pick), 0.0);
    return vec4<f32>(base * grain * in.shade.x, 1.0);
}
