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
    out.fx = in.fx;
    out.intensity = in.intensity;
    out.accent = in.accent;
    out.time = view.time;
    out.aspect = in.half.y / max(in.half.x, 1.0);
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

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Esquinas redondeadas (SDF) con borde suave (alfa).
    let uvc = in.uv * 2.0 - vec2<f32>(1.0, 1.0);
    let rad = 0.12;
    let dr = length(max(abs(uvc) - vec2<f32>(1.0 - rad, 1.0 - rad), vec2<f32>(0.0))) - rad;
    let alpha = 1.0 - smoothstep(0.0, 0.012, dr);
    if (alpha <= 0.001) {
        discard;
    }

    let h = in.hover;
    let t = in.time;
    let inten = in.intensity;
    let fin = i32(round(in.finish));
    var col = in.base;

    // Reflejo (glare) comun: highlight radial centrado en el cursor.
    let gd = distance(in.uv, in.pointer);
    let glare = smoothstep(0.55, 0.0, gd) * 0.45 * h;

    if (fin >= 200) {
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
    let edge = smoothstep(-0.05, 0.0, dr);
    col = mix(col, vec3<f32>(1.0, 1.0, 1.0), edge * 0.35);

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), alpha);
}
