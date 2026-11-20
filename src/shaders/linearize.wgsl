struct Params {
    gamut: mat3x3<f32>,
    transfer: u32,
    reference_white: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

fn pq_to_nits(e: vec3<f32>) -> vec3<f32> {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let p = pow(clamp(e, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / m2));
    let num = max(p - c1, vec3<f32>(0.0));
    let den = c2 - c3 * p;
    return 10000.0 * pow(num / den, vec3<f32>(1.0 / m1));
}

fn hlg_to_nits(e: vec3<f32>) -> vec3<f32> {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let x = clamp(e, vec3<f32>(0.0), vec3<f32>(1.0));
    let low = x * x / 3.0;
    let high = (exp((x - c) / a) + b) / 12.0;
    let scene = select(high, low, x <= vec3<f32>(0.5));
    let luma = dot(scene, vec3<f32>(0.2627, 0.6780, 0.0593));
    return 1000.0 * pow(max(luma, 1e-6), 0.2) * scene;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureLoad(src, vec2<i32>(in.pos.xy), 0);
    var rgb: vec3<f32>;
    switch params.transfer {
        case 0u: {
            rgb = srgb_to_linear(texel.rgb);
        }
        case 2u: {
            rgb = pq_to_nits(texel.rgb) / params.reference_white;
        }
        case 3u: {
            rgb = hlg_to_nits(texel.rgb) / params.reference_white;
        }
        default: {
            rgb = texel.rgb;
        }
    }
    return vec4<f32>(params.gamut * rgb, texel.a);
}
