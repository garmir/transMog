// the site's distort: the red channel of a map image, read at the same
// uv as the pixel, becomes a shift in pixels. s = red * 255; where s is
// above the threshold the pixel shows the source at
// p + (remap(s, 0, 255, -x, x), remap(s, 0, 255, -y, y)) clamped to the
// frame, otherwise it copies itself. both lookups are nearest.

struct Params {
    threshold: f32,
    x_shift: f32,
    y_shift: f32,
    _pad: f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var map: texture_2d<f32>;
@group(1) @binding(1) var map_sampler: sampler;
@group(2) @binding(0) var<uniform> params: Params;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let p = vec2<i32>(in.position.xy);
    let map_size = vec2<i32>(textureDimensions(map));
    let mp = clamp(vec2<i32>(in.uv * vec2<f32>(map_size)), vec2<i32>(0), map_size - 1);
    let s = textureLoad(map, mp, 0).r * 255.0;
    var q = p;
    if (s > params.threshold) {
        let shift = vec2<f32>(
            remap(s, 0.0, 255.0, -params.x_shift, params.x_shift),
            remap(s, 0.0, 255.0, -params.y_shift, params.y_shift),
        );
        q = clamp(vec2<i32>(round(vec2<f32>(p) + shift)), vec2<i32>(0), size - 1);
    }
    return vec4<f32>(textureLoad(src, q, 0).rgb, 1.0);
}
