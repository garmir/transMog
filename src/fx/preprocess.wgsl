// the site's shared preprocessing, in its cpu order: gaussian blur,
// monochrome grain re-rolled per frame, gamma, then black and white
// point levels. the blur is separable: fs_blur_h writes the horizontal
// pass to a scratch target and fs_blur_v finishes it vertically before
// the tone steps; fs_tone skips the blur when it is zero.

struct Params {
    blur: f32,
    grain: f32,
    gamma: f32,
    black: f32,
    white: f32,
    frame: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

// three sigmas each side covers 99.7 % of the kernel; the cap bounds the
// tap count at the slider's top
const MAX_RADIUS: i32 = 30;

fn blur_axis(uv: vec2<f32>, axis: vec2<f32>) -> vec3<f32> {
    let size = vec2<f32>(textureDimensions(src));
    let sigma = max(params.blur, 1e-3);
    let radius = min(i32(ceil(sigma * 3.0)), MAX_RADIUS);
    let denom = 2.0 * sigma * sigma;
    var sum = vec3<f32>(0.0);
    var weight = 0.0;
    for (var i = -radius; i <= radius; i++) {
        let w = exp(-f32(i * i) / denom);
        let offset = axis * f32(i) / size;
        sum += textureSampleLevel(src, src_sampler, uv + offset, 0.0).rgb * w;
        weight += w;
    }
    return sum / weight;
}

fn tone(c: vec3<f32>, px: vec2<f32>) -> vec3<f32> {
    let noise = (0.5 - hash31(vec3<f32>(px, f32(params.frame)))) * params.grain;
    var v = clamp(c + vec3<f32>(noise), vec3<f32>(0.0), vec3<f32>(1.0));
    v = pow(v, vec3<f32>(params.gamma));
    let black = params.black / 255.0;
    let white = params.white / 255.0;
    v = (v - black) / max(white - black, 1.0 / 255.0);
    return clamp(v, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_blur_h(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur_axis(in.uv, vec2<f32>(1.0, 0.0)), 1.0);
}

@fragment
fn fs_blur_v(in: VertexOut) -> @location(0) vec4<f32> {
    let c = blur_axis(in.uv, vec2<f32>(0.0, 1.0));
    return vec4<f32>(tone(c, floor(in.position.xy)), 1.0);
}

@fragment
fn fs_tone(in: VertexOut) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(src, src_sampler, in.uv, 0.0).rgb;
    return vec4<f32>(tone(c, floor(in.position.xy)), 1.0);
}
