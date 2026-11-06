// maps one channel of the pixel (brightness, hue or saturation) through
// a gradient. the channel is pushed around by perlin noise, posterized
// and wrapped `reps` times before the lookup, in the site's order.

const MAX_STOPS: u32 = 8u;

struct Params {
    // rgb and the position 0..1, sorted by position
    stops: array<vec4<f32>, 8>,
    count: u32,
    posterize: f32,
    intensity: f32,
    scale: f32,
    gamma: f32,
    reps: f32,
    map: u32,
    _pad: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

const MAP_BRIGHTNESS: u32 = 0u;
const MAP_HUE: u32 = 1u;
const MAP_SATURATION: u32 = 2u;

// p5's hue, 0..1 instead of 0..360; grey is 0
fn hue(c: vec3<f32>) -> f32 {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let d = mx - mn;
    if (d < 1e-6) {
        return 0.0;
    }
    var h: f32;
    if (mx == c.r) {
        h = (c.g - c.b) / d;
    } else if (mx == c.g) {
        h = (c.b - c.r) / d + 2.0;
    } else {
        h = (c.r - c.g) / d + 4.0;
    }
    return fract(h / 6.0);
}

// p5's hsb saturation, 0..1
fn saturation(c: vec3<f32>) -> f32 {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    if (mx < 1e-6) {
        return 0.0;
    }
    return (mx - mn) / mx;
}

fn gradient_at(f: f32) -> vec3<f32> {
    let n = min(params.count, MAX_STOPS);
    if (f <= params.stops[0].w) {
        return params.stops[0].rgb;
    }
    for (var i = 1u; i < n; i++) {
        let a = params.stops[i - 1u];
        let b = params.stops[i];
        if (f <= b.w) {
            let t = (f - a.w) / max(b.w - a.w, 1e-6);
            return mix(a.rgb, b.rgb, t);
        }
    }
    return params.stops[n - 1u].rgb;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let c = over_white(textureSample(src, src_sampler, in.uv));
    var f: f32;
    switch params.map {
        case MAP_HUE: {
            f = hue(c);
        }
        case MAP_SATURATION: {
            f = saturation(c);
        }
        default: {
            f = luma(c);
        }
    }
    let px = floor(in.position.xy);
    let noise = pow(perlin2(px * params.scale), params.gamma);
    f = clamp(f + (noise - 0.5) * 2.0 * params.intensity, 0.0, 1.0);
    // the site divides by n - 1, so a full channel lands just past 1
    // and the gradient clamps it to the last stop
    f = floor(f * params.posterize) / (params.posterize - 1.0);
    if (params.reps > 1.0) {
        f = fract(f * params.reps);
    }
    return vec4<f32>(gradient_at(clamp(f, 0.0, 1.0)), 1.0);
}
