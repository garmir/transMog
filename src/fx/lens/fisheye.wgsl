// angle-driven inverse lens mapping. every output pixel is turned into a
// ray by the chosen lens, the ray is rotated by the virtual camera, and
// the ray is projected back into the rectilinear source. with
// p = (uv - 0.5 - centre) * (aspect, 1) / zoom and rn = |p| / aspect
// (so rn = 1 at the output's horizontal edge, where the lens sees
// lens_fov / 2 off axis), the ray angle theta is
//   rectilinear   theta = atan(rn tan(h))
//   equidistant   theta = rn h
//   equisolid     theta = 2 asin(rn sin(h / 2))
//   stereographic theta = 2 atan(rn tan(h / 2))
//   orthographic  theta = asin(rn sin(h))
// with h = lens_fov / 2 clamped to where each formula is finite. the
// source covers source_fov across its width and its edge is 0.5 from
// centre, so a ray d lands at p_src = d.xy / d.z * aspect / 2 / tan(source_fov / 2). rays behind the
// source (d.z <= 0) and asin arguments past 1 are outside; what happens
// to lookups outside the source frame is the edge mode.

struct Params {
    lens_fov: f32,
    source_fov: f32,
    zoom: f32,
    k1: f32,
    centre: vec2<f32>,
    projection: u32,
    edge: u32,
    aspect: f32,
    yaw: f32,
    pitch: f32,
    roll: f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

const RECTILINEAR: u32 = 0u;
const EQUIDISTANT: u32 = 1u;
const EQUISOLID: u32 = 2u;
const STEREOGRAPHIC: u32 = 3u;
const ORTHOGRAPHIC: u32 = 4u;

const EDGE_BLACK: u32 = 0u;
const EDGE_CLAMP: u32 = 1u;
const EDGE_MIRROR: u32 = 2u;

const PI: f32 = 3.1415927;
// just short of a right angle, where tan is still finite in f32
const NEAR_RIGHT: f32 = 1.5533;

// the off-axis angle of the ray through normalised radius rn, or -1
// when the lens has nothing there
fn ray_angle(rn: f32) -> f32 {
    let h = params.lens_fov * 0.5;
    switch params.projection {
        case EQUIDISTANT: {
            return rn * h;
        }
        case EQUISOLID: {
            let s = rn * sin(min(h, PI - 1e-3) * 0.5);
            if (s > 1.0) {
                return -1.0;
            }
            return 2.0 * asin(s);
        }
        case STEREOGRAPHIC: {
            return 2.0 * atan(rn * tan(min(h * 0.5, NEAR_RIGHT)));
        }
        case ORTHOGRAPHIC: {
            let s = rn * sin(min(h, PI * 0.5));
            if (s > 1.0) {
                return -1.0;
            }
            return asin(s);
        }
        default: {
            return atan(rn * tan(min(h, NEAR_RIGHT)));
        }
    }
}

// yaw about the vertical axis, pitch about the horizontal, roll about
// the view axis, applied in that order to the lens ray
fn rotate(d: vec3<f32>) -> vec3<f32> {
    let cr = cos(params.roll);
    let sr = sin(params.roll);
    let cp = cos(params.pitch);
    let sp = sin(params.pitch);
    let cy = cos(params.yaw);
    let sy = sin(params.yaw);
    let rolled = vec3<f32>(cr * d.x - sr * d.y, sr * d.x + cr * d.y, d.z);
    let pitched = vec3<f32>(rolled.x, cp * rolled.y - sp * rolled.z, sp * rolled.y + cp * rolled.z);
    return vec3<f32>(cy * pitched.x + sy * pitched.z, pitched.y, -sy * pitched.x + cy * pitched.z);
}

fn mirror(uv: vec2<f32>) -> vec2<f32> {
    return 1.0 - abs(1.0 - 2.0 * fract(uv * 0.5));
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let scale = vec2<f32>(params.aspect, 1.0);
    let p = (in.uv - 0.5 - params.centre) * scale / params.zoom;
    let r = length(p);
    var rn = 2.0 * r / params.aspect;
    rn = rn * (1.0 + params.k1 * rn * rn);
    var outside = false;
    var uv = in.uv;
    let theta = ray_angle(rn);
    if (theta < 0.0) {
        outside = true;
    } else {
        let phi = select(atan2(p.y, p.x), 0.0, r < 1e-6);
        let st = sin(theta);
        let d = rotate(vec3<f32>(st * cos(phi), st * sin(phi), cos(theta)));
        if (d.z <= 1e-5) {
            outside = true;
        } else {
            let p_src = d.xy / d.z * 0.5 * params.aspect / tan(params.source_fov * 0.5);
            uv = p_src / scale + 0.5;
        }
    }
    let off_frame = any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0));
    switch params.edge {
        case EDGE_CLAMP: {
            uv = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
        }
        case EDGE_MIRROR: {
            uv = mirror(uv);
        }
        default: {
            outside = outside || off_frame;
        }
    }
    // the sample stays in uniform control flow because textureSample
    // takes derivatives; the select drops it afterwards
    let colour = textureSample(src, src_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    return vec4<f32>(select(colour.rgb, vec3<f32>(0.0), outside), 1.0);
}
