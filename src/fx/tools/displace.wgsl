// the site's displace: one point every `step` px of the frame, centred
// on the origin with y up, pushed along z by remap(l, 0, 1, 0, disp)
// where l is the pixel's mean-channel luminance, and coloured by the
// pixel. each point is an instanced quad of dot_size px on screen,
// cut to a disc in the fragment stage, so its size does not change with
// depth, like a gl point. the vertex stage reads the frame itself with
// textureLoad.

struct Params {
    view_proj: mat4x4<f32>,
    size: vec2<f32>,
    step: f32,
    displacement: f32,
    dot_size: f32,
    columns: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

struct PointOut {
    @builtin(position) position: vec4<f32>,
    @location(0) colour: vec3<f32>,
    @location(1) corner: vec2<f32>,
}

@vertex
fn vs_points(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> PointOut {
    let col = ii % params.columns;
    let row = ii / params.columns;
    let step = i32(params.step);
    let limit = vec2<i32>(params.size) - 1;
    let px = clamp(vec2<i32>(i32(col) * step, i32(row) * step), vec2<i32>(0), limit);
    let c = textureLoad(src, px, 0).rgb;
    let world = vec3<f32>(
        f32(px.x) - params.size.x * 0.5,
        params.size.y * 0.5 - f32(px.y),
        luma(c) * params.displacement,
    );
    var clip = params.view_proj * vec4<f32>(world, 1.0);
    // two triangles: 0 1 2 and 3 4 5 over the corners of a unit square
    let corner = vec2<f32>(
        select(-1.0, 1.0, vi == 1u || vi == 3u || vi == 4u),
        select(-1.0, 1.0, vi == 2u || vi == 4u || vi == 5u),
    );
    // a dot_size px sprite spans dot_size / size ndc either side of the
    // centre, scaled by w so it stays that size after the divide
    clip.x += corner.x * params.dot_size / params.size.x * clip.w;
    clip.y += corner.y * params.dot_size / params.size.y * clip.w;
    var out: PointOut;
    out.position = clip;
    out.colour = c;
    out.corner = corner;
    return out;
}

@fragment
fn fs_points(in: PointOut) -> @location(0) vec4<f32> {
    if (dot(in.corner, in.corner) > 1.0) {
        discard;
    }
    return vec4<f32>(in.colour, 1.0);
}
