// inverse lens mapping: every output pixel looks up where in the source
// its light came from. with p = (uv - 0.5 - centre) * (aspect, 1) / zoom
// and r = |p|, the source radius r' is
//   barrel        r' = r * (1 + k1 r^2),        k1 = 2 strength
//   equidistant   r' = tan(r f) / f
//   equisolid     r' = tan(2 asin(r f / 2)) / f
//   stereographic r' = tan(2 atan(r f / 2)) / f
// with f = 1.6 strength + 1e-4, so strength 0 is the identity for all
// four. lookups outside the source come back black, and so do rays whose
// angle reaches pi/2, where tan blows up and then wraps back to positive.

struct Params {
    strength: f32,
    zoom: f32,
    centre: vec2<f32>,
    projection: u32,
    aspect: f32,
    pad: vec2<f32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

const BARREL: u32 = 0u;
const EQUIDISTANT: u32 = 1u;
const EQUISOLID: u32 = 2u;
const STEREOGRAPHIC: u32 = 3u;
const HALF_PI: f32 = 1.5707963;

// the source radius, or -1 when the ray leaves the hemisphere. the sign
// of tan is no test for that: past pi it is positive again, and
// tan(2 asin(1)) is a tiny number of driver-dependent sign.
fn remap(r: f32) -> f32 {
    let f = params.strength * 1.6 + 1e-4;
    var theta = 0.0;
    switch params.projection {
        case EQUIDISTANT: {
            theta = r * f;
        }
        case EQUISOLID: {
            theta = 2.0 * asin(min(r * f * 0.5, 1.0));
        }
        case STEREOGRAPHIC: {
            theta = 2.0 * atan(r * f * 0.5);
        }
        default: {
            let k1 = params.strength * 2.0;
            return r * (1.0 + k1 * r * r);
        }
    }
    if (theta >= HALF_PI - 1e-3) {
        return -1.0;
    }
    return tan(theta) / f;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let scale = vec2<f32>(params.aspect, 1.0);
    let p = (in.uv - 0.5 - params.centre) * scale / params.zoom;
    let r = length(p);
    var q = p;
    var outside = false;
    if (params.strength > 0.0 && r > 1e-6) {
        let rp = remap(r);
        outside = rp < 0.0;
        q = p * (rp / r);
    }
    let uv = q / scale + 0.5 + params.centre;
    outside = outside || any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0));
    // the sample stays in uniform control flow because textureSample
    // takes derivatives; the select drops it afterwards
    let colour = textureSample(src, src_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    return vec4<f32>(select(colour.rgb, vec3<f32>(0.0), outside), 1.0);
}
