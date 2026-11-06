// patterns: the frame is cut into square cells of min(W, H) / density
// px, each cell point-samples the source at its top-left, and a cell
// darker than the threshold is filled with the tile whose rank is
// floor(L / threshold * N) from the atlas; lighter cells stay white.
// the atlas is one row of tile_px square tiles, darkest first.

struct Params {
    threshold: f32,
    density: f32,
    tile_count: u32,
    tile_px: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;
@group(2) @binding(0) var atlas: texture_2d<f32>;
@group(2) @binding(1) var atlas_sampler: sampler;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(src));
    let cell = max(min(size.x, size.y) / params.density, 1e-3);
    let px = floor(in.position.xy);
    let origin = floor(px / cell) * cell;
    let sp = clamp(vec2<i32>(floor(origin)), vec2<i32>(0), vec2<i32>(size) - vec2<i32>(1));
    let l = luma(over_white(textureLoad(src, sp, 0))) * 255.0;
    if (l >= params.threshold || params.tile_count == 0u) {
        return vec4<f32>(1.0);
    }
    let rank = min(u32(floor(l / params.threshold * f32(params.tile_count))), params.tile_count - 1u);
    let tile = f32(params.tile_px);
    let f = clamp((px - origin) / cell, vec2<f32>(0.0), vec2<f32>(0.9999));
    let tp = vec2<i32>(i32(rank) * i32(params.tile_px) + i32(f.x * tile), i32(f.y * tile));
    let t = textureLoad(atlas, tp, 0);
    return vec4<f32>(over_white(t), 1.0);
}
