// the gpu half of the dithering effect. fs_cells box-averages every
// pixel_size block of the frame into one texel of the cell texture;
// fs_dither then reads that texture per output pixel and applies the
// bayer or random rule, in mono or against the uploaded palette. the
// floyd-steinberg pattern is serial and runs on the cpu instead.

struct Params {
    pixel_size: u32,
    // 1 bayer, 2 random; 0 (floyd-steinberg) never reaches the gpu
    pattern: u32,
    colour_mode: u32,
    colour_count: u32,
    threshold: f32,
    frame: u32,
    _pad0: u32,
    _pad1: u32,
    palette: array<vec4<f32>, 32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

const BAYER: u32 = 1u;

@fragment
fn fs_cells(in: VertexOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let ps = i32(params.pixel_size);
    let origin = vec2<i32>(floor(in.position.xy)) * ps;
    var sum = vec3<f32>(0.0);
    var n = 0.0;
    for (var y = 0; y < ps; y++) {
        for (var x = 0; x < ps; x++) {
            let p = origin + vec2<i32>(x, y);
            if (p.x < size.x && p.y < size.y) {
                sum += over_white(textureLoad(src, p, 0));
                n += 1.0;
            }
        }
    }
    return vec4<f32>(sum / max(n, 1.0), 1.0);
}

// the site's 4x4 ordered matrix, scaled to 0..15
fn bayer(cell: vec2<i32>) -> f32 {
    var m = array<f32, 16>(
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    );
    return m[(cell.y & 3) * 4 + (cell.x & 3)];
}

// the site's weighted rgb distance, squared
fn palette_dist(c: vec3<f32>, p: vec3<f32>) -> f32 {
    let d = c - p;
    return dot(d * d, vec3<f32>(0.299, 0.587, 0.114));
}

@fragment
fn fs_dither(in: VertexOut) -> @location(0) vec4<f32> {
    let cells = vec2<i32>(textureDimensions(src));
    let ps = i32(params.pixel_size);
    let cell = min(vec2<i32>(floor(in.position.xy)) / ps, cells - vec2<i32>(1));
    let c = textureLoad(src, cell, 0).rgb;
    // 0..1 ordered or random value for the cell
    var t: f32;
    if (params.pattern == BAYER) {
        t = bayer(cell) / 16.0;
    } else {
        t = hash31(vec3<f32>(f32(cell.x), f32(cell.y), f32(params.frame)));
    }
    if (params.colour_mode == 0u) {
        let v = luma(c) * 255.0;
        var on: bool;
        if (params.pattern == BAYER) {
            on = v > params.threshold / 128.0 * (t * 255.0);
        } else {
            on = v > params.threshold * t * 2.0;
        }
        return vec4<f32>(vec3<f32>(select(0.0, 1.0, on)), 1.0);
    }
    var best = 0u;
    var second = 0u;
    var best_d = 1e9;
    var second_d = 1e9;
    for (var i = 0u; i < params.colour_count; i++) {
        let d = palette_dist(c, params.palette[i].rgb);
        if (d < best_d) {
            second = best;
            second_d = best_d;
            best = i;
            best_d = d;
        } else if (d < second_d) {
            second = i;
            second_d = d;
        }
    }
    let pick = select(best, second, t >= 0.5);
    return vec4<f32>(params.palette[pick].rgb, 1.0);
}
