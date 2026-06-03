// Shader de trazos: transforma vertices de mundo a clip-space con la matriz de camara
// y pinta con el color por-vertice. La suavidad de bordes la da el MSAA.
//
// GOMA POR TIMESTAMPS: la mascara M (group 1) guarda, por pixel (en espacio de mundo), el
// TIEMPO del ultimo borrado. Cada vertice lleva su `time` de creacion. El fragmento es
// visible solo si su `time` es mayor que el tiempo de borrado en ese pixel (es decir, si
// el trazo se dibujo DESPUES del borrado). Asi un borrado nunca afecta a lo dibujado
// despues, ni reaparece lo viejo al dibujar/re-borrar encima.

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

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
    @location(2) time: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) world: vec2<f32>,
    @location(2) time: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 0.0, 1.0);
    out.color = in.color;
    out.world = in.pos;
    out.time = in.time;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = (in.world - mask.min) * mask.inv_size;
    let erase_t = textureSampleLevel(mask_tex, mask_samp, uv, 0.0).r;
    // Visible si se dibujo despues del ultimo borrado en este pixel.
    let visible = select(0.0, 1.0, in.time > erase_t);
    return vec4<f32>(in.color.rgb, in.color.a * visible);
}
