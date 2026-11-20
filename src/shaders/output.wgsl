struct Output {
    mode: u32,
    encode_srgb: u32,
    reference_white: f32,
    pad: f32,
};

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var<uniform> output: Output;

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

fn tonemap_sdr(rgb: vec3<f32>) -> vec3<f32> {
    let c = max(rgb, vec3<f32>(0.0));
    let peak = max(max(c.r, c.g), c.b);
    let knee = 0.8;
    if peak <= knee {
        return c;
    }
    let mapped = knee + (1.0 - knee) * (1.0 - exp(-(peak - knee) / (1.0 - knee)));
    return c * (mapped / peak);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let low = c * 12.92;
    let high = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, c <= vec3<f32>(0.0031308));
}

fn nits_to_pq(nits: vec3<f32>) -> vec3<f32> {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let y = pow(clamp(nits / 10000.0, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(m1));
    return pow((c1 + c2 * y) / (1.0 + c3 * y), vec3<f32>(m2));
}

const BT709_TO_BT2020 = mat3x3<f32>(
    vec3<f32>(0.6274040, 0.0690970, 0.0163916),
    vec3<f32>(0.3292820, 0.9195400, 0.0880132),
    vec3<f32>(0.0433136, 0.0113612, 0.8955950),
);

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let rgb = textureLoad(scene, vec2<i32>(in.pos.xy), 0).rgb;
    switch output.mode {
        case 1u: {
            return vec4<f32>(rgb * (output.reference_white / 80.0), 1.0);
        }
        case 2u: {
            let wide = max(BT709_TO_BT2020 * rgb, vec3<f32>(0.0));
            return vec4<f32>(nits_to_pq(wide * output.reference_white), 1.0);
        }
        default: {
            let sdr = clamp(tonemap_sdr(rgb), vec3<f32>(0.0), vec3<f32>(1.0));
            if output.encode_srgb == 1u {
                return vec4<f32>(linear_to_srgb(sdr), 1.0);
            }
            return vec4<f32>(sdr, 1.0);
        }
    }
}
