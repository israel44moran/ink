// Shader de ESTAMPADOS texturizados (pinceles estilo Photoshop).
//
// Cada estampado es un quad con coordenadas de mundo + UV en la textura de la punta
// (mascara alfa en escala de grises) + color RGBA. El fragment multiplica el color
// del pincel por la cobertura de la punta (canal rojo de la textura R8) y el alfa del
// estampado (flujo/opacidad de ese punto).
//
// Tambien multiplica por la MASCARA DE BORRADO (group 2) para que la goma borre los
// estampados a nivel de pixel igual que los trazos.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var tip_tex: texture_2d<f32>;
@group(1) @binding(1) var tip_samp: sampler;

struct Mask {
    min: vec2<f32>,
    inv_size: vec2<f32>,
};
@group(2) @binding(0) var<uniform> mask: Mask;
@group(2) @binding(1) var mask_tex: texture_2d<f32>;
@group(2) @binding(2) var mask_samp: sampler;

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.world = in.pos;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Cobertura de la punta (R8): 1.0 = opaco, 0.0 = transparente.
    let coverage = textureSample(tip_tex, tip_samp, in.uv).r;
    let muv = (in.world - mask.min) * mask.inv_size;
    let m = textureSample(mask_tex, mask_samp, muv).r;
    let a = coverage * in.color.a * m;
    return vec4<f32>(in.color.rgb, a);
}
