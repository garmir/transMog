// cellular automata over a cell grid. fs_seed marks a cell alive when
// any pixel of its block is at or below the threshold. one generation
// is two passes: fs_hsum sums the live cells along each row for up to
// four half-widths into an rgba16f texture, and fs_step sums those
// vertically, which gives every rule its square neighbourhood counts
// from 2 (2 r + 1) loads instead of (2 r + 1)². the grid is a torus.
// fs_show paints live cells black on white at frame resolution. the
// state textures are r8, so only .r carries anything.

struct Params {
    threshold: f32,
    cell: u32,
    // 0 classic, 1 larger than life, 2 mncab, 3 mncc
    kind: u32,
    _pad: u32,
    // the half-width each channel of the sum texture covers; 0 unused
    radii: vec4<u32>,
    // survive lo, survive hi, birth lo, birth hi
    classic: vec4<f32>,
    // birth lo, birth hi, survive lo, survive hi
    ltl: vec4<f32>,
    // threshold 1, threshold 2
    mncab: vec4<f32>,
    mncc_lo: vec4<f32>,
    mncc_hi: vec4<f32>,
}

// the frame for fs_seed, the state for fs_hsum and fs_show, the row
// sums for fs_step
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;
// the state again, for fs_step only
@group(2) @binding(0) var cells: texture_2d<f32>;
@group(2) @binding(1) var cells_sampler: sampler;

const CLASSIC: u32 = 0u;
const LTL: u32 = 1u;
const MNCAB: u32 = 2u;
const MNCC: u32 = 3u;

@fragment
fn fs_seed(in: VertexOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let cs = i32(params.cell);
    let origin = vec2<i32>(floor(in.position.xy)) * cs;
    var alive = false;
    for (var y = 0; y < cs; y++) {
        for (var x = 0; x < cs; x++) {
            let p = origin + vec2<i32>(x, y);
            if (p.x < size.x && p.y < size.y) {
                let l = luma(over_white(textureLoad(src, p, 0))) * 255.0;
                alive = alive || l <= params.threshold;
            }
        }
    }
    return vec4<f32>(select(0.0, 1.0, alive), 0.0, 0.0, 1.0);
}

fn wrap(p: vec2<i32>, size: vec2<i32>) -> vec2<i32> {
    return ((p % size) + size) % size;
}

fn max_radius() -> i32 {
    let r = params.radii;
    return i32(max(max(r.x, r.y), max(r.z, r.w)));
}

// which channels take a cell `a` steps from the centre
fn covers(a: i32) -> vec4<bool> {
    let r = vec4<i32>(params.radii);
    return vec4<bool>(a <= r.x, a <= r.y, a <= r.z, a <= r.w);
}

@fragment
fn fs_hsum(in: VertexOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let p = vec2<i32>(floor(in.position.xy));
    let rmax = max_radius();
    var s = vec4<f32>(0.0);
    for (var dx = -rmax; dx <= rmax; dx++) {
        let v = textureLoad(src, wrap(p + vec2<i32>(dx, 0), size), 0).r;
        s += select(vec4<f32>(0.0), vec4<f32>(v), covers(abs(dx)));
    }
    return s;
}

fn in_range(v: f32, lo: f32, hi: f32) -> bool {
    return v >= lo && v <= hi;
}

@fragment
fn fs_step(in: VertexOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(src));
    let p = vec2<i32>(floor(in.position.xy));
    let rmax = max_radius();
    // live cells in the squares of each half-width, centre included
    var s = vec4<f32>(0.0);
    for (var dy = -rmax; dy <= rmax; dy++) {
        let row = textureLoad(src, wrap(p + vec2<i32>(0, dy), size), 0);
        s += select(vec4<f32>(0.0), row, covers(abs(dy)));
    }
    let me = textureLoad(cells, p, 0).r > 0.5;
    let centre = select(0.0, 1.0, me);
    var next = false;
    switch params.kind {
        case LTL: {
            let n = s.x - centre;
            next = select(
                in_range(n, params.ltl.x, params.ltl.y),
                in_range(n, params.ltl.z, params.ltl.w),
                me,
            );
        }
        case MNCAB: {
            // a square ring of radius r has 8 r cells
            let m1 = (s.x - centre) / 8.0;
            let m2 = (s.y - s.x) / 16.0;
            let t1 = params.mncab.x;
            let t2 = params.mncab.y;
            next = in_range(m1, t1, t2) || in_range(m2, t1, t2);
        }
        case MNCC: {
            var m = vec4<f32>(s.x - centre, s.y - s.x, s.z - s.y, s.w - s.z)
                / vec4<f32>(8.0, 16.0, 24.0, 32.0);
            var lo = params.mncc_lo;
            var hi = params.mncc_hi;
            var alive = me;
            for (var i = 0; i < 4; i++) {
                if (in_range(m[i], lo[i], hi[i])) {
                    alive = !alive;
                }
            }
            next = alive;
        }
        default: {
            let n = s.x - centre;
            next = select(
                in_range(n, params.classic.z, params.classic.w),
                in_range(n, params.classic.x, params.classic.y),
                me,
            );
        }
    }
    return vec4<f32>(select(0.0, 1.0, next), 0.0, 0.0, 1.0);
}

@fragment
fn fs_show(in: VertexOut) -> @location(0) vec4<f32> {
    let grid = vec2<i32>(textureDimensions(src));
    let cell = min(vec2<i32>(floor(in.position.xy)) / i32(params.cell), grid - vec2<i32>(1));
    let alive = textureLoad(src, cell, 0).r > 0.5;
    return vec4<f32>(vec3<f32>(select(1.0, 0.0, alive)), 1.0);
}
