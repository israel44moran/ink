// "Cartas" de la biblioteca: cada cuaderno es un quad con INCLINACION 3D en perspectiva que
// sigue el cursor (como pokemon-cards-css) y un acabado HOLOGRAFICO (arcoiris + reflejo) que
// se intensifica al pasar el cursor. Instanciado: un quad (6 vertices) por carta.

struct View {
    vp: vec2<f32>,   // tamano del viewport en pixeles
    focal: f32,      // distancia focal (px) para la perspectiva del tilt
    _pad: f32,
};
@group(0) @binding(0) var<uniform> view: View;

struct VsIn {
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2<f32>,  // centro de la carta (px de pantalla)
    @location(1) half: vec2<f32>,    // medio tamano (px)
    @location(2) rot: vec2<f32>,     // inclinacion (rotX, rotY) en radianes
    @location(3) pointer: vec2<f32>, // posicion del cursor sobre la carta (uv 0..1)
    @location(4) hover: f32,         // 0 = reposo, 1 = cursor encima
    @location(5) base: vec3<f32>,    // color base de la carta
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) pointer: vec2<f32>,
    @location(2) hover: f32,
    @location(3) base: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    let q = corners[in.vi];                       // esquina en -1..1
    var p = vec3<f32>(q.x * in.half.x, q.y * in.half.y, 0.0); // local (px)

    // Rotacion 3D: primero alrededor del eje Y, luego del eje X.
    let cy = cos(in.rot.y); let sy = sin(in.rot.y);
    let p1 = vec3<f32>(p.x * cy + p.z * sy, p.y, -p.x * sy + p.z * cy);
    let cx = cos(in.rot.x); let sx = sin(in.rot.x);
    var p2 = vec3<f32>(p1.x, p1.y * cx - p1.z * sx, p1.y * sx + p1.z * cx);

    // Al pasar el cursor, la carta se "levanta" hacia la camara (z+) -> se ve mas grande.
    p2.z = p2.z + in.hover * 60.0;

    // Perspectiva: las partes mas cercanas (z>0) se agrandan.
    let factor = view.focal / max(view.focal - p2.z, 1.0);
    let sp = in.center + vec2<f32>(p2.x, p2.y) * factor; // pantalla (px)

    // px -> clip space.
    let clip = vec2<f32>(sp.x / view.vp.x * 2.0 - 1.0, 1.0 - sp.y / view.vp.y * 2.0);

    var out: VsOut;
    out.clip = vec4<f32>(clip, 0.0, 1.0);
    out.uv = (q + vec2<f32>(1.0, 1.0)) * 0.5; // 0..1
    out.pointer = in.pointer;
    out.hover = in.hover;
    out.base = in.base;
    return out;
}

// Hue (0..1) -> RGB (para el arcoiris holografico).
fn hue2rgb(h: f32) -> vec3<f32> {
    let r = abs(h * 6.0 - 3.0) - 1.0;
    let g = 2.0 - abs(h * 6.0 - 2.0);
    let b = 2.0 - abs(h * 6.0 - 4.0);
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Esquinas redondeadas (SDF de rectangulo) con borde suave (antialias por alfa).
    let uvc = in.uv * 2.0 - vec2<f32>(1.0, 1.0); // -1..1
    let rad = 0.12;
    let d = length(max(abs(uvc) - vec2<f32>(1.0 - rad, 1.0 - rad), vec2<f32>(0.0))) - rad;
    let alpha = 1.0 - smoothstep(0.0, 0.012, d);
    if (alpha <= 0.001) {
        discard;
    }

    var col = in.base;

    // Holografico: arcoiris que se desplaza con el cursor, modulado por un patron de bandas
    // (efecto "foil"). Solo brilla cuando el cursor esta encima (hover).
    let hue = fract((in.uv.x + in.uv.y) * 2.0 + in.pointer.x * 0.6 - in.pointer.y * 0.4);
    let holo = hue2rgb(hue);
    let band = 0.5 + 0.5 * sin((in.uv.x - in.uv.y) * 42.0 + in.pointer.x * 9.0);
    col = col + holo * (0.40 * in.hover) * band;

    // Reflejo (glare): brillo radial centrado en el cursor.
    let gd = distance(in.uv, in.pointer);
    let glare = smoothstep(0.55, 0.0, gd) * 0.45 * in.hover;
    col = col + vec3<f32>(glare, glare, glare);

    // Borde claro sutil.
    let edge = smoothstep(-0.05, 0.0, d);
    col = mix(col, vec3<f32>(1.0, 1.0, 1.0), edge * 0.35);

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), alpha);
}
