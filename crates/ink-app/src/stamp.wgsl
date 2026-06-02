// Shader de ESTAMPADOS texturizados (pinceles estilo Photoshop).
//
// Cada estampado es un quad con coordenadas de mundo + UV en la textura de la punta
// (mascara alfa en escala de grises) + color RGBA. El fragment multiplica el color
// del pincel por la cobertura de la punta (canal rojo de la textura R8) y el alfa del
// estampado (flujo/opacidad de ese punto).

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var tip_tex: texture_2d<f32>;
@group(1) @binding(1) var tip_samp: sampler;

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Cobertura de la punta (R8): 1.0 = opaco, 0.0 = transparente.
    let coverage = textureSample(tip_tex, tip_samp, in.uv).r;
    let a = coverage * in.color.a;
    // Alfa "straight": el blend estandar (src.a, 1-src.a) lo compone correctamente.
    return vec4<f32>(in.color.rgb, a);
}
