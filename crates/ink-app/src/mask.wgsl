// Escribe en la mascara M (textura Rg32Float, en espacio de mundo) el estado de borrado por
// pixel: R = TIEMPO del ultimo borrado, G = FUERZA de borrado (0..1, para goma con textura
// suave). Sin blend (replace): los borrados se aplican en orden de tiempo creciente.
// Los shaders de contenido leen M: un trazo se ve si su tiempo de creacion es mayor que R
// (dibujado despues del borrado); si no, su alfa se multiplica por (1 - G).

struct Mask {
    min: vec2<f32>,
    inv_size: vec2<f32>,
};
@group(0) @binding(0) var<uniform> mask: Mask;

fn world_to_mask_clip(world: vec2<f32>) -> vec4<f32> {
    let uv = (world - mask.min) * mask.inv_size;
    return vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

// ---------- Goma REDONDA: cada disco es una instancia [center, radio, tiempo] ----------
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
    // Dentro del circulo: borrado DURO (fuerza = 1). Fuera: no se toca.
    if (distance(in.world, in.center) > in.radius) {
        discard;
    }
    return vec4<f32>(in.time, 1.0, 0.0, 0.0);
}

// ---------- Goma con FORMA de pincel: estampados texturizados (quad + UV de la punta) ----------
@group(1) @binding(0) var tip_tex: texture_2d<f32>;
@group(1) @binding(1) var tip_samp: sampler;

struct StIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>, // no se usa (el layout es el de StampVertex)
    @location(3) time: f32,
};
struct StOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) time: f32,
};

@vertex
fn vs_erase_stamp(in: StIn) -> StOut {
    var out: StOut;
    out.clip = world_to_mask_clip(in.pos);
    out.uv = in.uv;
    out.time = in.time;
    return out;
}

@fragment
fn fs_erase_stamp(in: StOut) -> @location(0) vec4<f32> {
    // La cobertura (alfa) de la punta = fuerza de borrado en ese pixel (textura suave).
    let coverage = textureSampleLevel(tip_tex, tip_samp, in.uv, 0.0).r;
    if (coverage < 0.02) {
        discard; // no pisar los pixeles que la punta apenas cubre (conserva lo ya borrado)
    }
    return vec4<f32>(in.time, coverage, 0.0, 0.0);
}
