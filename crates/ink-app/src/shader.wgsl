// Shader de trazos: transforma vertices de mundo a clip-space con la matriz de
// camara y pinta con el color por-vertice. La suavidad de bordes la da el MSAA.
//
// Ademas multiplica el alfa por la MASCARA DE BORRADO (group 1): una textura R8 en
// espacio de mundo donde 1 = visible y 0 = borrado. Asi la goma borra a nivel de
// pixel (su forma/tamano/opacidad exactos) sin tocar la geometria vectorial.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// Mascara de borrado: uv = (world - min) * inv_size.
struct Mask {
    min: vec2<f32>,
    inv_size: vec2<f32>,
};
@group(1) @binding(0) var<uniform> mask: Mask;
@group(1) @binding(1) var mask_tex: texture_2d<f32>;
@group(1) @binding(2) var mask_samp: sampler;

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) world: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 0.0, 1.0);
    out.color = in.color;
    out.world = in.pos;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = (in.world - mask.min) * mask.inv_size;
    let m = textureSample(mask_tex, mask_samp, uv).r;
    return vec4<f32>(in.color.rgb, in.color.a * m);
}
