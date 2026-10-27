// yuyv: the source is an rgba8 texture half as wide as the frame, each
// texel holding Y0 U Y1 V, so a pixel reads texel (x / 2, y) and picks
// Y0 or Y1 by the parity of x. the decode is bt.601 limited range
// (Y 16..235, Cb and Cr 16..240). rgba8: a plain copy.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> vec3<f32> {
    let yn = (y * 255.0 - 16.0) / 219.0;
    let cbn = (cb * 255.0 - 128.0) / 224.0;
    let crn = (cr * 255.0 - 128.0) / 224.0;
    let rgb = vec3<f32>(
        yn + 1.402 * crn,
        yn - 0.344136 * cbn - 0.714136 * crn,
        yn + 1.772 * cbn,
    );
    return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_yuyv(in: VertexOut) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(in.position.xy);
    let packed = textureLoad(src, vec2<i32>(xy.x / 2, xy.y), 0);
    let y = select(packed.r, packed.b, (xy.x & 1) == 1);
    return vec4<f32>(ycbcr_to_rgb(y, packed.g, packed.a), 1.0);
}

@fragment
fn fs_copy(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(src, src_sampler, in.uv);
}
