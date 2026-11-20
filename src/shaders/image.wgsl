struct View {
    scale: vec2<f32>,
    offset: vec2<f32>,
};

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let corner = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    let p = (corner * 2.0 - 1.0) * vec2<f32>(1.0, -1.0);
    var out: VsOut;
    out.pos = vec4<f32>(p * view.scale + view.offset, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    let cell = vec2<i32>(floor(in.pos.xy / 12.0));
    let checker = select(0.04, 0.09, ((cell.x + cell.y) & 1) == 0);
    return vec4<f32>(mix(vec3<f32>(checker), c.rgb, c.a), 1.0);
}
