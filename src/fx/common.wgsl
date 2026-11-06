// helpers every effect shader can call; build_shader prepends this
// after the fullscreen vertex stage. names here are fixed: an effect
// shader must not redefine them.

const TAU: f32 = 6.2831853;

// the site's luminance: a plain mean of the channels, 0..1
fn luma(c: vec3<f32>) -> f32 {
    return (c.r + c.g + c.b) / 3.0;
}

fn luma709(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// the colour composited over white, as the site reads rgba canvases
fn over_white(c: vec4<f32>) -> vec3<f32> {
    return mix(vec3<f32>(1.0), c.rgb, c.a);
}

// p5's map(v, a, b, c, d) without clamping
fn remap(v: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    return c + (v - a) * (d - c) / (b - a);
}

// lowbias32 by chris wellons; the integer hash under every random
// value here, so results do not depend on float precision
fn hash_u32(x: u32) -> u32 {
    var h = x;
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    h = h * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return h;
}

fn hash_f32(h: u32) -> f32 {
    return f32(h) * (1.0 / 4294967296.0);
}

// deterministic 0..1 value for a point; integer-valued inputs such as
// pixel coordinates or lattice corners give distinct results
fn hash21(p: vec2<f32>) -> f32 {
    let x = bitcast<u32>(p.x);
    let y = bitcast<u32>(p.y);
    return hash_f32(hash_u32(x ^ hash_u32(y + 0x9e3779b9u)));
}

fn hash22(p: vec2<f32>) -> vec2<f32> {
    let x = bitcast<u32>(p.x);
    let y = bitcast<u32>(p.y);
    let h = hash_u32(x ^ hash_u32(y + 0x9e3779b9u));
    return vec2<f32>(hash_f32(h), hash_f32(hash_u32(h + 0x85ebca6bu)));
}

// a third input for anything that re-rolls per frame
fn hash31(p: vec3<f32>) -> f32 {
    let x = bitcast<u32>(p.x);
    let y = bitcast<u32>(p.y);
    let z = bitcast<u32>(p.z);
    return hash_f32(hash_u32(x ^ hash_u32(y ^ hash_u32(z + 0x9e3779b9u))));
}

fn fade(t: vec2<f32>) -> vec2<f32> {
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

// smooth 0..1 noise from hashed lattice values, one feature per unit
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fade(fract(p));
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

fn gradient(i: vec2<f32>) -> vec2<f32> {
    let a = hash21(i) * TAU;
    return vec2<f32>(cos(a), sin(a));
}

// gradient noise with unit gradients, scaled so the output covers 0..1
// and sits at 0.5 on the lattice, like p5's noise
fn perlin2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = fade(f);
    let n00 = dot(gradient(i), f);
    let n10 = dot(gradient(i + vec2<f32>(1.0, 0.0)), f - vec2<f32>(1.0, 0.0));
    let n01 = dot(gradient(i + vec2<f32>(0.0, 1.0)), f - vec2<f32>(0.0, 1.0));
    let n11 = dot(gradient(i + vec2<f32>(1.0, 1.0)), f - vec2<f32>(1.0, 1.0));
    let n = mix(mix(n00, n10, u.x), mix(n01, n11, u.x), u.y);
    return clamp(0.5 + n * 0.70710678, 0.0, 1.0);
}

fn rotate2(v: vec2<f32>, angle: f32) -> vec2<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return vec2<f32>(c * v.x - s * v.y, s * v.x + c * v.y);
}

// signed distance to a box of half size `b` with corners rounded by `r`
fn rounded_box_sdf(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}
