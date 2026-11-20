struct Panel {
    rect: vec4<f32>,
    color: vec4<f32>,
    screen: vec2<f32>,
    radius: f32,
    pad: f32,
};

@group(0) @binding(0) var<uniform> panel: Panel;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) pixel: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let corner = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    let pixel = panel.rect.xy + corner * panel.rect.zw;
    let ndc = pixel / panel.screen * 2.0 - 1.0;
    var out: VsOut;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.pixel = pixel;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let half_size = panel.rect.zw * 0.5;
    let p = in.pixel - (panel.rect.xy + half_size);
    let q = abs(p) - half_size + panel.radius;
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - panel.radius;
    let coverage = clamp(0.5 - d, 0.0, 1.0);
    return vec4<f32>(panel.color.rgb, panel.color.a * coverage);
}
