//! Modelo de trazos y teselado a malla de triangulos.
//!
//! Un trazo es una lista de muestras (posicion + presion). Para dibujarlo lo
//! convertimos en triangulos de ancho variable: en cada segmento generamos un
//! cuadrilatero desplazando la linea central segun la normal, y en cada muestra
//! estampamos un circulo. Los circulos rellenan las uniones (joins) y forman las
//! puntas redondeadas (caps) sin huecos, incluso en curvas cerradas.

use bytemuck::{Pod, Zeroable};
use glam::Vec2;

/// Vertice que consume la GPU: posicion en coordenadas de mundo + color RGBA.
///
/// `#[repr(C)]` + `Pod` permiten subirlo a un buffer de vertices sin copias.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
}

impl Vertex {
    #[inline]
    fn new(p: Vec2, color: [f32; 4]) -> Self {
        Self { pos: [p.x, p.y], color }
    }
}

/// Una muestra de entrada del lapiz/raton.
#[derive(Clone, Copy, Debug)]
pub struct InputSample {
    /// Posicion en coordenadas de mundo (no de pantalla).
    pub pos: Vec2,
    /// Presion normalizada en `0.0..=1.0`.
    pub pressure: f32,
}

/// Tipo de pincel: define COMO se dibuja el trazo (su "motor").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushKind {
    Pen,        // ancho variable por presion (pluma)
    FixedWidth, // ancho constante
    Marker,     // marcador (ancho constante, ideal con opacidad)
    Pencil,     // lapiz granulado (textura)
    Watercolor, // acuarela (manchas suaves que se acumulan)
    Airbrush,   // aerografo (spray de puntos)
    Dotted,     // punteado
}

impl Default for BrushKind {
    fn default() -> Self {
        BrushKind::Pen
    }
}

/// Parametros del pincel actual.
#[derive(Clone, Copy, Debug)]
pub struct Brush {
    pub color: [f32; 4],
    /// Ancho maximo (a presion plena), en unidades de mundo.
    pub width: f32,
    /// Opacidad del trazo, 0..=1 (se aplica al canal alfa del color).
    pub opacity: f32,
    /// Suavizado del trazo, 0..=1 (mas alto = mas suave, un poco mas de lag).
    pub smoothing: f32,
    /// Tipo de pincel (motor de dibujo).
    pub kind: BrushKind,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            color: [0.10, 0.10, 0.13, 1.0],
            width: 4.0,
            opacity: 1.0,
            smoothing: 0.35,
            kind: BrushKind::Pen,
        }
    }
}

/// Un trazo completo.
#[derive(Clone, Debug)]
pub struct Stroke {
    pub samples: Vec<InputSample>,
    pub brush: Brush,
}

impl Stroke {
    pub fn new(brush: Brush) -> Self {
        Self { samples: Vec::new(), brush }
    }

    #[inline]
    pub fn push(&mut self, s: InputSample) {
        self.samples.push(s);
    }

    /// Tesela este trazo, *anexando* los triangulos a `out`.
    pub fn tessellate(&self, out: &mut Vec<Vertex>) {
        tessellate_stroke(&self.samples, &self.brush, out);
    }
}

/// Medio-ancho del trazo en una muestra. Mantiene un minimo para que ni la
/// presion baja haga desaparecer la linea.
#[inline]
fn half_width(brush: &Brush, pressure: f32, use_pressure: bool) -> f32 {
    if use_pressure {
        let p = pressure.clamp(0.0, 1.0);
        brush.width * 0.5 * (0.25 + 0.75 * p)
    } else {
        brush.width * 0.5
    }
}

/// PRNG determinista (hash) -> [0,1). Estable al re-teselar el mismo trazo.
#[inline]
fn hash01(a: i32, b: i32, salt: u32) -> f32 {
    let mut h = (a as u32)
        .wrapping_mul(0x9E37_79B1)
        ^ (b as u32).wrapping_mul(0x85EB_CA77)
        ^ salt.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    (h & 0x00FF_FFFF) as f32 / 0x00FF_FFFF as f32
}

#[inline]
fn with_alpha(mut c: [f32; 4], factor: f32) -> [f32; 4] {
    c[3] *= factor;
    c
}

/// Segmentos por circulo de union/punta. Mas = mas suave, mas triangulos.
const CIRCLE_SEGMENTS: usize = 16;

fn emit_circle(out: &mut Vec<Vertex>, center: Vec2, r: f32, color: [f32; 4]) {
    if r <= 0.0 {
        return;
    }
    use core::f32::consts::TAU;
    let step = TAU / CIRCLE_SEGMENTS as f32;
    let mut prev = center + Vec2::new(r, 0.0);
    for i in 1..=CIRCLE_SEGMENTS {
        let a = step * i as f32;
        let cur = center + Vec2::new(r * a.cos(), r * a.sin());
        out.push(Vertex::new(center, color));
        out.push(Vertex::new(prev, color));
        out.push(Vertex::new(cur, color));
        prev = cur;
    }
}

#[inline]
fn safe_dir(v: Vec2) -> Vec2 {
    let len = v.length();
    if len > 1e-6 {
        v / len
    } else {
        Vec2::new(1.0, 0.0)
    }
}

/// Convierte una secuencia de muestras en triangulos (lista de triangulos).
///
/// Anexa a `out` para poder hornear muchos trazos en un solo buffer.
pub fn tessellate_stroke(samples: &[InputSample], brush: &Brush, out: &mut Vec<Vertex>) {
    if samples.is_empty() {
        return;
    }
    match brush.kind {
        BrushKind::Pen => stroke_variable(samples, brush, out, true),
        BrushKind::FixedWidth | BrushKind::Marker => stroke_variable(samples, brush, out, false),
        BrushKind::Pencil => stroke_pencil(samples, brush, out),
        BrushKind::Watercolor => stroke_watercolor(samples, brush, out),
        BrushKind::Airbrush => stroke_airbrush(samples, brush, out),
        BrushKind::Dotted => stroke_dotted(samples, brush, out),
    }
}

/// Teselado INCREMENTAL del trazo en vivo: anexa a `out` solo la geometria de la
/// ULTIMA muestra recien agregada, en vez de re-teselar todo el trazo (que es O(n)
/// por punto -> O(n^2) por trazo y causa lag al escribir trazos largos).
///
/// `out` debe contener ya la geometria de las muestras previas. Devuelve `false` si
/// el pincel no soporta incremental (tiene estado entre segmentos); en ese caso el
/// llamador debe re-teselar el trazo completo. No modifica `out` cuando devuelve `false`.
pub fn tessellate_incremental(samples: &[InputSample], brush: &Brush, out: &mut Vec<Vertex>) -> bool {
    let n = samples.len();
    if n == 0 {
        return true;
    }
    match brush.kind {
        BrushKind::Pen | BrushKind::FixedWidth | BrushKind::Marker => {
            let use_pressure = matches!(brush.kind, BrushKind::Pen);
            let color = brush.color;
            if n == 1 {
                // Punta inicial.
                emit_circle(out, samples[0].pos, half_width(brush, samples[0].pressure, use_pressure), color);
            } else {
                // Cuadrilatero del nuevo segmento + circulo de union/punta del nuevo punto.
                let p0 = samples[n - 2].pos;
                let p1 = samples[n - 1].pos;
                let dir = safe_dir(p1 - p0);
                let nrm = Vec2::new(-dir.y, dir.x);
                let hw0 = half_width(brush, samples[n - 2].pressure, use_pressure);
                let hw1 = half_width(brush, samples[n - 1].pressure, use_pressure);
                let l0 = p0 + nrm * hw0;
                let r0 = p0 - nrm * hw0;
                let l1 = p1 + nrm * hw1;
                let r1 = p1 - nrm * hw1;
                out.push(Vertex::new(l0, color));
                out.push(Vertex::new(r0, color));
                out.push(Vertex::new(l1, color));
                out.push(Vertex::new(r0, color));
                out.push(Vertex::new(r1, color));
                out.push(Vertex::new(l1, color));
                emit_circle(out, p1, hw1, color);
            }
            true
        }
        BrushKind::Pencil => {
            // Granos del ultimo segmento (mismo hash/indice que el teselado completo).
            if n >= 2 {
                pencil_segment(samples[n - 2].pos, samples[n - 1].pos, n - 2, brush, out);
            }
            true
        }
        BrushKind::Watercolor => {
            if n >= 2 {
                watercolor_segment(samples[n - 2].pos, samples[n - 1].pos, n - 2, brush, out);
            }
            true
        }
        BrushKind::Airbrush => {
            // El aerografo rocia por MUESTRA (no por segmento): solo la nueva muestra.
            airbrush_sample(samples[n - 1].pos, n - 1, brush, out);
            true
        }
        // El punteado acumula distancia entre segmentos: no es incremental trivial.
        BrushKind::Dotted => false,
    }
}

/// Trazo de ancho (variable por presion o constante): pluma / ancho fijo / marcador.
fn stroke_variable(samples: &[InputSample], brush: &Brush, out: &mut Vec<Vertex>, use_pressure: bool) {
    let color = brush.color;
    if samples.len() == 1 {
        emit_circle(out, samples[0].pos, half_width(brush, samples[0].pressure, use_pressure), color);
        return;
    }
    let n = samples.len();
    for i in 0..n - 1 {
        let p0 = samples[i].pos;
        let p1 = samples[i + 1].pos;
        let dir = safe_dir(p1 - p0);
        let nrm = Vec2::new(-dir.y, dir.x);
        let hw0 = half_width(brush, samples[i].pressure, use_pressure);
        let hw1 = half_width(brush, samples[i + 1].pressure, use_pressure);
        let l0 = p0 + nrm * hw0;
        let r0 = p0 - nrm * hw0;
        let l1 = p1 + nrm * hw1;
        let r1 = p1 - nrm * hw1;
        out.push(Vertex::new(l0, color));
        out.push(Vertex::new(r0, color));
        out.push(Vertex::new(l1, color));
        out.push(Vertex::new(r0, color));
        out.push(Vertex::new(r1, color));
        out.push(Vertex::new(l1, color));
    }
    for s in samples {
        emit_circle(out, s.pos, half_width(brush, s.pressure, use_pressure), color);
    }
}

/// Granos de lapiz de UN segmento (`wi` = indice de ventana, para el hash estable).
fn pencil_segment(p0: Vec2, p1: Vec2, wi: usize, brush: &Brush, out: &mut Vec<Vertex>) {
    let hw = brush.width * 0.5;
    let seg = p1 - p0;
    let len = seg.length();
    if len < 1e-4 {
        return;
    }
    let dir = seg / len;
    let nrm = Vec2::new(-dir.y, dir.x);
    let steps = ((len / (brush.width * 0.3).max(0.5)).ceil() as usize).max(1);
    for s in 0..steps {
        let t = s as f32 / steps as f32;
        let base = p0 + seg * t;
        for k in 0..3 {
            let r1 = hash01(wi as i32 * 31 + s as i32, k, 7);
            let r2 = hash01(wi as i32 * 31 + s as i32, k, 13);
            let off = nrm * ((r1 - 0.5) * 2.0 * hw);
            emit_circle(out, base + off, brush.width * 0.16 + 0.4, with_alpha(brush.color, 0.22 + 0.5 * r2));
        }
    }
}

/// Lapiz: granos pequenos con jitter y alfa variable -> textura granulada.
fn stroke_pencil(samples: &[InputSample], brush: &Brush, out: &mut Vec<Vertex>) {
    for (wi, w) in samples.windows(2).enumerate() {
        pencil_segment(w[0].pos, w[1].pos, wi, brush, out);
    }
}

/// Manchas de acuarela de UN segmento.
fn watercolor_segment(p0: Vec2, p1: Vec2, wi: usize, brush: &Brush, out: &mut Vec<Vertex>) {
    let col = with_alpha(brush.color, 0.16);
    let seg = p1 - p0;
    let len = seg.length();
    let steps = ((len / (brush.width * 0.4).max(0.5)).ceil() as usize).max(1);
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let r = hash01(wi as i32, s as i32, 3);
        emit_circle(out, p0 + seg * t, brush.width * (1.05 + 0.35 * r), col);
    }
}

/// Acuarela: manchas grandes de baja opacidad que se acumulan al solaparse.
fn stroke_watercolor(samples: &[InputSample], brush: &Brush, out: &mut Vec<Vertex>) {
    for (wi, w) in samples.windows(2).enumerate() {
        watercolor_segment(w[0].pos, w[1].pos, wi, brush, out);
    }
}

/// Spray de aerografo de UNA muestra (`i` = indice de muestra, para el hash estable).
fn airbrush_sample(pos: Vec2, i: usize, brush: &Brush, out: &mut Vec<Vertex>) {
    let col = with_alpha(brush.color, 0.10);
    for k in 0..18 {
        let r1 = hash01(i as i32, k, 5);
        let r2 = hash01(i as i32, k, 9);
        let r3 = hash01(i as i32, k, 11);
        let ang = r1 * std::f32::consts::TAU;
        let rad = r2.sqrt() * brush.width * 0.9;
        let off = Vec2::new(ang.cos() * rad, ang.sin() * rad);
        emit_circle(out, pos + off, 1.4, with_alpha(col, 0.5 + 0.5 * r3));
    }
}

/// Aerografo: spray de puntos pequenos alrededor del trazo.
fn stroke_airbrush(samples: &[InputSample], brush: &Brush, out: &mut Vec<Vertex>) {
    for (i, s) in samples.iter().enumerate() {
        airbrush_sample(s.pos, i, brush, out);
    }
}

/// Punteado: puntos a intervalos regulares a lo largo del trazo.
fn stroke_dotted(samples: &[InputSample], brush: &Brush, out: &mut Vec<Vertex>) {
    let color = brush.color;
    let r = brush.width * 0.5;
    let spacing = (brush.width * 1.7).max(3.0);
    emit_circle(out, samples[0].pos, r, color);
    let mut acc = 0.0_f32;
    for w in samples.windows(2) {
        let seg = w[1].pos - w[0].pos;
        let len = seg.length();
        if len < 1e-4 {
            continue;
        }
        let dir = seg / len;
        let mut d = 0.0;
        loop {
            let need = spacing - acc;
            if d + need > len {
                acc += len - d;
                break;
            }
            d += need;
            acc = 0.0;
            emit_circle(out, w[0].pos + dir * d, r, color);
        }
    }
}
