// Shader que escribe el TIEMPO DE BORRADO en la mascara M (textura R32Float en espacio de
// mundo). Cada disco de la goma es una INSTANCIA [center.x, center.y, radio, tiempo]. El
// fragmento escribe `tiempo` en los pixeles dentro del circulo (descarta los de fuera).
// Sin blend (replace): como los borrados se aplican en orden de tiempo creciente, el
// ultimo (mayor) gana donde se solapan. Los shaders de contenido comparan: un trazo se ve
// solo si su tiempo de creacion es mayor que el tiempo guardado aqui.

struct Mask {
    min: vec2<f32>,
    inv_size: vec2<f32>,
};
@group(0) @binding(0) var<uniform> mask: Mask;

fn world_to_mask_clip(world: vec2<f32>) -> vec4<f32> {
    let uv = (world - mask.min) * mask.inv_size;
    return vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

struct ErIn {
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2<f32>,
    @location(1) rs: vec2<f32>, // x = radio, y = tiempo del borrado
};
struct ErOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec2<f32>,
    @location(1) center: vec2<f32>,
    @location(2) radius: f32,
    @location(3) time: f32,
};

@vertex
fn vs_erase(in: ErIn) -> ErOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    let c = corners[in.vi];
    let world = in.center + c * in.rs.x;
    var out: ErOut;
    out.clip = world_to_mask_clip(world);
    out.world = world;
    out.center = in.center;
    out.radius = in.rs.x;
    out.time = in.rs.y;
    return out;
}

@fragment
fn fs_erase(in: ErOut) -> @location(0) vec4<f32> {
    // Solo los pixeles dentro del circulo se marcan con el tiempo de borrado.
    if (distance(in.world, in.center) > in.radius) {
        discard;
    }
    return vec4<f32>(in.time, 0.0, 0.0, 0.0);
}
