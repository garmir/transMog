// packs the effected rgba frame into uyvy for NDI at the send size: the
// target is half as wide as the sent frame and texel (x, y) holds U Y0 V
// Y1 for pixels 2x and 2x + 1, so the bytes read back in uyvy order.
// bt.709 limited range, what NDI assumes for hd. each sent pixel is the
// mean of a grid of source texels over its footprint, so a 3:1 shrink
// does not alias the way one bilinear tap would. rows 0..band carry the
// stamp instead of the picture; stamp.rs has the scheme and its cpu twin.

struct Params {
    width: u32,
    height: u32,
    band: u32,
    value: u32,
    src_width: u32,
    src_height: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

// the footprint's taps per axis are capped, so a 4k source sent at 640
// samples a 4 x 4 grid of its 6 x 6 texels
const MAX_TAPS: u32 = 4u;

fn luma(rgb: vec3<f32>) -> f32 {
    return (16.0 + 219.0 * dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722))) / 255.0;
}

fn stamp(x: u32) -> f32 {
    let cell = x * 16u / params.width;
    let on = (params.value >> (15u - cell)) & 1u;
    return select(16.0, 235.0, on == 1u) / 255.0;
}

fn taps(src_len: u32, len: u32) -> u32 {
    return clamp((src_len + len - 1u) / len, 1u, MAX_TAPS);
}

// the mean of the source texels under sent pixel (x, y)
fn pixel(x: u32, y: u32) -> vec3<f32> {
    let scale = vec2<f32>(f32(params.src_width) / f32(params.width),
                          f32(params.src_height) / f32(params.height));
    let n = vec2<u32>(taps(params.src_width, params.width), taps(params.src_height, params.height));
    let top = vec2<f32>(f32(x), f32(y)) * scale;
    let last = vec2<i32>(i32(params.src_width) - 1, i32(params.src_height) - 1);
    var sum = vec3<f32>(0.0);
    for (var j = 0u; j < n.y; j++) {
        for (var i = 0u; i < n.x; i++) {
            let at = top + (vec2<f32>(f32(i), f32(j)) + 0.5) / vec2<f32>(n) * scale;
            let texel = clamp(vec2<i32>(at), vec2<i32>(0), last);
            sum += textureLoad(src, texel, 0).rgb;
        }
    }
    return sum / f32(n.x * n.y);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let xy = vec2<u32>(in.position.xy);
    let x0 = xy.x * 2u;
    if (xy.y < params.band) {
        return vec4<f32>(128.0 / 255.0, stamp(x0), 128.0 / 255.0, stamp(x0 + 1u));
    }
    let a = pixel(x0, xy.y);
    let b = pixel(min(x0 + 1u, params.width - 1u), xy.y);
    let rgb = (a + b) * 0.5;
    let y = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let cb = (rgb.b - y) / 1.8556;
    let cr = (rgb.r - y) / 1.5748;
    let u = (128.0 + 224.0 * cb) / 255.0;
    let v = (128.0 + 224.0 * cr) / 255.0;
    return vec4<f32>(u, luma(a), v, luma(b));
}
