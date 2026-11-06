// the site's crt, five fullscreen draws:
//   fs_bright   luma709 of the input through smoothstep(thr, thr + 0.2)
//   fs_main     barrel uv' = uv + cc (1 + d) d with cc = uv - 0.5 and
//               d = dot(cc, cc) distortion; red and blue read at
//               uv' + convergence * strength; times the brightness boost
//               and the subpixel mask; plus a 32-tap radial glow; then
//               pow(c, 1 / 2.2)
//   fs_blur_h   13-tap gaussian on the bright pass, step bloom_radius px
//   fs_blur_v   the vertical half of the same blur
//   fs_combine  main + bloom through the blend mode
// the input is decoded with pow(c, 2.2) before the bright and main
// passes so the glow, mask and bloom add up in linear light and the
// final pow(c, 1 / 2.2) hands the base tones back unchanged; without
// that the glow lifts black to a quarter grey.
// the mask reads the fragment's pixel position so it is fixed to the
// output grid: monitor is a shadow mask of round dots in columns of
// pitch px, odd columns shifted 1.5 pitch down, r g b stacked down each
// column; lcd is three vertical rects per triad, each of aspect 0.31;
// tv is the lcd layout with every other triad shifted half a cell.

struct Params {
    kind: u32,
    distortion: f32,
    dot_scale: f32,
    dot_pitch: f32,
    falloff: f32,
    glow_radius: f32,
    glow_intensity: f32,
    blend: u32,
    bloom_threshold: f32,
    bloom_intensity: f32,
    bloom_radius: f32,
    brightness_boost: f32,
    red_conv: vec2<f32>,
    blue_conv: vec2<f32>,
    conv_strength: f32,
    aspect: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var bloom: texture_2d<f32>;
@group(1) @binding(1) var bloom_sampler: sampler;
@group(2) @binding(0) var<uniform> params: Params;

const MONITOR: u32 = 0u;
const LCD: u32 = 1u;
const TV: u32 = 2u;

const BLEND_ADD: u32 = 0u;
const BLEND_SCREEN: u32 = 1u;
const BLEND_SOFT: u32 = 2u;
const BLEND_LIGHT: u32 = 3u;
const BLEND_HDR: u32 = 4u;

// lcd subpixel width over height
const SUBPIXEL_ASPECT: f32 = 0.31;
const GLOW_TAPS: i32 = 32;
const GOLDEN_ANGLE: f32 = 2.3999632;
const BLUR_TAPS: i32 = 6;
const BLUR_SIGMA: f32 = 2.0;

fn sample(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(src, src_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
}

fn sample_linear(uv: vec2<f32>) -> vec3<f32> {
    return pow(sample(uv), vec3<f32>(2.2));
}

fn barrel(uv: vec2<f32>) -> vec2<f32> {
    let cc = uv - 0.5;
    let d = dot(cc, cc) * params.distortion;
    return uv + cc * (1.0 + d) * d;
}

// coverage of a disc of radius r at distance d, softened over f px
fn disc(d: f32, r: f32, f: f32) -> f32 {
    return 1.0 - smoothstep(r - f, r + f, d);
}

fn channel_mask(channel: i32, m: f32) -> vec3<f32> {
    return vec3<f32>(
        select(0.0, m, channel == 0),
        select(0.0, m, channel == 1),
        select(0.0, m, channel == 2),
    );
}

fn mask(p: vec2<f32>) -> vec3<f32> {
    let pitch = params.dot_pitch;
    if (pitch < 0.01) {
        return vec3<f32>(1.0);
    }
    let f = max(params.falloff * pitch, 1e-3);
    let col = floor(p.x / pitch);
    let odd = (i32(col) & 1) == 1;
    switch params.kind {
        case LCD, TV: {
            let h = pitch / SUBPIXEL_ASPECT;
            let triad_odd = (i32(floor(p.x / (3.0 * pitch))) & 1) == 1;
            let y = p.y + select(0.0, h * 0.5, params.kind == TV && triad_odd);
            let channel = i32(col) % 3;
            let local = vec2<f32>((fract(p.x / pitch) - 0.5) * pitch, (fract(y / h) - 0.5) * h);
            let half = vec2<f32>(pitch, h) * params.dot_scale * 0.5;
            let sd = rounded_box_sdf(local, half, 0.0);
            return channel_mask(channel, 1.0 - smoothstep(-f, f, sd));
        }
        default: {
            let y = p.y + select(0.0, 1.5 * pitch, odd);
            let row = floor(y / pitch);
            let channel = ((i32(row) % 3) + 3) % 3;
            let centre = vec2<f32>((col + 0.5) * pitch, (row + 0.5) * pitch);
            let d = distance(vec2<f32>(p.x, y), centre);
            return channel_mask(channel, disc(d, params.dot_scale * pitch * 0.5, f));
        }
    }
}

@fragment
fn fs_bright(in: VertexOut) -> @location(0) vec4<f32> {
    let c = sample_linear(in.uv);
    let w = smoothstep(params.bloom_threshold, params.bloom_threshold + 0.2, luma709(c));
    return vec4<f32>(c * w, 1.0);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let uv = barrel(in.uv);
    let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    let r = sample_linear(uv + params.red_conv * params.conv_strength).r;
    let g = sample_linear(uv).g;
    let b = sample_linear(uv + params.blue_conv * params.conv_strength).b;
    var c = vec3<f32>(r, g, b) * params.brightness_boost * mask(in.position.xy);
    // a sunflower spiral covers the glow disc evenly; the y scale keeps
    // it round on a wide frame
    var glow = vec3<f32>(0.0);
    for (var i = 0; i < GLOW_TAPS; i++) {
        let a = f32(i) * GOLDEN_ANGLE;
        let rad = sqrt((f32(i) + 0.5) / f32(GLOW_TAPS)) * params.glow_radius;
        let offset = vec2<f32>(cos(a), sin(a) * params.aspect) * rad;
        glow += sample_linear(uv + offset);
    }
    c += glow / f32(GLOW_TAPS) * params.glow_intensity;
    c = select(vec3<f32>(0.0), c, inside);
    return vec4<f32>(pow(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / 2.2)), 1.0);
}

fn blur_axis(uv: vec2<f32>, axis: vec2<f32>) -> vec3<f32> {
    let size = vec2<f32>(textureDimensions(src));
    var sum = vec3<f32>(0.0);
    var weight = 0.0;
    for (var i = -BLUR_TAPS; i <= BLUR_TAPS; i++) {
        let w = exp(-f32(i * i) / (2.0 * BLUR_SIGMA * BLUR_SIGMA));
        let offset = axis * f32(i) * params.bloom_radius / size;
        sum += sample(uv + offset) * w;
        weight += w;
    }
    return sum / weight;
}

@fragment
fn fs_blur_h(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur_axis(in.uv, vec2<f32>(1.0, 0.0)), 1.0);
}

@fragment
fn fs_blur_v(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur_axis(in.uv, vec2<f32>(0.0, 1.0)), 1.0);
}

@fragment
fn fs_combine(in: VertexOut) -> @location(0) vec4<f32> {
    let c = sample(in.uv);
    // the bloom is still linear; the main pass is already gamma encoded
    let linear = textureSampleLevel(bloom, bloom_sampler, in.uv, 0.0).rgb;
    let b = pow(linear, vec3<f32>(1.0 / 2.2)) * params.bloom_intensity;
    var out: vec3<f32>;
    switch params.blend {
        case BLEND_ADD: {
            out = c + b;
        }
        case BLEND_SOFT: {
            // w3c soft light with the bloom as the top layer
            let lo = c - (1.0 - 2.0 * b) * c * (1.0 - c);
            let d = select(sqrt(c), ((16.0 * c - 12.0) * c + 4.0) * c, c <= vec3<f32>(0.25));
            let hi = c + (2.0 * b - 1.0) * (d - c);
            out = select(hi, lo, b <= vec3<f32>(0.5));
        }
        case BLEND_LIGHT: {
            out = max(c, b);
        }
        case BLEND_HDR: {
            // reinhard on the added bloom only, so a frame without
            // bloom passes through unchanged
            out = (c + b) / (1.0 + b);
        }
        default: {
            out = 1.0 - (1.0 - c) * (1.0 - b);
        }
    }
    return vec4<f32>(clamp(out, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
