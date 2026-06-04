// "Cartas" de la biblioteca: cada cuaderno es un quad con INCLINACION 3D en perspectiva que
// sigue el cursor (como pokemon-cards-css). El ACABADO se compone por CAPAS:
//   - un DISEÑO base: foils (mate, holo, galaxia, oro, prisma, destellos, aurora, neon,
//     esmeralda, rubi, cromo, atardecer) o un CARGADOR organico animado (finish >= 100), y
//   - CAPAS combinables (mascara de bits `fx`): 1=destellos, 2=brillo animado, 4=resplandor,
//     con una `intensity` global y un `accent` (color) elegibles.
// Todo se anima con `time` (las cartas animan sutil siempre; al pasar el cursor se intensifica).

struct View {
    vp: vec2<f32>,
    focal: f32,
    time: f32,
};
@group(0) @binding(0) var<uniform> view: View;
// Texturas de MATERIAL realistas (array de capas). El indice de capa = (texture - 1).
@group(0) @binding(1) var mat_tex: texture_2d_array<f32>;
@group(0) @binding(2) var mat_samp: sampler;

// Muestrea una capa de material a un LOD fijo (textureSampleLevel: valido en cualquier flujo;
// LOD 1.5 ~ acorde al tamaño de la carta -> nitido y sin aliasing). uv se repite (tileado).
fn mat_sample(layer: i32, uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(mat_tex, mat_samp, uv, layer, 1.5).rgb;
}
// Muestreo triplanar (para las figuras 3D): mezcla por la normal las 3 proyecciones.
fn mat_triplanar(layer: i32, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let an = abs(n);
    let w = an / max(an.x + an.y + an.z, 0.001);
    let cx = mat_sample(layer, p.yz * 0.5 + vec2<f32>(0.5));
    let cy = mat_sample(layer, p.xz * 0.5 + vec2<f32>(0.5));
    let cz = mat_sample(layer, p.xy * 0.5 + vec2<f32>(0.5));
    return cx * w.x + cy * w.y + cz * w.z;
}

// DIAMANTE: acolchado capitoné (rombos puff + costuras + boton), cuero/satén burdeos. Procedural.
fn diamante(uv: vec2<f32>) -> vec3<f32> {
    let p = uv * 5.0;
    let q = vec2<f32>(p.x + p.y, p.x - p.y);            // ejes diagonales -> rombos
    let cell = fract(q) - vec2<f32>(0.5);
    let d = max(abs(cell.x), abs(cell.y));              // 0 centro .. 0.5 borde del rombo
    let seam = smoothstep(0.5, 0.40, d);                // costura hundida en el borde
    let puff = clamp(1.0 - d * 1.6, 0.0, 1.0);          // abultamiento hacia el centro
    let button = smoothstep(0.09, 0.0, length(cell));   // boton central
    let base = vec3<f32>(0.34, 0.05, 0.09);             // burdeos
    var c = base * (0.70 + 0.55 * puff);
    c = c * (1.0 - 0.6 * seam);
    c = c - vec3<f32>(button * 0.22);
    let sheen = pow(puff, 3.0) * 0.32;                  // brillo satinado
    return c + vec3<f32>(0.95, 0.75, 0.78) * sheen;
}

const TEX_DIAMOND: i32 = 11; // indice procedural (no es capa de imagen)
// Material por UV (tapa/caras): diamante procedural o textura de imagen segun el indice.
fn material_at(tex: i32, uv: vec2<f32>) -> vec3<f32> {
    if (tex == TEX_DIAMOND) { return diamante(uv); }
    return mat_sample(tex - 1, uv);
}
// Material para figuras 3D (triplanar); diamante usa la proyeccion dominante.
fn material_tri(tex: i32, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    if (tex == TEX_DIAMOND) {
        let an = abs(n);
        var uv2: vec2<f32>;
        if (an.x >= an.y && an.x >= an.z) { uv2 = p.yz; }
        else if (an.y >= an.z) { uv2 = p.xz; }
        else { uv2 = p.xy; }
        return diamante(uv2 * 0.5 + vec2<f32>(0.5));
    }
    return mat_triplanar(tex - 1, p, n);
}

struct VsIn {
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2<f32>,
    @location(1) half: vec2<f32>,
    @location(2) rot: vec2<f32>,
    @location(3) pointer: vec2<f32>,
    @location(4) hover: f32,
    @location(5) base: vec3<f32>,
    @location(6) finish: f32,
    @location(7) fx: f32,
    @location(8) intensity: f32,
    @location(9) accent: vec3<f32>,
    @location(10) depth: f32,
    @location(11) overhang: f32,
    @location(12) board: f32,
    @location(13) shape: f32,
    @location(14) texture: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) pointer: vec2<f32>,
    @location(2) hover: f32,
    @location(3) base: vec3<f32>,
    @location(4) finish: f32,
    @location(5) fx: f32,
    @location(6) intensity: f32,
    @location(7) accent: vec3<f32>,
    @location(8) time: f32,
    @location(9) aspect: f32,
    @location(10) face: f32,
    @location(11) pz01: f32,
    @location(12) board: f32,
    @location(13) shape: f32,
    @location(14) overhang: f32,
    @location(15) texture: f32,
};

// El cuaderno es una CAJA (6 caras): 0=frente (portada animada), 1=reverso (cinta con el
// nombre), 2-5=cantos (pila de hojas). Backface culling deja ver solo las caras visibles.
@vertex
fn vs_main(in: VsIn) -> VsOut {
    let face = i32(in.vi) / 6;
    let k = i32(in.vi) % 6;
    var qp = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0)
    );
    let st = qp[k];
    // Origen + aristas (en unidades -1..1) por cara, con bobinado CCW hacia afuera.
    var origin: vec3<f32>; var du: vec3<f32>; var dv: vec3<f32>;
    if (face == 0) { origin = vec3<f32>(-1.0, -1.0, 1.0); du = vec3<f32>(2.0, 0.0, 0.0); dv = vec3<f32>(0.0, 2.0, 0.0); }
    else if (face == 1) { origin = vec3<f32>(1.0, -1.0, -1.0); du = vec3<f32>(-2.0, 0.0, 0.0); dv = vec3<f32>(0.0, 2.0, 0.0); }
    else if (face == 2) { origin = vec3<f32>(1.0, -1.0, -1.0); du = vec3<f32>(0.0, 2.0, 0.0); dv = vec3<f32>(0.0, 0.0, 2.0); }
    else if (face == 3) { origin = vec3<f32>(-1.0, -1.0, 1.0); du = vec3<f32>(0.0, 2.0, 0.0); dv = vec3<f32>(0.0, 0.0, -2.0); }
    else if (face == 4) { origin = vec3<f32>(-1.0, 1.0, -1.0); du = vec3<f32>(0.0, 0.0, 2.0); dv = vec3<f32>(2.0, 0.0, 0.0); }
    else { origin = vec3<f32>(-1.0, -1.0, -1.0); du = vec3<f32>(2.0, 0.0, 0.0); dv = vec3<f32>(0.0, 0.0, 2.0); }
    var unit = origin + du * st.x + dv * st.y;          // posicion en el cubo unidad
    // Ceja de tapa: las caras de canto (pila de hojas) se meten hacia dentro en X/Y; las tapas
    // (0,1) y el lomo (3, a ras del lomo) quedan a tamaño completo -> la tapa sobresale.
    let oh = in.overhang;
    if (face == 2) { unit.x = unit.x * (1.0 - 2.0 * oh); }              // fore-edge (derecha)
    if (face == 4 || face == 5) { unit.y = unit.y * (1.0 - 2.0 * oh); } // top / pie
    let hz = in.half.x * max(in.depth, 0.02);           // grosor por instancia (segun la forma)
    var p = vec3<f32>(unit.x * in.half.x, unit.y * in.half.y, unit.z * hz);

    let cy = cos(in.rot.y); let sy = sin(in.rot.y);
    let p1 = vec3<f32>(p.x * cy + p.z * sy, p.y, -p.x * sy + p.z * cy);
    let cx = cos(in.rot.x); let sx = sin(in.rot.x);
    var p2 = vec3<f32>(p1.x, p1.y * cx - p1.z * sx, p1.y * sx + p1.z * cx);
    p2.z = p2.z + in.hover * 70.0;

    let factor = view.focal / max(view.focal - p2.z, 1.0);
    let sp = in.center + vec2<f32>(p2.x, p2.y) * factor;
    let clip = vec2<f32>(sp.x / view.vp.x * 2.0 - 1.0, 1.0 - sp.y / view.vp.y * 2.0);

    var out: VsOut;
    out.clip = vec4<f32>(clip, 0.0, 1.0);
    out.uv = st;
    out.pointer = in.pointer;
    out.hover = in.hover;
    out.base = in.base;
    out.finish = in.finish;
    out.fx = in.fx;
    out.intensity = in.intensity;
    out.accent = in.accent;
    out.time = view.time;
    out.aspect = in.half.y / max(in.half.x, 1.0);
    out.face = f32(face);
    out.pz01 = (unit.z + 1.0) * 0.5;
    out.board = in.board;
    out.shape = in.shape;
    out.overhang = in.overhang;
    out.texture = in.texture;
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

// Destellos en rejilla que titilan con el cursor y el tiempo.
fn sparkles(uv: vec2<f32>, pointer: vec2<f32>, t: f32, density: f32, thresh: f32) -> f32 {
    let g = uv * density;
    let cell = floor(g);
    let f = fract(g) - vec2<f32>(0.5);
    let rnd = hash21(cell);
    let d = length(f);
    let tw = 0.5 + 0.5 * sin(rnd * 40.0 + (pointer.x - pointer.y) * 16.0 + t * 3.0);
    return smoothstep(0.16, 0.0, d) * step(thresh, rnd) * tw;
}

// --- Utilidades para los cargadores organicos (SDF de blobs) ---
fn sd_circle(p: vec2<f32>, r: f32) -> f32 {
    return length(p) - r;
}
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}
// Cobertura (1 dentro, 0 fuera) de una SDF con borde suave.
fn fill(d: f32, aa: f32) -> f32 {
    return 1.0 - smoothstep(-aa, aa, d);
}
fn rot2(a: f32) -> mat2x2<f32> {
    let c = cos(a); let s = sin(a);
    return mat2x2<f32>(vec2<f32>(c, s), vec2<f32>(-s, c));
}

// Cobertura del cargador organico `id` en el punto `p` (centrado, con aspecto corregido).
fn loader_cov(id: i32, p: vec2<f32>, t: f32) -> f32 {
    let aa = 0.02;
    var cov = 0.0;
    if (id == 0) {
        // Gota: circulo cuyo radio ondula por angulo y tiempo.
        let ang = atan2(p.y, p.x);
        let r = 0.52 + 0.07 * sin(ang * 3.0 + t * 2.0) + 0.05 * sin(ang * 5.0 - t * 1.3);
        cov = fill(length(p) - r, aa);
    } else if (id == 1) {
        // Metabolas: dos circulos que orbitan y se funden.
        let a = t * 1.5;
        let c1 = vec2<f32>(cos(a), sin(a)) * 0.30;
        let d = smin(sd_circle(p - c1, 0.26), sd_circle(p + c1, 0.26), 0.24);
        cov = fill(d, aa);
    } else if (id == 2) {
        // Onda: cuadrado redondeado que gira y respira.
        let q = rot2(t * 0.7) * p;
        let rb = 0.20 + 0.07 * sin(t * 2.0);
        let bx = vec2<f32>(0.40, 0.40) - vec2<f32>(rb, rb);
        let d = length(max(abs(q) - bx, vec2<f32>(0.0))) - rb;
        cov = fill(d, aa);
    } else if (id == 3) {
        // Pulso: anillo organico que late.
        let ang = atan2(p.y, p.x);
        let rr = 0.42 + 0.05 * sin(t * 2.5) + 0.04 * sin(ang * 4.0 + t);
        cov = fill(abs(length(p) - rr) - 0.10, aa);
    } else if (id == 4) {
        // Orbita: tres puntos girando.
        for (var i: i32 = 0; i < 3; i = i + 1) {
            let a = t * 1.6 + f32(i) * 2.0944;
            let c = vec2<f32>(cos(a), sin(a)) * 0.42;
            cov = max(cov, fill(sd_circle(p - c, 0.13), aa));
        }
    } else if (id == 5) {
        // Espiral: puntos que menguan en espiral.
        for (var i: i32 = 0; i < 6; i = i + 1) {
            let f = f32(i) / 6.0;
            let a = t * 1.2 + f * 6.2832;
            let rad = 0.12 + f * 0.34;
            let c = vec2<f32>(cos(a), sin(a)) * rad;
            cov = max(cov, fill(sd_circle(p - c, 0.11 * (1.0 - 0.5 * f)), aa));
        }
    } else if (id == 6) {
        // Ameba: blob deformado que gira.
        let ang = atan2(p.y, p.x) + t * 0.5;
        let r = 0.48 + 0.05 * sin(ang * 2.0) + 0.04 * sin(ang * 3.0 + t) + 0.03 * sin(ang * 5.0 - t * 1.7);
        cov = fill(length(p) - r, aa);
    } else if (id == 7) {
        // Burbujas: ascienden y se desvanecen arriba/abajo.
        for (var i: i32 = 0; i < 5; i = i + 1) {
            let fi = f32(i);
            let x = 0.5 * sin(fi * 1.7);
            let y = fract(t * 0.22 + fi * 0.2) * 1.7 - 0.85;
            let rr = 0.09 + 0.05 * abs(sin(fi));
            let fade = smoothstep(0.85, 0.6, abs(y));
            cov = max(cov, fill(sd_circle(p - vec2<f32>(x, y), rr), aa) * fade);
        }
    } else if (id == 8) {
        // Cometa: arco que gira con estela.
        let q = rot2(-t * 2.0) * p;
        let ang = atan2(q.y, q.x);
        let ring = fill(abs(length(p) - 0.40) - 0.085, aa);
        let comet = clamp((ang + 3.14159) / 6.28318, 0.0, 1.0);
        cov = ring * comet;
    } else if (id == 9) {
        // Flor: rosa polar que late.
        let ang = atan2(p.y, p.x);
        let r = 0.20 + 0.26 * abs(cos(ang * 2.5 + t * 0.6));
        cov = fill(length(p) - r, aa);
    } else if (id == 10) {
        // Gusano: cadena de blobs fundidos.
        var d = 1000.0;
        for (var i: i32 = 0; i < 6; i = i + 1) {
            let fi = f32(i);
            let a = t * 1.5 - fi * 0.5;
            let c = vec2<f32>(cos(a), sin(a * 1.7)) * vec2<f32>(0.42, 0.34);
            d = smin(d, sd_circle(p - c, 0.15 - fi * 0.013), 0.18);
        }
        cov = fill(d, aa);
    } else if (id == 11) {
        // Lava: blobs que suben y bajan fundiendose.
        var d = 1000.0;
        for (var i: i32 = 0; i < 4; i = i + 1) {
            let fi = f32(i);
            let x = 0.40 * sin(fi * 2.1 + t * 0.3);
            let y = sin(t * 0.6 + fi * 1.5) * 0.45;
            d = smin(d, sd_circle(p - vec2<f32>(x, y), 0.17 + 0.04 * sin(t + fi)), 0.26);
        }
        cov = fill(d, aa);
    }
    return clamp(cov, 0.0, 1.0);
}

// --- ESCENAS 3D: campo de estrellas, sistema solar y atractores extraños ---

// Campo de estrellas tenue que titila.
fn star_field(uv: vec2<f32>, t: f32) -> f32 {
    let g = uv * vec2<f32>(64.0, 90.0);
    let cell = floor(g);
    let f = fract(g) - vec2<f32>(0.5);
    let rnd = hash21(cell);
    let d = length(f);
    let tw = 0.6 + 0.4 * sin(t * 2.0 + rnd * 40.0);
    return smoothstep(0.10, 0.0, d) * step(0.90, rnd) * tw;
}

fn planet_color(i: i32) -> vec3<f32> {
    if (i == 0) { return vec3<f32>(0.72, 0.62, 0.50); }      // rocoso
    else if (i == 1) { return vec3<f32>(0.40, 0.62, 1.00); } // oceanico
    else if (i == 2) { return vec3<f32>(0.92, 0.42, 0.22); } // rojo
    else if (i == 3) { return vec3<f32>(0.86, 0.76, 0.52); } // gaseoso (anillos)
    else { return vec3<f32>(0.50, 0.86, 0.86); }             // helado
}

// Sistema solar 3D: sol central + planetas en orbitas inclinadas (con test de
// profundidad para que pasen por delante/detras del sol), sobre estrellas.
fn solar(p: vec2<f32>, uv: vec2<f32>, t: f32) -> vec3<f32> {
    var col = vec3<f32>(0.01, 0.01, 0.03) + vec3<f32>(star_field(uv, t));
    let tilt = 0.46;
    let st = sin(tilt); let ct = cos(tilt);
    // Sol: resplandor + nucleo.
    let sd = length(p);
    col = col + vec3<f32>(1.0, 0.80, 0.35) * exp(-sd * 4.0) * 0.75;
    var zbuf = -100.0;
    let suncore = smoothstep(0.17, 0.13, sd);
    if (suncore > 0.0) {
        col = mix(col, vec3<f32>(1.0, 0.92, 0.55), suncore);
        zbuf = 0.0;
    }
    for (var i: i32 = 0; i < 5; i = i + 1) {
        let fi = f32(i);
        let r = 0.30 + fi * 0.135;
        let spd = 0.7 / pow(r, 1.5);              // mas lentos los exteriores (Kepler)
        let a = t * spd + fi * 1.7;
        let ox = cos(a) * r;
        let oz = sin(a) * r;
        let center2 = vec2<f32>(ox, -oz * st);     // proyeccion del plano inclinado
        let depth = oz * ct;                        // +depth = mas cerca del observador
        let persp = 1.0 / (1.0 - depth * 0.22);
        let c2 = center2 * persp;
        let pr = (0.05 + fi * 0.006) * persp;
        let pd = length(p - c2);
        let pc = smoothstep(pr, pr * 0.6, pd);
        if (pc > 0.0 && depth > zbuf) {
            let off = normalize(p - c2 + vec2<f32>(0.0001, 0.0001));
            let sundir = normalize(-c2 + vec2<f32>(0.0001, 0.0001));
            let lit = clamp(0.30 + 0.75 * dot(off, sundir), 0.22, 1.05);
            col = mix(col, planet_color(i) * lit, pc);
            zbuf = depth;
        }
    }
    return col;
}

// Derivada (campo vectorial) de cada atractor extraño.
fn attractor_deriv(id: i32, s: vec3<f32>) -> vec3<f32> {
    if (id == 0) {
        // Lorenz
        return vec3<f32>(10.0 * (s.y - s.x), s.x * (28.0 - s.z) - s.y, s.x * s.y - 2.6667 * s.z);
    } else if (id == 1) {
        // Aizawa
        let a = 0.95; let b = 0.7; let c = 0.6; let d = 3.5; let e = 0.25; let f = 0.1;
        return vec3<f32>(
            (s.z - b) * s.x - d * s.y,
            d * s.x + (s.z - b) * s.y,
            c + a * s.z - s.z * s.z * s.z / 3.0 - (s.x * s.x + s.y * s.y) * (1.0 + e * s.z) + f * s.z * s.x * s.x * s.x
        );
    } else if (id == 2) {
        // Halvorsen
        let a = 1.89;
        return vec3<f32>(
            -a * s.x - 4.0 * s.y - 4.0 * s.z - s.y * s.y,
            -a * s.y - 4.0 * s.z - 4.0 * s.x - s.z * s.z,
            -a * s.z - 4.0 * s.x - 4.0 * s.y - s.x * s.x
        );
    } else if (id == 3) {
        // Thomas
        let b = 0.19;
        return vec3<f32>(sin(s.y) - b * s.x, sin(s.z) - b * s.y, sin(s.x) - b * s.z);
    }
    // Rössler
    let a = 0.2; let b = 0.2; let c = 5.7;
    return vec3<f32>(-s.y - s.z, s.x + a * s.y, b + s.z * (s.x - c));
}

const ATTR_N: i32 = 520;

// Dibuja un atractor extraño TRAZANDOSE: un "cabezal" recorre la trayectoria (integrada en
// cada pixel) dejando estela, de modo que la curva se va GENERANDO y reinicia en ciclo. Gira
// lento para apreciar el 3D. El color es el de acento.
fn attractor_scene(aid: i32, p: vec2<f32>, uv: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.02, 0.02, 0.05) + vec3<f32>(star_field(uv, t)) * 0.5;
    var s = vec3<f32>(0.1, 0.0, 0.0);
    var dt = 0.0075; var scale = 0.045; var center = vec3<f32>(0.0, 0.0, 25.0);
    var warm: i32 = 60;
    if (aid == 1) { s = vec3<f32>(0.1, 0.0, 0.0); dt = 0.014; scale = 0.62; center = vec3<f32>(0.0, 0.0, 0.0); }
    else if (aid == 2) { s = vec3<f32>(-1.0, 0.0, 0.0); dt = 0.0075; scale = 0.085; center = vec3<f32>(-2.0, -2.0, -2.0); }
    else if (aid == 3) { s = vec3<f32>(0.5, 0.1, 0.0); dt = 0.06; scale = 0.22; center = vec3<f32>(0.0, 0.0, 0.0); }
    else if (aid == 4) { s = vec3<f32>(-6.0, 0.0, 0.0); dt = 0.013; scale = 0.085; center = vec3<f32>(0.0, 0.0, 3.0); warm = 360; }
    // Calentamiento: entrar en el atractor (descartar el transitorio; Rössler tarda mas).
    for (var w: i32 = 0; w < warm; w = w + 1) { s = s + attractor_deriv(aid, s) * dt; }
    // Progreso del trazado (ciclo): el cabezal avanza de 0 a N y reinicia.
    let prog = fract(t * 0.13);
    let head = prog * f32(ATTR_N);
    // Atenuacion suave al reiniciar el ciclo (entra/sale sin saltos bruscos).
    let cyc = smoothstep(0.0, 0.06, prog) * smoothstep(1.0, 0.92, prog);
    // Balanceo suave (no giro completo) para no colapsar la proyeccion de atractores planos.
    let ang = sin(t * 0.25) * 0.55;
    let ca = cos(ang); let sa = sin(ang);
    var glow = 0.0;
    var head_glow = 0.0;
    for (var i: i32 = 0; i < ATTR_N; i = i + 1) {
        s = s + attractor_deriv(aid, s) * dt;
        let q = (s - center) * scale;
        let x2 = q.x * ca + q.z * sa;     // giro lento alrededor de Y
        let proj = vec2<f32>(x2, q.y);
        let dd = dot(p - proj, p - proj);
        let fi = f32(i);
        // Solo lo ya recorrido por el cabezal es visible (la curva "crece").
        let drawn = smoothstep(head + 2.0, head - 2.0, fi);
        let behind = max(head - fi, 0.0);          // distancia detras del cabezal
        let comet = exp(-behind / 14.0);           // realce compacto que viaja con el cabezal
        let bright = 0.000038 / (dd + 0.0005);     // linea fina
        glow = glow + drawn * (0.6 + 1.5 * comet) * bright;
        head_glow = head_glow + drawn * comet * bright;
    }
    // Mapeo de tono: comprime los cruces de la curva para ver la ESTRUCTURA (no un borron
    // blanco). Cuerpo en el color de acento + nucleo del cabezal blanco.
    let att = accent * glow + vec3<f32>(1.0, 1.0, 1.0) * head_glow * 0.5;
    let mapped = vec3<f32>(1.0, 1.0, 1.0) - exp(-att * 1.5);
    return col + mapped * cyc;
}

// --- CARGADORES 3D: esferas proyectadas (perspectiva + sombreado + profundidad) ---

fn rot3(v: vec3<f32>, ay: f32, ax: f32) -> vec3<f32> {
    let cy = cos(ay); let sy = sin(ay);
    let v1 = vec3<f32>(v.x * cy + v.z * sy, v.y, -v.x * sy + v.z * cy);
    let cx = cos(ax); let sx = sin(ax);
    return vec3<f32>(v1.x, v1.y * cx - v1.z * sx, v1.y * sx + v1.z * cx);
}

// Dibuja una esfera 3D (con test de profundidad y sombreado difuso) en el punto P.
fn put_sphere(p: vec2<f32>, big: vec3<f32>, r: f32, color: vec3<f32>,
              col: ptr<function, vec3<f32>>, zbuf: ptr<function, f32>) {
    let persp = 1.0 / (1.0 - big.z * 0.18);
    let c2 = vec2<f32>(big.x, big.y) * persp;
    let rr = r * persp;
    let off = p - c2;
    let dd = dot(off, off);
    let cov = smoothstep(rr, rr * 0.55, length(off));
    if (cov > 0.0 && big.z > *zbuf) {
        let nz = sqrt(max(rr * rr - dd, 0.0)) / max(rr, 0.0001);
        let nrm = normalize(vec3<f32>(off / max(rr, 0.0001), nz));
        let lit = clamp(dot(nrm, normalize(vec3<f32>(-0.45, -0.6, 0.65))), 0.0, 1.0);
        let shaded = color * (0.22 + 0.9 * lit) + vec3<f32>(1.0) * pow(lit, 16.0) * 0.5;
        *col = mix(*col, shaded, cov);
        *zbuf = big.z;
    }
}

fn loader3d(id: i32, p: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.04, 0.04, 0.06);
    var zbuf = -1000.0;
    let ay = t * 0.7;
    if (id == 0) {
        // Atomo: tres anillos de esferas en planos distintos.
        for (var k: i32 = 0; k < 3; k = k + 1) {
            let fk = f32(k);
            for (var i: i32 = 0; i < 14; i = i + 1) {
                let a = f32(i) / 14.0 * 6.2832 + t * 1.6;
                var big = vec3<f32>(cos(a) * 0.64, sin(a) * 0.64, 0.0);
                big = rot3(big, fk * 1.05 + t * 0.2, fk * 0.7);
                put_sphere(p, big, 0.075, accent, &col, &zbuf);
            }
        }
    } else if (id == 1) {
        // Helice doble (ADN) girando.
        for (var i: i32 = 0; i < 26; i = i + 1) {
            let f = f32(i) / 26.0;
            let yy = (f - 0.5) * 1.5;
            let a = f * 9.0 + t * 2.0;
            var p0 = rot3(vec3<f32>(cos(a) * 0.42, yy, sin(a) * 0.42), ay, 0.18);
            var p1 = rot3(vec3<f32>(cos(a + 3.1416) * 0.42, yy, sin(a + 3.1416) * 0.42), ay, 0.18);
            put_sphere(p, p0, 0.06, accent, &col, &zbuf);
            put_sphere(p, p1, 0.06, accent * 0.6 + vec3<f32>(0.25, 0.25, 0.3), &col, &zbuf);
        }
    } else if (id == 2) {
        // Esfera de puntos (Fibonacci) girando.
        for (var i: i32 = 0; i < 42; i = i + 1) {
            let fi = f32(i);
            let yy = 1.0 - (fi / 41.0) * 2.0;
            let rad = sqrt(max(1.0 - yy * yy, 0.0));
            let phi = fi * 2.39996;
            var big = rot3(vec3<f32>(cos(phi) * rad, yy, sin(phi) * rad) * 0.72, ay, 0.5);
            put_sphere(p, big, 0.052, accent, &col, &zbuf);
        }
    } else if (id == 3) {
        // Anillo (toro) girando en 3D.
        for (var i: i32 = 0; i < 24; i = i + 1) {
            let a = f32(i) / 24.0 * 6.2832;
            var big = rot3(vec3<f32>(cos(a) * 0.72, sin(a) * 0.72, 0.0), t * 1.3, 0.95);
            put_sphere(p, big, 0.07, accent, &col, &zbuf);
        }
    } else if (id == 4) {
        // Cumulo esferico que late.
        for (var i: i32 = 0; i < 32; i = i + 1) {
            let fi = f32(i);
            let yy = 1.0 - (fi / 31.0) * 2.0;
            let rad = sqrt(max(1.0 - yy * yy, 0.0));
            let phi = fi * 2.39996;
            let pr = 0.5 + 0.28 * sin(t * 2.2 + fi * 0.35);
            var big = rot3(vec3<f32>(cos(phi) * rad, yy, sin(phi) * rad) * pr, ay, 0.4);
            put_sphere(p, big, 0.05, accent, &col, &zbuf);
        }
    } else {
        // Espiral conica 3D.
        for (var i: i32 = 0; i < 34; i = i + 1) {
            let f = f32(i) / 34.0;
            let a = f * 12.0 + t * 2.0;
            let rad = 0.12 + f * 0.6;
            var big = rot3(vec3<f32>(cos(a) * rad, (f - 0.5) * 1.35, sin(a) * rad), ay, 0.3);
            put_sphere(p, big, 0.062 * (1.0 - 0.4 * f), accent, &col, &zbuf);
        }
    }
    return col;
}

// ============== FIGURAS 3D: solidos raymarcheados (cubo, esfera, piramide, toro, octaedro) ==============

// Ruido de valor suave (a partir de hash21) para las texturas procedurales.
fn nz2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash21(i + vec2<f32>(0.0, 0.0)), hash21(i + vec2<f32>(1.0, 0.0)), u.x),
               mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), u.x), u.y);
}

// Ruido fractal (3 octavas) en 0..~0.94.
fn fbm2(p: vec2<f32>) -> f32 {
    return nz2(p) * 0.5 + nz2(p * 2.03 + vec2<f32>(1.7, 9.2)) * 0.25 + nz2(p * 4.01 + vec2<f32>(8.3, 2.8)) * 0.125;
}

// Voronoi (celulas): devuelve (distancia a la celda mas cercana, valor aleatorio de esa celda).
// Para el grano del cuero y materiales celulares.
fn vor(p: vec2<f32>) -> vec2<f32> {
    let n = floor(p);
    let f = fract(p);
    var md = 8.0;
    var mr = 0.0;
    for (var j: i32 = -1; j <= 1; j = j + 1) {
        for (var i: i32 = -1; i <= 1; i = i + 1) {
            let g = vec2<f32>(f32(i), f32(j));
            let o = vec2<f32>(hash21(n + g), hash21(n + g + vec2<f32>(31.0, 17.0)));
            let r = g + o - f;
            let d = dot(r, r);
            if (d < md) { md = d; mr = hash21(n + g + vec2<f32>(7.0, 3.0)); }
        }
    }
    return vec2<f32>(sqrt(md), mr);
}

// Color base CARACTERISTICO de cada material (para cuadernos de material entero).
fn material_color(tex: i32) -> vec3<f32> {
    if (tex == 1) { return vec3<f32>(0.42, 0.26, 0.16); } // cuero (marron)
    if (tex == 2) { return vec3<f32>(0.80, 0.74, 0.62); } // tela / lino (natural)
    if (tex == 3) { return vec3<f32>(0.52, 0.34, 0.18); } // madera (nogal)
    if (tex == 4) { return vec3<f32>(0.72, 0.55, 0.34); } // kraft
    if (tex == 5) { return vec3<f32>(0.10, 0.10, 0.12); } // fibra de carbono (oscuro)
    return vec3<f32>(0.88, 0.88, 0.86);                   // cuadros / otros (claro)
}

fn sd_box3(p: vec3<f32>, b: vec3<f32>) -> f32 {
    let q = abs(p) - b;
    return length(max(q, vec3<f32>(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}
fn sd_torus3(p: vec3<f32>, ra: f32, rb: f32) -> f32 {
    let q = vec2<f32>(length(p.xz) - ra, p.y);
    return length(q) - rb;
}
fn sd_octa3(p: vec3<f32>, s: f32) -> f32 {
    let q = abs(p);
    return (q.x + q.y + q.z - s) * 0.57735027;
}
fn sd_tetra3(p: vec3<f32>, s: f32) -> f32 {
    return (max(abs(p.x + p.y) - p.z, abs(p.x - p.y) + p.z) - s) * 0.57735027;
}
fn map_fig(id: i32, p: vec3<f32>) -> f32 {
    if (id == 1) { return length(p) - 0.82; }        // esfera
    if (id == 2) { return sd_tetra3(p, 1.05); }      // piramide (tetraedro)
    if (id == 3) { return sd_torus3(p, 0.6, 0.26); } // toro / dona
    if (id == 4) { return sd_octa3(p, 1.0); }        // octaedro
    return sd_box3(p, vec3<f32>(0.6, 0.6, 0.6));     // cubo (id 0)
}

// Textura procedural sobre la superficie (proyeccion triplanar). tex: 0 ninguna, 1 cuero,
// 2 tela/lino, 3 madera, 4 kraft, 5 fibra de carbono, 6 cuadros.
fn tex_surface(tex: i32, base: vec3<f32>, q: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    if (tex <= 0) { return base; }
    let an = abs(n);
    var uv2: vec2<f32>;
    if (an.x >= an.y && an.x >= an.z) { uv2 = q.yz; }
    else if (an.y >= an.z) { uv2 = q.xz; }
    else { uv2 = q.xy; }
    if (tex == 1) { // CUERO: celulas Voronoi (grano) + surcos + poros + variacion de tono
        let v = vor(uv2 * 6.0);
        let groove = smoothstep(0.0, 0.07, v.x);        // surcos oscuros entre celulas
        let pores = fbm2(uv2 * 26.0);
        var c = base * (0.82 + 0.30 * v.y);             // cada celula, tono ligeramente distinto
        c = c * (0.66 + 0.34 * groove);                 // hundir los surcos
        c = c * (0.92 + 0.12 * pores);                  // poros finos
        // leve brillo en el centro de las celulas
        c = c + vec3<f32>(0.05) * (1.0 - groove) * 0.5;
        return c;
    }
    if (tex == 2) { // TELA / LINO: hilos entrelazados (urdimbre/trama) con sombreado
        let s = uv2 * 30.0;
        let warp = 0.5 + 0.5 * sin(s.x * 6.2831);
        let weft = 0.5 + 0.5 * sin(s.y * 6.2831);
        let over = step(0.5, fract((floor(s.x) + floor(s.y)) * 0.5)); // alternancia del tejido
        let th = mix(warp, weft, over);
        let fuzz = 0.94 + 0.10 * fbm2(uv2 * 70.0);
        return base * (0.74 + 0.36 * th) * fuzz;
    }
    if (tex == 3) { // MADERA: anillos con domain warp + vetas finas + nudos
        let w = fbm2(uv2 * vec2<f32>(1.4, 3.2)) * 1.8;
        let rings = 0.5 + 0.5 * sin((uv2.x * 7.5 + w) * 6.2831);
        let grain = 0.86 + 0.14 * fbm2(uv2 * vec2<f32>(46.0, 9.0));
        return base * mix(0.66, 1.10, rings) * grain;
    }
    if (tex == 4) { // KRAFT: fibra de papel + motas
        let fib = fbm2(uv2 * 30.0);
        let fleck = step(0.94, hash21(floor(uv2 * 90.0))) * 0.12;
        return base * (0.90 + 0.16 * fib) - vec3<f32>(fleck);
    }
    if (tex == 5) { // FIBRA DE CARBONO: sarga 2x2 con brillo direccional
        let s = uv2 * 11.0;
        let blk = floor(s);
        let diag = fract((blk.x - blk.y) * 0.5);                 // direccion de la sarga
        let dirv = select(fract(s.y), fract(s.x), diag < 0.5);
        let sheen = 0.35 + 0.65 * pow(0.5 + 0.5 * sin(dirv * 6.2831), 2.0);
        return base * (0.5 + 1.0 * sheen);
    }
    // tex == 6: CUADROS (tablero)
    let g = floor(uv2 * 6.0);
    let c = abs(fract((g.x + g.y) * 0.5) * 2.0 - 1.0);
    return mix(base * 0.55, base, 1.0 - c);
}

// Raymarcher de una figura solida que gira, con luz fija en el mundo y textura opcional.
fn figure3d(id: i32, p2: vec2<f32>, t: f32, accent: vec3<f32>, tex: i32) -> vec3<f32> {
    var col = vec3<f32>(0.035, 0.04, 0.055);
    let ro = vec3<f32>(0.0, 0.0, -3.0);
    let rd = normalize(vec3<f32>(p2, 1.7));
    let ay = t * 0.55;
    let ax = t * 0.32 + 0.5;
    var tt = 0.4;
    var hit = false;
    var pr = vec3<f32>(0.0);
    for (var i: i32 = 0; i < 72; i = i + 1) {
        let pos = ro + rd * tt;
        pr = rot3(pos, ay, ax); // marchamos en el espacio de la figura (que gira con el tiempo)
        let d = map_fig(id, pr);
        if (d < 0.0015) { hit = true; break; }
        tt = tt + d;
        if (tt > 7.0) { break; }
    }
    if (hit) {
        let e = vec2<f32>(0.0018, 0.0);
        let n = normalize(vec3<f32>(
            map_fig(id, pr + e.xyy) - map_fig(id, pr - e.xyy),
            map_fig(id, pr + e.yxy) - map_fig(id, pr - e.yxy),
            map_fig(id, pr + e.yyx) - map_fig(id, pr - e.yyx)
        ));
        // luz fija en el mundo: la rotamos al espacio objeto para iluminar las caras al girar.
        let lw = normalize(vec3<f32>(-0.5, 0.72, -0.55));
        let ldir = rot3(lw, ay, ax);
        let rdo = rot3(rd, ay, ax);
        let dif = clamp(dot(n, ldir), 0.0, 1.0);
        let spec = pow(clamp(dot(reflect(rdo, n), ldir), 0.0, 1.0), 26.0);
        let rim = pow(1.0 - clamp(dot(n, -rdo), 0.0, 1.0), 3.0) * 0.35;
        var base = accent;
        if (tex > 0) { base = material_tri(tex, pr, n); } // material realista/diamante sobre la figura
        col = base * (0.22 + 0.95 * dif) + vec3<f32>(1.0) * spec * 0.6 + accent * rim;
    }
    return col;
}

// ============== ARCADE Y DEMOS: animaciones retrowave / arcade / generativas ==============

fn grid_line(x: f32, w: f32) -> f32 {
    let g = min(fract(x), 1.0 - fract(x));
    return 1.0 - smoothstep(0.0, w, g);
}

// Retrowave: cielo con sol de franjas + rejilla en perspectiva que avanza hacia el espectador.
fn fx_retrowave(uv: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    let horizon = 0.52;
    var col: vec3<f32>;
    if (uv.y < horizon) {
        col = mix(vec3<f32>(0.06, 0.02, 0.16), vec3<f32>(0.40, 0.07, 0.34), uv.y / horizon);
        col = col + vec3<f32>(star_field(uv * vec2<f32>(1.0, 2.2), t)) * 0.5 * (1.0 - uv.y / horizon);
        let sc = vec2<f32>(0.5, horizon - 0.02);
        let d = distance(uv, sc);
        let g = clamp((uv.y - (horizon - 0.26)) / 0.26, 0.0, 1.0);
        let suncol = mix(vec3<f32>(1.0, 0.86, 0.2), vec3<f32>(1.0, 0.2, 0.5), g);
        let sun = smoothstep(0.215, 0.205, d);
        let stripe = step(0.0, sin((uv.y - horizon) * 150.0));
        let mask = select(1.0, stripe, uv.y > sc.y);
        col = mix(col, suncol, sun * mask);
    } else {
        col = vec3<f32>(0.03, 0.01, 0.06);
        let fy = (uv.y - horizon) / (1.0 - horizon);
        let z = 0.18 / (fy + 0.02);
        let wx = (uv.x - 0.5) * z * 2.4;
        let lh = grid_line(z * 3.0 - t * 2.2, 0.05);
        let lv = grid_line(wx, 0.045);
        let grid = max(lh, lv) * clamp(fy * 1.6, 0.0, 1.0);
        let neon = mix(vec3<f32>(0.95, 0.1, 0.7), mix(vec3<f32>(0.1, 0.85, 1.0), accent, 0.4), fy);
        col = col + neon * grid;
    }
    return col;
}

// Hiperespacio: estrellas que se alargan en rayos radiales (viaje a supervelocidad).
fn fx_warp(p: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.01, 0.01, 0.03);
    let ang = atan2(p.y, p.x);
    let rad = length(p);
    let spokes = 80.0;
    let a01 = ang / 6.28318 + 0.5;
    let cell = floor(a01 * spokes);
    let rnd = hash21(vec2<f32>(cell, 3.0));
    let rnd2 = hash21(vec2<f32>(cell, 9.0));
    let speed = 0.5 + rnd * 1.3;
    let head = fract(rnd2 + t * speed);
    let len = 0.2 + rnd * 0.45;
    let along = smoothstep(head, head - len, rad) * step(rad, head + 0.02);
    let acen = abs(fract(a01 * spokes) - 0.5);
    let line = smoothstep(0.5, 0.12, acen);
    let star = along * line * smoothstep(0.0, 0.55, rad);
    let tint = mix(vec3<f32>(0.8, 0.9, 1.0), accent, 0.45);
    col = col + tint * star * 1.4 + tint * smoothstep(0.14, 0.0, rad) * 0.35;
    return col;
}

// Vortice: espiral que gira y absorbe hacia el centro.
fn fx_vortex(p: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    let ang = atan2(p.y, p.x);
    let rad = length(p);
    let sw = sin(4.0 * ang + 14.0 * log(rad + 0.06) - t * 3.0);
    let band = smoothstep(0.0, 0.85, sw);
    let fall = smoothstep(1.15, 0.0, rad);
    var col = vec3<f32>(0.02, 0.01, 0.04);
    let hue = fract(rad * 1.2 - t * 0.1);
    col = col + mix(accent, hue2rgb(hue), 0.5) * band * fall;
    col = col + accent * smoothstep(0.12, 0.0, rad) * 0.6;
    return col;
}

// Sprite 8x8 de un "invasor" (dos fotogramas para la animacion de patas).
fn invader_bit(lx: i32, ly: i32, frame: i32) -> f32 {
    if (lx < 0 || lx > 7 || ly < 0 || ly > 7) { return 0.0; }
    var row: i32 = 0;
    if (frame == 0) {
        var ra = array<i32, 8>(0x18, 0x3C, 0x7E, 0xDB, 0xFF, 0x5A, 0x81, 0x42);
        row = ra[ly];
    } else {
        var rb = array<i32, 8>(0x18, 0x3C, 0x7E, 0xDB, 0xFF, 0x24, 0x42, 0xA5);
        row = rb[ly];
    }
    return f32((row >> u32(7 - lx)) & 1);
}

// Space Invaders: formacion de invasores que se mueve + canon + bala.
fn fx_invaders(uv: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.02, 0.02, 0.04);
    let frame = i32(floor(t * 2.0)) & 1;
    let sway = sin(t) * 0.10;
    let descend = fract(t * 0.04) * 0.12;
    let ax = (uv.x - 0.12 - sway) / 0.76;
    let ay = (uv.y - 0.10 - descend) / 0.46;
    if (ax > 0.0 && ax < 1.0 && ay > 0.0 && ay < 1.0) {
        let cx = ax * 5.0;
        let cy = ay * 3.0;
        let lx = i32(floor(fract(cx) * 8.0));
        let ly = i32(floor(fract(cy) * 8.0));
        let acol = mix(accent, vec3<f32>(0.2, 1.0, 0.45), 0.45);
        col = col + acol * invader_bit(lx, ly, frame);
    }
    let cannonx = 0.5 + sin(t * 1.3) * 0.32;
    let base = step(abs(uv.x - cannonx), 0.05) * step(abs(uv.y - 0.93), 0.018);
    let barrel = step(abs(uv.x - cannonx), 0.012) * step(abs(uv.y - 0.90), 0.02);
    col = col + vec3<f32>(0.9, 0.95, 1.0) * clamp(base + barrel, 0.0, 1.0);
    let by = 0.9 - fract(t * 0.7) * 0.78;
    col = col + vec3<f32>(1.0, 1.0, 0.6) * step(abs(uv.x - cannonx), 0.006) * step(abs(uv.y - by), 0.03);
    return col;
}

// Tetris: el pozo se llena de bloques de colores y se reinicia (con una pieza cayendo).
fn fx_tetris(uv: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.03, 0.03, 0.05);
    let w = 8.0; let h = 14.0;
    let gx = floor(uv.x * w);
    let gy = floor(uv.y * h);
    let cycle = 11.0;
    let prog = fract(t / cycle);
    let epoch = floor(t / cycle);
    let top = h - prog * h + (hash21(vec2<f32>(gx, epoch)) - 0.5) * 2.0; // borde superior irregular
    var filled = select(0.0, 1.0, gy > top);
    // Pieza cayendo (2 celdas) en una columna pseudo-aleatoria.
    let pcol = floor(hash21(vec2<f32>(floor(t * 1.5), epoch)) * w);
    let py = fract(t * 1.5) * (top + 1.0);
    if (abs(gx - pcol) < 0.5 && gy >= floor(py) && gy <= floor(py) + 1.0) { filled = 1.0; }
    if (filled > 0.5) {
        let bcol = hue2rgb(fract(hash21(vec2<f32>(gx, gy)) + 0.05));
        let lx = fract(uv.x * w); let ly = fract(uv.y * h);
        let edge = step(0.1, lx) * step(lx, 0.9) * step(0.1, ly) * step(ly, 0.9);
        col = mix(bcol * 0.45, bcol, edge);
    }
    return col;
}

fn life_seed(idx: i32, n: i32, epoch: f32) -> i32 {
    let x = f32(idx % n);
    let y = f32(idx / n);
    return select(0, 1, hash21(vec2<f32>(x + epoch * 13.0, y + epoch * 7.0)) > 0.56);
}

// Juego de la Vida de Conway: se siembra y evoluciona unas generaciones; luego reinicia.
fn fx_life(uv: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    let n = 10;
    let maxgen = 8.0;
    let g = t * 3.0;
    let epoch = floor(g / maxgen);
    let gen = i32(floor(g % maxgen));
    var cur = array<i32, 100>();
    for (var i = 0; i < 100; i = i + 1) { cur[i] = life_seed(i, n, epoch); }
    for (var s = 0; s < gen; s = s + 1) {
        var nxt = array<i32, 100>();
        for (var y = 0; y < n; y = y + 1) {
            for (var x = 0; x < n; x = x + 1) {
                var cnt = 0;
                for (var dy = -1; dy <= 1; dy = dy + 1) {
                    for (var dx = -1; dx <= 1; dx = dx + 1) {
                        if (dx == 0 && dy == 0) { continue; }
                        let xx = (x + dx + n) % n;
                        let yy = (y + dy + n) % n;
                        cnt = cnt + cur[yy * n + xx];
                    }
                }
                let alive = cur[y * n + x];
                var nv = 0;
                if (alive == 1 && (cnt == 2 || cnt == 3)) { nv = 1; }
                if (alive == 0 && cnt == 3) { nv = 1; }
                nxt[y * n + x] = nv;
            }
        }
        cur = nxt;
    }
    let cx = i32(floor(uv.x * f32(n)));
    let cy = i32(floor(uv.y * f32(n)));
    let idx = clamp(cy * n + cx, 0, 99);
    var col = vec3<f32>(0.03, 0.04, 0.05);
    if (cur[idx] == 1) {
        let lx = fract(uv.x * f32(n));
        let ly = fract(uv.y * f32(n));
        let cell = smoothstep(0.08, 0.18, lx) * smoothstep(0.08, 0.18, 1.0 - lx)
                 * smoothstep(0.08, 0.18, ly) * smoothstep(0.08, 0.18, 1.0 - ly);
        col = mix(col, accent, cell);
    }
    return col;
}

// Flores creciendo: petalos (rosa polar) que florecen y se reinician, en varias fases.
fn fx_flowers(p: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.02, 0.03, 0.02);
    for (var i = 0; i < 3; i = i + 1) {
        let fi = f32(i);
        let center = vec2<f32>((fi - 1.0) * 0.55, 0.18 * sin(fi * 2.0));
        let q = p - center;
        let ang = atan2(q.y, q.x);
        let rad = length(q);
        let bloom = fract(t * 0.2 + fi * 0.33);
        let pr = (0.16 + 0.18 * abs(cos(ang * 3.0))) * smoothstep(0.0, 0.35, bloom) * (0.45 + 0.55 * bloom);
        let pet = smoothstep(0.02, -0.02, rad - pr);
        let pcol = mix(accent, vec3<f32>(1.0, 0.82, 0.25), 0.5 + 0.5 * sin(fi * 2.0));
        col = mix(col, pcol, pet);
        col = mix(col, vec3<f32>(1.0, 0.85, 0.25), smoothstep(0.045, 0.0, rad) * step(0.12, bloom));
    }
    return col;
}

// Pac-Man: se mueve comiendo puntos (boca que abre/cierra) con un fantasma detras.
fn fx_pacman(uv: vec2<f32>, t: f32, aspect: f32, accent: vec3<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.02, 0.02, 0.06);
    let y = 0.5;
    let px = fract(t * 0.22) * 1.3 - 0.15;
    let dotrow = step(abs(uv.y - y), 0.02) * step(0.4, fract(uv.x * 12.0)) * step(fract(uv.x * 12.0), 0.6);
    col = col + vec3<f32>(1.0, 1.0, 0.7) * dotrow * (1.0 - step(uv.x, px));
    let pd = vec2<f32>(uv.x - px, (uv.y - y) * aspect);
    let pang = abs(atan2(pd.y, pd.x));
    let mouth = 0.15 + 0.4 * abs(sin(t * 8.0));
    col = mix(col, vec3<f32>(1.0, 0.9, 0.1), step(length(pd), 0.07) * step(mouth, pang));
    let gx = px - 0.2;
    let gd = vec2<f32>(uv.x - gx, (uv.y - y) * aspect);
    let dome = step(length(vec2<f32>(gd.x, min(gd.y, 0.0))), 0.055) * step(abs(gd.x), 0.055) * step(gd.y, 0.05) * step(-0.06, gd.y);
    col = mix(col, vec3<f32>(1.0, 0.35, 0.35), clamp(dome, 0.0, 1.0));
    return col;
}

// Relampagos realistas: rayos ramificados que caen y destellan iluminando el cielo tormentoso.
fn fx_lightning(uv: vec2<f32>, t: f32, accent: vec3<f32>) -> vec3<f32> {
    var col = mix(vec3<f32>(0.02, 0.02, 0.05), vec3<f32>(0.06, 0.06, 0.11), uv.y);
    // Nubes oscuras arremolinadas arriba.
    col = col + vec3<f32>(0.04, 0.04, 0.07) * smoothstep(0.45, 0.0, uv.y) * (0.5 + 0.5 * sin(uv.x * 8.0 + t * 0.6));
    let cyc = 1.1;
    let strike = floor(t / cyc);
    let lt = fract(t / cyc) * cyc;
    let seed = hash21(vec2<f32>(strike, 1.0));
    let seed2 = hash21(vec2<f32>(strike, 7.0));
    let flash = exp(-lt * 7.0); // destello: aparece de golpe y decae rapido
    let bx = 0.2 + 0.6 * seed;
    let lightcol = mix(vec3<f32>(0.75, 0.85, 1.0), accent, 0.25);
    var core = 0.0;
    var glow = 0.0;
    // Canal principal (zigzag), se ensancha al bajar.
    let n = sin(uv.y * 23.0 + seed * 50.0) + 0.5 * sin(uv.y * 57.0 + seed2 * 20.0) + 0.7 * sin(uv.y * 11.0);
    let px = bx + n * 0.035 * (0.3 + uv.y);
    let d = abs(uv.x - px);
    let on = step(uv.y, 0.98);
    core = core + smoothstep(0.012, 0.0, d) * on;
    glow = glow + smoothstep(0.10, 0.0, d) * on;
    // Rama secundaria desde un punto medio.
    let yb = 0.35 + 0.25 * seed2;
    if (uv.y > yb) {
        let nn = sin(uv.y * 40.0 + seed * 30.0) + 0.6 * sin(uv.y * 19.0);
        let px2 = bx + (uv.y - yb) * (0.5 * (seed - 0.5)) + nn * 0.03 * (uv.y - yb);
        let d2 = abs(uv.x - px2);
        core = core + smoothstep(0.009, 0.0, d2) * step(uv.y, 0.9) * 0.85;
        glow = glow + smoothstep(0.07, 0.0, d2) * 0.7;
    }
    col = col + lightcol * core * (0.4 + flash);
    col = col + lightcol * glow * (0.12 + 0.6 * flash);
    col = col + lightcol * flash * 0.18; // el cielo se ilumina con el destello
    return col;
}

fn arcade(id: i32, p: vec2<f32>, uv: vec2<f32>, t: f32, aspect: f32, accent: vec3<f32>) -> vec3<f32> {
    if (id == 0) { return fx_retrowave(uv, t, accent); }
    else if (id == 1) { return fx_warp(p, t, accent); }
    else if (id == 2) { return fx_vortex(p, t, accent); }
    else if (id == 3) { return fx_invaders(uv, t, accent); }
    else if (id == 4) { return fx_tetris(uv, t, accent); }
    else if (id == 5) { return fx_life(uv, t, accent); }
    else if (id == 6) { return fx_flowers(p, t, accent); }
    else if (id == 7) { return fx_pacman(uv, t, aspect, accent); }
    return fx_lightning(uv, t, accent);
}

// Color de la PORTADA (cara frontal) del cuaderno: el diseño/animacion elegido + capas.
// Lineas paralelas a un angulo `ang` para los papeles iso/triangular. `n` = celdas de ancho;
// se corrige por `asp` (alto/ancho) para que las celdas salgan casi cuadradas.
fn paper_dir(uv: vec2<f32>, asp: f32, ang: f32, n: f32) -> f32 {
    let c = vec2<f32>(uv.x * n, uv.y * n * asp);
    let d = vec2<f32>(cos(ang), sin(ang));
    let proj = dot(c, d);
    return smoothstep(0.45, 0.5, abs(fract(proj) - 0.5));
}

fn cover_color(in: VsOut) -> vec3<f32> {
    let h = in.hover;
    let t = in.time;
    let inten = in.intensity;
    let fin = i32(round(in.finish));
    var col = in.base;

    // Reflejo (glare) comun: highlight radial centrado en el cursor.
    let gd = distance(in.uv, in.pointer);
    let glare = smoothstep(0.55, 0.0, gd) * 0.45 * h;

    if (fin >= 600) {
        // ---------------- FIGURAS 3D: solido raymarcheado que gira (con textura opcional) ----------------
        var p = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
        p.y = p.y * in.aspect;
        col = figure3d(fin - 600, p, t, in.accent, i32(round(in.texture)));
    } else if (fin >= 500) {
        // ---------------- NOTA RAPIDA: hoja de papel que refleja la cuadricula REAL ----------------
        // kind: 0 rayas, 1 milimetrado, 2 puntos, 3 blanca (sin cuadricula), 4 iso, 5 triangular.
        let kind = fin - 500;
        let paper = vec3<f32>(0.97, 0.97, 0.95); // casi blanco
        let ink = vec3<f32>(0.55, 0.64, 0.78);   // azul claro de cuaderno
        col = paper;
        let rows = 13.0;
        if (kind == 0 || kind == 1) {
            // rayas horizontales
            let dy = abs(fract(in.uv.y * rows) - 0.5);
            col = mix(col, ink, smoothstep(0.45, 0.5, dy) * 0.55);
        }
        if (kind == 1) {
            // verticales (milimetrado); ~9 columnas para que salgan casi cuadradas
            let dx = abs(fract(in.uv.x * 9.0) - 0.5);
            col = mix(col, ink, smoothstep(0.45, 0.5, dx) * 0.55);
        }
        if (kind == 2) {
            // puntos
            let f = fract(in.uv * vec2<f32>(9.0, rows)) - vec2<f32>(0.5, 0.5);
            col = mix(col, ink, smoothstep(0.18, 0.10, length(f)) * 0.6);
        }
        if (kind == 4) {
            // isometrica: lineas a +30, -30 y verticales
            let l = max(max(paper_dir(in.uv, in.aspect, 0.5236, 9.0), paper_dir(in.uv, in.aspect, -0.5236, 9.0)), paper_dir(in.uv, in.aspect, 1.5708, 9.0));
            col = mix(col, ink, l * 0.5);
        }
        if (kind == 5) {
            // triangular: tres direcciones (0, +60, -60 grados)
            let l = max(max(paper_dir(in.uv, in.aspect, 0.0, 9.0), paper_dir(in.uv, in.aspect, 1.0472, 9.0)), paper_dir(in.uv, in.aspect, -1.0472, 9.0));
            col = mix(col, ink, l * 0.5);
        }
        if (kind == 0 || kind == 1) {
            // margen rojo a la izquierda (estilo cuaderno)
            col = mix(col, vec3<f32>(0.86, 0.45, 0.45), smoothstep(0.012, 0.005, abs(in.uv.x - 0.12)) * 0.55);
        }
        // kind == 3 (blanca): sin patron, queda el papel casi blanco.
    } else if (fin >= 400) {
        // ---------------- ARCADE Y DEMOS (retrowave / arcade / generativas) ----------------
        var p = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
        p.y = p.y * in.aspect;
        col = arcade(fin - 400, p, in.uv, t, in.aspect, in.accent);
    } else if (fin >= 300) {
        // ---------------- CARGADORES 3D (esferas en perspectiva) ----------------
        var p = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
        p.y = p.y * in.aspect;
        col = loader3d(fin - 300, p, t, in.accent);
    } else if (fin >= 200) {
        // ---------------- ESCENAS 3D: sistema solar / atractores ----------------
        var p = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
        p.y = p.y * in.aspect;
        if (fin == 200) {
            col = solar(p, in.uv, t);
        } else {
            col = attractor_scene(fin - 210, p, in.uv, t, in.accent);
        }
    } else if (fin >= 100) {
        // ---------------- CARGADOR ORGANICO (B&N / acento) animado ----------------
        var p = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
        p.y = p.y * in.aspect;
        let cov = loader_cov(fin - 100, p, t);
        col = mix(in.base, in.accent, cov);
        col = col + in.accent * cov * 0.08; // leve halo
    } else {
        // MATERIAL del cuaderno: si hay textura, la tapa toma el COLOR del material (cuero/tela/
        // madera/kraft/carbono) + su grano realista; el foil/efecto se anade encima. Asi, Mate +
        // textura = un cuaderno de material puro (sin diseño de carátula).
        let tex = i32(round(in.texture));
        if (tex > 0) {
            // MATERIAL realista (textura CC0 tileada) o diamante procedural; el foil va encima.
            col = material_at(tex, in.uv * vec2<f32>(1.0, in.aspect));
        }
        // ---------------- FOIL: el efecto vive a reposo (idle) y crece con el cursor ----------------
        let eff = (0.30 + 0.70 * h) * (0.6 + 0.4 * inten);
        if (fin == 1) {
            // Holografico: arcoiris diagonal con bandas de foil.
            let hue = fract((in.uv.x + in.uv.y) * 2.0 + in.pointer.x * 0.6 - in.pointer.y * 0.4 + t * 0.04);
            let band = 0.5 + 0.5 * sin((in.uv.x - in.uv.y) * 42.0 + in.pointer.x * 9.0 + t * 1.2);
            col = col + hue2rgb(hue) * eff * band;
        } else if (fin == 2) {
            // Galaxia: nebulosa + estrellas.
            let hue = fract(in.uv.y * 1.2 + in.pointer.x * 0.5 + sin(in.uv.x * 6.0 + t * 0.3) * 0.1);
            col = col + hue2rgb(hue) * (eff * 0.5);
            let st = sparkles(in.uv, in.pointer, t, 26.0, 0.80);
            col = col + vec3<f32>(0.9, 0.95, 1.0) * st * (0.4 + 0.6 * h);
        } else if (fin == 3) {
            // Oro: bandas doradas metalicas.
            let band = 0.5 + 0.5 * sin((in.uv.x + in.uv.y) * 30.0 - in.pointer.x * 10.0 + t * 1.0);
            col = col + vec3<f32>(1.0, 0.82, 0.35) * eff * band;
        } else if (fin == 4) {
            // Prisma: arcoiris vertical fino que se desplaza.
            let hue = fract(in.uv.x * 3.0 + in.pointer.y * 0.8 + t * 0.06);
            let band = 0.5 + 0.5 * sin(in.uv.x * 80.0 + t * 0.8);
            col = col + hue2rgb(hue) * eff * band;
        } else if (fin == 5) {
            // Destellos: glitter denso que titila.
            let s1 = sparkles(in.uv, in.pointer, t, 34.0, 0.72);
            let s2 = sparkles(in.uv + vec2<f32>(0.5, 0.5), in.pointer * 1.3, t, 22.0, 0.78);
            col = col + vec3<f32>(1.0, 1.0, 0.95) * (s1 + s2) * (0.3 + 0.7 * h);
        } else if (fin == 6) {
            // Aurora: ondas verde-rosa que suben.
            let w = sin(in.uv.x * 6.0 + t * 0.8) * 0.1;
            let band = 0.5 + 0.5 * sin((in.uv.y + w) * 10.0 + t * 1.0);
            let aur = mix(vec3<f32>(0.1, 0.9, 0.5), vec3<f32>(0.8, 0.2, 0.9), 0.5 + 0.5 * sin(in.uv.y * 3.0 + t * 0.5));
            col = col + aur * eff * band;
        } else if (fin == 7) {
            // Neon: rayas cian-magenta que brillan.
            let stripe = 0.5 + 0.5 * sin(in.uv.x * 22.0 - t * 1.5);
            let neon = mix(vec3<f32>(0.0, 0.9, 1.0), vec3<f32>(1.0, 0.1, 0.8), 0.5 + 0.5 * sin(in.uv.y * 5.0 + t));
            col = col + neon * eff * stripe;
        } else if (fin == 8) {
            // Esmeralda: bandas verdes de foil.
            let band = 0.5 + 0.5 * sin((in.uv.x - in.uv.y) * 34.0 + in.pointer.x * 6.0 + t * 1.0);
            col = col + vec3<f32>(0.15, 0.95, 0.55) * eff * band;
        } else if (fin == 9) {
            // Rubi: bandas rojas de foil.
            let band = 0.5 + 0.5 * sin((in.uv.x + in.uv.y) * 34.0 - in.pointer.y * 6.0 + t * 1.0);
            col = col + vec3<f32>(1.0, 0.18, 0.30) * eff * band;
        } else if (fin == 10) {
            // Cromo: reflejo metalico que se desplaza (escala de grises).
            let m = 0.5 + 0.5 * sin((in.uv.y + in.pointer.x * 0.4) * 8.0 + t * 1.2);
            let chrome = mix(vec3<f32>(0.35, 0.38, 0.45), vec3<f32>(0.95, 0.97, 1.0), m);
            col = mix(col, chrome, eff * 0.9);
        } else if (fin == 11) {
            // Atardecer: degradado vertical naranja-rosa-violeta.
            let g = clamp(in.uv.y + 0.1 * sin(t * 0.4), 0.0, 1.0);
            let sky = mix(mix(vec3<f32>(1.0, 0.55, 0.2), vec3<f32>(0.95, 0.25, 0.45), g), vec3<f32>(0.3, 0.15, 0.5), g * g);
            col = col + sky * eff;
        }
        // fin == 0 (MATE): solo color base + glare.
    }

    // ---------------- CAPAS COMBINABLES (fx) sobre CUALQUIER diseño ----------------
    let fx = i32(round(in.fx));
    let fxamt = (0.4 + 0.6 * h) * inten;
    if ((fx & 1) != 0) {
        // Destellos.
        let s = sparkles(in.uv, in.pointer, t, 32.0, 0.74)
              + sparkles(in.uv + vec2<f32>(0.5, 0.5), in.pointer, t, 22.0, 0.80);
        col = col + vec3<f32>(1.0, 1.0, 0.96) * s * fxamt;
    }
    if ((fx & 2) != 0) {
        // Brillo animado: barrido diagonal que recorre la carta.
        let sw = fract((in.uv.x + in.uv.y) * 0.5 - t * 0.18);
        let band = smoothstep(0.045, 0.0, abs(sw - 0.5));
        col = col + vec3<f32>(1.0, 1.0, 1.0) * band * fxamt * 0.8;
    }
    if ((fx & 4) != 0) {
        // Resplandor: latido suave en el color de acento.
        let pulse = 0.5 + 0.5 * sin(t * 2.2);
        col = col + in.accent * (0.16 * pulse * inten) * (1.0 - gd * 0.8);
    }

    col = col + vec3<f32>(glare, glare, glare);
    return clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let face = i32(round(in.face));
    let shp = i32(round(in.shape));
    var col: vec3<f32>;
    // NOTA RAPIDA (hoja, finish 500..599): la portada lleva el patron; el resto, papel claro.
    // (Las figuras 3D, >= 600, son cuadernos normales y NO entran aqui.)
    let fin_sc = i32(round(in.finish));
    if (fin_sc >= 500 && fin_sc < 600) {
        if (face == 0) {
            col = cover_color(in);
        } else {
            col = vec3<f32>(0.93, 0.93, 0.91);
        }
        var shs = 1.0;
        if (face == 5) { shs = 0.9; } else if (face == 4) { shs = 1.05; }
        return vec4<f32>(clamp(col * shs, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
    }
    if (face == 0) {
        // Portada: el diseño/animacion elegido.
        col = cover_color(in);
        // TAPA DURA: ranura de bisagra (francesa) junto al lomo.
        if (shp == 1) {
            let groove = smoothstep(0.012, 0.0, abs(in.uv.x - (0.04 + in.overhang)));
            col = col * (1.0 - 0.45 * groove);
        }
        // MOLESKINE: cincha elastica (banda oscura vertical cerca del borde derecho).
        if (shp == 2) {
            let elastic = smoothstep(0.022, 0.0, abs(in.uv.x - 0.84));
            col = mix(col, vec3<f32>(0.11, 0.10, 0.13), elastic * 0.85);
        }
    } else if (face == 1) {
        // Reverso: si hay TEXTURA, el material envuelve TODO el cuaderno (cuaderno real); si no,
        // carton oscuro. Encima, una "cinta de masquin" beige para el nombre (lo dibuja la UI).
        let tex1 = select(0, i32(round(in.texture)), i32(round(in.finish)) < 100);
        if (tex1 > 0) {
            col = material_at(tex1, in.uv * vec2<f32>(1.0, in.aspect));
        } else {
            col = vec3<f32>(0.15, 0.13, 0.12);
        }
        let d = abs(in.uv - vec2<f32>(0.5, 0.5));
        let tape = step(d.x, 0.40) * step(d.y, 0.15);
        let tcol = vec3<f32>(0.86, 0.80, 0.62) * (0.96 + 0.04 * sin(in.uv.x * 70.0));
        col = mix(col, tcol, tape);
    } else if (face == 3) {
        // LOMO (encuadernacion). Con textura, el lomo tambien es del material.
        let tex3 = select(0, i32(round(in.texture)), i32(round(in.finish)) < 100);
        if (tex3 > 0 && shp != 3) {
            col = material_at(tex3, in.uv * vec2<f32>(1.0, in.aspect));
        } else if (shp == 3) {
            // ESPIRAL: anillos metalicos a lo largo del lomo (uv.x recorre el largo del lomo).
            let rings = grid_line(in.uv.x * 22.0, 0.32);
            let metal = mix(vec3<f32>(0.32, 0.34, 0.38), vec3<f32>(0.88, 0.91, 0.96), rings);
            col = mix(vec3<f32>(0.05, 0.05, 0.06), metal, rings);
        } else {
            let sheen = smoothstep(0.55, 0.0, abs(in.pz01 - 0.5));
            col = mix(vec3<f32>(0.10, 0.09, 0.12), vec3<f32>(0.27, 0.25, 0.30), sheen * 0.85);
            // TAPA DURA: nervios (cords) del lomo.
            if (shp == 1) {
                let c1 = smoothstep(0.02, 0.0, abs(in.uv.x - 0.18));
                let c2 = smoothstep(0.02, 0.0, abs(in.uv.x - 0.82));
                col = col + vec3<f32>(0.09, 0.09, 0.10) * (c1 + c2);
            }
        }
    } else {
        // Cantos (2,4,5): TABLA de tapa en los extremos del grosor + pila de HOJAS en el centro.
        let bf = in.board;
        let inboard = (in.pz01 < bf) || (in.pz01 > (1.0 - bf));
        let paper = vec3<f32>(0.93, 0.91, 0.85);
        let lines = grid_line(in.pz01 * 46.0, 0.25);
        let pages = mix(paper, paper * 0.6, lines * 0.55);
        col = mix(pages, vec3<f32>(0.13, 0.12, 0.14), select(0.0, 1.0, inboard));
    }
    // Sombreado por cara para dar sensacion de volumen 3D.
    var shade = 1.0;
    if (face == 1) { shade = 0.92; }
    else if (face == 2) { shade = 0.84; }
    else if (face == 3) { shade = 1.0; }   // lomo (color propio)
    else if (face == 4) { shade = 1.08; }
    else if (face == 5) { shade = 0.66; }
    col = col * shade;
    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
