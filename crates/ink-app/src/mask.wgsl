// Shaders que ESCRIBEN en la mascara de borrado M (textura R8 en espacio de mundo).
//
// - vs_erase/fs_erase: estampan discos suaves (forma/tamano/opacidad de la goma) que
//   RESTAN cobertura de M (blend: M_new = M_old * (1 - cov*strength)). Cada disco es una
//   INSTANCIA (center.xy, radius, strength) -> todos los discos de un trazo se dibujan en
//   UN solo draw (sin un submit por disco), eliminando el lag al borrar y al deshacer.
// - vs_restore/fs_restore: dibujan geometria de trazo (mundo + color) SUBIENDO M
//   (blend Max) para que el contenido nuevo no quede borrado por borrados anteriores.

struct Mask {
    min: vec2<f32>,
    inv_size: vec2<f32>,
};
@group(0) @binding(0) var<uniform> mask: Mask;

// world -> clip de la textura M (y invertida porque la textura es y-abajo).
fn world_to_mask_clip(world: vec2<f32>) -> vec4<f32> {
    let uv = (world - mask.min) * mask.inv_size;
    return vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

// ---------------- Borrar (discos instanciados) ----------------
struct ErIn {
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2<f32>,
    @location(1) rs: vec2<f32>, // x = radio, y = fuerza/opacidad
};
struct ErOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec2<f32>,
    @location(1) center: vec2<f32>,
    @location(2) rs: vec2<f32>,
};

@vertex
fn vs_erase(in: ErIn) -> ErOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    let c = corners[in.vi];
    let r = in.rs.x * 1.2; // margen para el borde suave
    let world = in.center + c * r;
    var out: ErOut;
    out.clip = world_to_mask_clip(world);
    out.world = world;
    out.center = in.center;
    out.rs = in.rs;
    return out;
}

@fragment
fn fs_erase(in: ErOut) -> @location(0) vec4<f32> {
    let radius = in.rs.x;
    let strength = in.rs.y;
    let d = distance(in.world, in.center);
    let inner = radius * 0.6;
    let cov = 1.0 - smoothstep(inner, radius, d);
    return vec4<f32>(cov * strength, 0.0, 0.0, 1.0);
}

// ---------------- Restaurar (geometria de trazo) ----------------
struct RsIn {
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
};
struct RsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) a: f32,
};

@vertex
fn vs_restore(in: RsIn) -> RsOut {
    var out: RsOut;
    out.clip = world_to_mask_clip(in.pos);
    out.a = in.color.a;
    return out;
}

@fragment
fn fs_restore(in: RsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.a, 0.0, 0.0, 1.0);
}
