// a rotated grid of black rounded squares sized by the luminance under
// each one, in two passes. fs_cells runs once per grid point into a
// cell map: the perlin jitter of the point and the size the sampled
// luminance gives it. fs_draw runs per output pixel in grid space and
// unions the dots of the cells around it, since the jitter and the
// dot size both cross cell borders.

struct Params {
    // in 0..255 units
    threshold: f32,
    // radians
    angle: f32,
    size_min: f32,
    size_max: f32,
    radius: f32,
    step: f32,
    // jitter amplitude in pixels
    noise: f32,
    // |cos a| + |sin a|, the site's divisor on the dot size
    norm: f32,
    benday: u32,
    // how many cells each way a dot can reach
    reach: i32,
    // the cell map texel that holds grid index (0, 0)
    origin: vec2<i32>,
    // half the frame size, which fs_draw cannot read from its input
    canvas_centre: vec2<f32>,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

// the cell map stores size / SIZE_SCALE in blue; the slider tops out
// at 50 so 64 keeps every size below 1
const SIZE_SCALE: f32 = 64.0;

// benday rows shift by half a step
fn row_shift(row: i32) -> f32 {
    return select(0.0, params.step * 0.5, params.benday != 0u && (row & 1) == 1);
}

fn grid_point(idx: vec2<i32>) -> vec2<f32> {
    return vec2<f32>(f32(idx.x) * params.step + row_shift(idx.y), f32(idx.y) * params.step);
}

@fragment
fn fs_cells(in: VertexOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(src));
    let idx = vec2<i32>(in.position.xy) - params.origin;
    let gp = grid_point(idx);
    // the site's noise frequency, 0.5 for every noise the slider allows
    let f = 0.5 / max(1.0, params.noise / 50.0);
    let jitter = vec2<f32>(
        (value_noise(gp * f) - 0.5) * 2.0,
        (value_noise(gp * f + vec2<f32>(100.0)) - 0.5) * 2.0,
    );
    let canvas = rotate2(gp + jitter * params.noise, params.angle) + dims * 0.5;
    let l = luma(textureSampleLevel(src, src_sampler, canvas / dims, 0.0).rgb) * 255.0;
    var size = params.size_min;
    if (l < params.threshold) {
        size = remap(l, 0.0, params.threshold, params.size_max, params.size_min);
    }
    size = size / params.norm;
    // the jitter is stored as a fraction of its amplitude so zero noise
    // decodes to exactly zero
    return vec4<f32>(jitter * 0.5 + 0.5, size / SIZE_SCALE, 1.0);
}

@fragment
fn fs_draw(in: VertexOut) -> @location(0) vec4<f32> {
    let cells = vec2<i32>(textureDimensions(src));
    let g = rotate2(in.position.xy - params.canvas_centre, -params.angle);
    let home = vec2<i32>(round(g / params.step));
    var cover = 0.0;
    for (var j = -params.reach; j <= params.reach; j++) {
        for (var i = -params.reach; i <= params.reach; i++) {
            let idx = home + vec2<i32>(i, j);
            let texel = idx + params.origin;
            if (any(texel < vec2<i32>(0)) || any(texel >= cells)) {
                continue;
            }
            let cell = textureLoad(src, texel, 0);
            let size = cell.b * SIZE_SCALE;
            if (size <= 0.0) {
                continue;
            }
            let dot = grid_point(idx) + (cell.rg * 2.0 - 1.0) * params.noise;
            let half = size * 0.5;
            let d = rounded_box_sdf(g - dot, vec2<f32>(half), min(params.radius, half));
            cover = max(cover, clamp(0.5 - d, 0.0, 1.0));
        }
    }
    let v = 1.0 - cover;
    return vec4<f32>(v, v, v, 1.0);
}
