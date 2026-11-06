// ascii: the frame is a grid of cols x rows cells. each cell samples
// the source at its top-left pixel, picks the glyph whose darkness rank
// is floor((1 - L) * (n - 1)) and draws it from the atlas, black on
// white, stretched to the cell. with a border on, the outer ring of
// cells holds a box instead: one line, two lines or a thick line
// through the ring's centre.

struct Params {
    cols: u32,
    rows: u32,
    glyphs: u32,
    // 0 none, 1 single, 2 double, 3 thick
    border: u32,
    // one atlas cell in atlas pixels
    cell_w: f32,
    cell_h: f32,
    atlas_w: f32,
    _pad: f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;
@group(2) @binding(0) var atlas: texture_2d<f32>;
@group(2) @binding(1) var atlas_sampler: sampler;

const SINGLE: u32 = 1u;
const DOUBLE: u32 = 2u;
const THICK: u32 = 3u;

// the box runs through the centres of the outer cells; `d` is the
// signed distance from it in px, `cell` the smaller cell side
fn border_ink(d: f32, cell: f32) -> bool {
    switch params.border {
        case DOUBLE: {
            let t = max(cell * 0.06, 0.5);
            let gap = cell * 0.16;
            return abs(abs(d) - gap) <= t;
        }
        case THICK: {
            return abs(d) <= max(cell * 0.18, 1.0);
        }
        default: {
            return abs(d) <= max(cell * 0.06, 0.5);
        }
    }
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(src));
    let grid = vec2<f32>(f32(params.cols), f32(params.rows));
    let cell = size / grid;
    let px = floor(in.position.xy);
    let idx = floor(px / cell);
    let outer = idx.x == 0.0 || idx.y == 0.0 || idx.x == grid.x - 1.0 || idx.y == grid.y - 1.0;
    if (params.border != 0u && outer) {
        // signed distance to the rectangle through the outer cell centres
        let centre = size * 0.5;
        let half = centre - cell * 0.5;
        let d = rounded_box_sdf(px + 0.5 - centre, half, 0.0);
        let ink = border_ink(d, min(cell.x, cell.y));
        return vec4<f32>(vec3<f32>(select(1.0, 0.0, ink)), 1.0);
    }
    if (params.glyphs == 0u) {
        return vec4<f32>(1.0);
    }
    let sp = clamp(vec2<i32>(floor(idx * size / grid)), vec2<i32>(0), vec2<i32>(size) - vec2<i32>(1));
    let l = luma(over_white(textureLoad(src, sp, 0)));
    let n = f32(params.glyphs);
    let rank = min(floor((1.0 - l) * (n - 1.0)), n - 1.0);
    // half a texel inside the glyph cell so filtering never bleeds
    let f = (px + 0.5 - idx * cell) / cell;
    let fx = clamp(f.x, 0.5 / params.cell_w, 1.0 - 0.5 / params.cell_w);
    let fy = clamp(f.y, 0.5 / params.cell_h, 1.0 - 0.5 / params.cell_h);
    let uv = vec2<f32>((rank + fx) * params.cell_w / params.atlas_w, fy);
    let v = textureSampleLevel(atlas, atlas_sampler, uv, 0.0).r;
    return vec4<f32>(vec3<f32>(v), 1.0);
}
