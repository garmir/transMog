// emboss: the luminance difference between a pixel and its neighbour
// one step towards the light, scaled by depth and added back to the
// pixel's own luminance. flat areas, where the difference is at or
// under the threshold, come out mid grey.

struct Params {
    depth: f32,
    threshold: f32,
    offset: vec2<i32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(src));
    let p = vec2<i32>(in.position.xy);
    let q = clamp(p + params.offset, vec2<i32>(0), dims - 1);
    let here = textureLoad(src, p, 0);
    let there = textureLoad(src, q, 0);
    // rounded to the site's byte values, which also keeps the driver's
    // float noise on two equal texels from counting as a step
    let u = round(luma(here.rgb) * 255.0);
    let d = round(luma(there.rgb) * 255.0) - u;
    var v = 128.0;
    if (abs(d) > params.threshold) {
        v = clamp(u + d * params.depth, 0.0, 255.0);
    }
    return vec4<f32>(vec3<f32>(v / 255.0), here.a);
}
