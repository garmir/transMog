// line halftone: a rotated grid of cells, each drawing a black bar of
// the full row height whose width follows the luminance at the cell
// centre. one pass: the fragment goes into grid space, finds its row
// and walks the columns whose bars can reach it.

struct Params {
    // in 0..255 units
    threshold: f32,
    // radians
    angle: f32,
    x_squares: f32,
    y_squares: f32,
    width_min: f32,
    width_max: f32,
    // |cos a| + |sin a|: the site shrinks the cells and the bar width
    // by it so the rotated grid still covers the frame
    norm: f32,
    benday: u32,
    // columns each way a bar can reach
    reach: i32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(src));
    let centre = dims * 0.5;
    let col_w = dims.x / params.x_squares / params.norm;
    let row_h = dims.y / params.y_squares / params.norm;
    let g = rotate2(in.position.xy - centre, -params.angle);
    let row = i32(floor(g.y / row_h));
    let shift = select(0.0, col_w * 0.5, params.benday != 0u && (row & 1) == 1);
    let home = i32(floor((g.x - shift) / col_w));
    let cy = (f32(row) + 0.5) * row_h;
    var cover = 0.0;
    for (var k = -params.reach; k <= params.reach; k++) {
        let cx = (f32(home + k) + 0.5) * col_w + shift;
        let canvas = rotate2(vec2<f32>(cx, cy), params.angle) + centre;
        let l = luma(textureSampleLevel(src, src_sampler, canvas / dims, 0.0).rgb) * 255.0;
        var w = params.width_min;
        if (l < params.threshold) {
            w = remap(l, 0.0, params.threshold, params.width_max, params.width_min);
        }
        w = w / params.norm;
        cover = max(cover, clamp(w * 0.5 - abs(g.x - cx) + 0.5, 0.0, 1.0));
    }
    let v = 1.0 - cover;
    return vec4<f32>(v, v, v, 1.0);
}
