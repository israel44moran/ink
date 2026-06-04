// "Cartas" de la biblioteca: cada cuaderno es un quad con INCLINACION 3D en perspectiva que
// sigue el cursor (como pokemon-cards-css) y un ACABADO elegible (mate, holografico, galaxia,
// oro, prisma, destellos) que se intensifica al pasar el cursor. Instanciado: 1 quad por carta.

struct View {
    vp: vec2<f32>,
    focal: f32,
    _pad: f32,
};
@group(0) @binding(0) var<uniform> view: View;

struct VsIn {
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2<f32>,
    @location(1) half: vec2<f32>,
    @location(2) rot: vec2<f32>,
    @location(3) pointer: vec2<f32>,
    @location(4) hover: f32,
    @location(5) base: vec3<f32>,
    @location(6) finish: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) pointer: vec2<f32>,
    @location(2) hover: f32,
    @location(3) base: vec3<f32>,
    @location(4) finish: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    let q = corners[in.vi];
    var p = vec3<f32>(q.x * in.half.x, q.y * in.half.y, 0.0);

    let cy = cos(in.rot.y); let sy = sin(in.rot.y);
    let p1 = vec3<f32>(p.x * cy + p.z * sy, p.y, -p.x * sy + p.z * cy);
    let cx = cos(in.rot.x); let sx = sin(in.rot.x);
    var p2 = vec3<f32>(p1.x, p1.y * cx - p1.z * sx, p1.y * sx + p1.z * cx);
    p2.z = p2.z + in.hover * 60.0;

    let factor = view.focal / max(view.focal - p2.z, 1.0);
    let sp = in.center + vec2<f32>(p2.x, p2.y) * factor;
    let clip = vec2<f32>(sp.x / view.vp.x * 2.0 - 1.0, 1.0 - sp.y / view.vp.y * 2.0);

    var out: VsOut;
    out.clip = vec4<f32>(clip, 0.0, 1.0);
    out.uv = (q + vec2<f32>(1.0, 1.0)) * 0.5;
    out.pointer = in.pointer;
    out.hover = in.hover;
    out.base = in.base;
    out.finish = in.finish;
    return out;
}

fn hue2rgb(h: f32) -> vec3<f32> {
    let r = abs(h * 6.0 - 3.0) - 1.0;
    let g = 2.0 - abs(h * 6.0 - 2.0);
    let b = 2.0 - abs(h * 6.0 - 4.0);
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 = p3 + dot(p3, vec3<f32>(p3.y, p3.z, p3.x) + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// Destellos en rejilla: puntos que titilan segun la posicion del cursor.
fn sparkles(uv: vec2<f32>, pointer: vec2<f32>, density: f32, thresh: f32) -> f32 {
    let g = uv * density;
    let cell = floor(g);
    let f = fract(g) - 0.5;
    let rnd = hash21(cell);
    let d = length(f);
    let tw = 0.5 + 0.5 * sin(rnd * 40.0 + (pointer.x - pointer.y) * 16.0);
    return smoothstep(0.16, 0.0, d) * step(thresh, rnd) * tw;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Esquinas redondeadas (SDF) con borde suave (alfa).
    let uvc = in.uv * 2.0 - vec2<f32>(1.0, 1.0);
    let rad = 0.12;
    let d = length(max(abs(uvc) - vec2<f32>(1.0 - rad, 1.0 - rad), vec2<f32>(0.0))) - rad;
    let alpha = 1.0 - smoothstep(0.0, 0.012, d);
    if (alpha <= 0.001) {
        discard;
    }

    let h = in.hover;
    let fin = i32(round(in.finish));
    var col = in.base;

    // Reflejo (glare) comun a todos los acabados.
    let gd = distance(in.uv, in.pointer);
    let glare = smoothstep(0.55, 0.0, gd) * 0.45 * h;

    if (fin == 1) {
        // HOLOGRAFICO: arcoiris diagonal con bandas de foil.
        let hue = fract((in.uv.x + in.uv.y) * 2.0 + in.pointer.x * 0.6 - in.pointer.y * 0.4);
        let band = 0.5 + 0.5 * sin((in.uv.x - in.uv.y) * 42.0 + in.pointer.x * 9.0);
        col = col + hue2rgb(hue) * (0.45 * h) * band;
    } else if (fin == 2) {
        // GALAXIA: nebulosa (arcoiris tenue ondulado) + muchos destellos (estrellas).
        let hue = fract(in.uv.y * 1.2 + in.pointer.x * 0.5 + sin(in.uv.x * 6.0) * 0.1);
        col = col + hue2rgb(hue) * (0.22 * h);
        let st = sparkles(in.uv, in.pointer, 26.0, 0.80);
        col = col + vec3<f32>(0.9, 0.95, 1.0) * st * (0.4 + 0.6 * h);
    } else if (fin == 3) {
        // ORO: bandas doradas brillantes (foil metalico) + brillo fuerte.
        let band = 0.5 + 0.5 * sin((in.uv.x + in.uv.y) * 30.0 - in.pointer.x * 10.0);
        let gold = vec3<f32>(1.0, 0.82, 0.35);
        col = col + gold * (0.5 * h) * band;
    } else if (fin == 4) {
        // PRISMA: arcoiris vertical fino que se desplaza con el cursor.
        let hue = fract(in.uv.x * 3.0 + in.pointer.y * 0.8);
        let band = 0.5 + 0.5 * sin(in.uv.x * 80.0);
        col = col + hue2rgb(hue) * (0.42 * h) * band;
    } else if (fin == 5) {
        // DESTELLOS: glitter denso que titila con el cursor.
        let st1 = sparkles(in.uv, in.pointer, 34.0, 0.72);
        let st2 = sparkles(in.uv + vec2<f32>(0.5, 0.5), in.pointer * 1.3, 22.0, 0.78);
        col = col + vec3<f32>(1.0, 1.0, 0.95) * (st1 + st2) * (0.3 + 0.7 * h);
    }
    // fin == 0 (MATE): solo el color base + glare.

    col = col + vec3<f32>(glare, glare, glare);
    let edge = smoothstep(-0.05, 0.0, d);
    col = mix(col, vec3<f32>(1.0, 1.0, 1.0), edge * 0.35);

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), alpha);
}
