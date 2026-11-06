// edge squares in two passes. fs_cells runs once per grid cell into a
// small cell map: the sobel magnitude at the cell's top-left pixel
// decides whether a square is drawn and how big. fs_draw runs per
// output pixel and unions the squares of the cells up and to the left
// that can reach it, since a square may be far larger than its cell.

struct Params {
    // in 0..255 units like the site's sobel magnitude
    threshold: f32,
    size_min: f32,
    size_max: f32,
    radius: f32,
    step: f32,
    // how many cells up and left a square can reach, ceil(size / step)
    reach: i32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

// the cell map stores size / SIZE_SCALE in its red channel; the slider
// tops out at 40 so 64 keeps every size below 1
const SIZE_SCALE: f32 = 64.0;

fn luma_at(p: vec2<i32>, dims: vec2<i32>) -> f32 {
    let q = clamp(p, vec2<i32>(0), dims - 1);
    return luma(textureLoad(src, q, 0).rgb) * 255.0;
}

@fragment
fn fs_cells(in: VertexOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(src));
    let cell = vec2<i32>(in.position.xy);
    let p = vec2<i32>(vec2<f32>(cell) * params.step);
    let tl = luma_at(p + vec2<i32>(-1, -1), dims);
    let t = luma_at(p + vec2<i32>(0, -1), dims);
    let tr = luma_at(p + vec2<i32>(1, -1), dims);
    let l = luma_at(p + vec2<i32>(-1, 0), dims);
    let r = luma_at(p + vec2<i32>(1, 0), dims);
    let bl = luma_at(p + vec2<i32>(-1, 1), dims);
    let b = luma_at(p + vec2<i32>(0, 1), dims);
    let br = luma_at(p + vec2<i32>(1, 1), dims);
    let gx = (tr + 2.0 * r + br) - (tl + 2.0 * l + bl);
    let gy = (bl + 2.0 * b + br) - (tl + 2.0 * t + tr);
    let mag = sqrt(gx * gx + gy * gy);
    var size = 0.0;
    if (mag > params.threshold) {
        if (params.threshold >= 255.0) {
            size = params.size_max;
        } else {
            let s = remap(mag, params.threshold, 255.0, params.size_min, params.size_max);
            // p5's constrain: the upper bound wins when min > max
            size = max(min(s, params.size_max), params.size_min);
        }
    }
    return vec4<f32>(size / SIZE_SCALE, 0.0, 0.0, 1.0);
}

@fragment
fn fs_draw(in: VertexOut) -> @location(0) vec4<f32> {
    let cells = vec2<i32>(textureDimensions(src));
    let p = in.position.xy;
    let home = vec2<i32>(floor(p / params.step));
    var cover = 0.0;
    for (var j = 0; j <= params.reach; j++) {
        for (var i = 0; i <= params.reach; i++) {
            let cell = home - vec2<i32>(i, j);
            if (any(cell < vec2<i32>(0)) || any(cell >= cells)) {
                continue;
            }
            let size = textureLoad(src, cell, 0).r * SIZE_SCALE;
            if (size <= 0.0) {
                continue;
            }
            let half = size * 0.5;
            let centre = vec2<f32>(cell) * params.step + vec2<f32>(half);
            let d = rounded_box_sdf(p - centre, vec2<f32>(half), min(params.radius, half));
            cover = max(cover, clamp(0.5 - d, 0.0, 1.0));
        }
    }
    let v = 1.0 - cover;
    return vec4<f32>(v, v, v, 1.0);
}
