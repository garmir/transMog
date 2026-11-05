// packs the effected rgba frame into uyvy for NDI: the target is half as
// wide as the frame and texel (x, y) holds U Y0 V Y1 for pixels 2x and
// 2x + 1, so the bytes read back in uyvy order. bt.709 limited range,
// what NDI assumes for hd. rows 0..band carry the stamp instead of the
// picture; stamp.rs has the scheme and its cpu twin.

struct Params {
    width: u32,
    band: u32,
    value: u32,
    _pad: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

fn luma(rgb: vec3<f32>) -> f32 {
    return (16.0 + 219.0 * dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722))) / 255.0;
}

fn stamp(x: u32) -> f32 {
    let cell = x * 16u / params.width;
    let on = (params.value >> (15u - cell)) & 1u;
    return select(16.0, 235.0, on == 1u) / 255.0;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let xy = vec2<u32>(in.position.xy);
    let x0 = xy.x * 2u;
    if (xy.y < params.band) {
        return vec4<f32>(128.0 / 255.0, stamp(x0), 128.0 / 255.0, stamp(x0 + 1u));
    }
    let a = textureLoad(src, vec2<u32>(x0, xy.y), 0).rgb;
    let b = textureLoad(src, vec2<u32>(min(x0 + 1u, params.width - 1u), xy.y), 0).rgb;
    let rgb = (a + b) * 0.5;
    let y = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let cb = (rgb.b - y) / 1.8556;
    let cr = (rgb.r - y) / 1.5748;
    let u = (128.0 + 224.0 * cb) / 255.0;
    let v = (128.0 + 224.0 * cr) / 255.0;
    return vec4<f32>(u, luma(a), v, luma(b));
}
