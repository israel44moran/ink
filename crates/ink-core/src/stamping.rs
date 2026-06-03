//! Motor de pincel estilo Photoshop: convierte un trazo (lista de muestras) en una
//! secuencia de ESTAMPADOS de la punta, aplicando todas las dinamicas del panel
//! "Ajustes del pincel" de PS.
//!
//! La parte que depende de la GPU (subir la textura de la punta y dibujarla) vive en
//! la capa de app; aqui solo se calcula, de forma portable y determinista, DONDE y
//! COMO cae cada estampado (posicion, tamano, angulo, redondez, opacidad/flujo).
//!
//! El modelo [`BrushSettings`] refleja 1:1 las secciones del panel de PS:
//! Forma de la punta, Dinamica de forma, Dispersion, Textura, Pincel doble,
//! Dinamica de color, Transferencia, Pose, Ruido, Bordes humedos, Concentracion,
//! Suavizar y Proteger textura.

use bytemuck::{Pod, Zeroable};
use glam::Vec2;

use crate::stroke::InputSample;

/// Vertice de un estampado texturizado que consume la GPU: posicion de mundo, UV en
/// la textura de la punta, y color RGBA (alfa = opacidad/flujo de ese estampado).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct StampVertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    /// Tiempo de creacion del trazo (goma por timestamps). Lo fija el shell por trazo.
    pub time: f32,
}

/// Convierte un [`Stamp`] en dos triangulos (un quad) con UV y color, y los anexa a
/// `out`. `tip_aspect` = ancho/alto de la textura de la punta (para no deformarla);
/// `rgb` es el color del pincel. Tambien aplica el desplazamiento de color del stamp.
pub fn push_stamp_quad(out: &mut Vec<StampVertex>, st: &Stamp, tip_aspect: f32, rgb: [f32; 3]) {
    let half = st.size * 0.5;
    // Respetar el aspecto de la punta: el "tamano" escala el lado mayor.
    let (hw, hh) = if tip_aspect >= 1.0 {
        (half, half / tip_aspect)
    } else {
        (half * tip_aspect, half)
    };
    let hh = hh * st.roundness.clamp(0.01, 1.0); // redondez aplana el eje menor
    let a = st.angle.to_radians();
    let (c, s) = (a.cos(), a.sin());
    let right = Vec2::new(c, s) * hw;
    let up = Vec2::new(-s, c) * hh;
    let p = st.pos;

    // UV con volteo opcional.
    let (u0, u1) = if st.flip_x { (1.0, 0.0) } else { (0.0, 1.0) };
    let (v0, v1) = if st.flip_y { (1.0, 0.0) } else { (0.0, 1.0) };

    // Color del pincel + desplazamiento simple por dinamica de color (en RGB).
    let col = [
        (rgb[0] + st.bright_shift * 0.5 + st.hue_shift * 0.15).clamp(0.0, 1.0),
        (rgb[1] + st.bright_shift * 0.5).clamp(0.0, 1.0),
        (rgb[2] + st.bright_shift * 0.5 - st.hue_shift * 0.15).clamp(0.0, 1.0),
        st.alpha,
    ];

    let tl = p - right + up;
    let tr = p + right + up;
    let bl = p - right - up;
    let br = p + right - up;
    let vtl = StampVertex { pos: [tl.x, tl.y], uv: [u0, v0], color: col, time: 0.0 };
    let vtr = StampVertex { pos: [tr.x, tr.y], uv: [u1, v0], color: col, time: 0.0 };
    let vbl = StampVertex { pos: [bl.x, bl.y], uv: [u0, v1], color: col, time: 0.0 };
    let vbr = StampVertex { pos: [br.x, br.y], uv: [u1, v1], color: col, time: 0.0 };
    out.extend_from_slice(&[vtl, vtr, vbl, vtr, vbr, vbl]);
}

/// Que controla la variacion de un parametro (columna "Control:" de PS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynControl {
    /// Desactivado: solo el "jitter" aleatorio actua.
    Off,
    /// Desvanecer en N estampados (de maximo a minimo).
    Fade(u32),
    /// Presion de la pluma (lo mas comun).
    PenPressure,
    /// Inclinacion del lapiz.
    PenTilt,
    /// Rueda del stylus / aerografo.
    StylusWheel,
    /// Direccion del trazo.
    Direction,
    /// Rotacion del stylus.
    Rotation,
}

impl Default for DynControl {
    fn default() -> Self {
        DynControl::Off
    }
}

/// Tipo de punta: redonda procedural (con dureza) o una textura muestreada (.abr).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipKind {
    /// Punta redonda generada por dureza (sin textura): el render usa un perfil radial.
    Round,
    /// Punta muestreada: el render usa la textura `tip` (indice externo).
    Sampled(u32),
}

impl Default for TipKind {
    fn default() -> Self {
        TipKind::Round
    }
}

/// Modo de mezcla del estampado (subconjunto util de los de PS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    LinearDodge, // "Subexponer color (Añadir)"
}

/// Ajustes COMPLETOS de un pincel, equivalentes al panel de PS.
#[derive(Clone, Debug)]
pub struct BrushSettings {
    pub name: String,
    pub tip: TipKind,

    // ---------------- Forma de la punta del pincel ----------------
    /// Diametro base, en pixeles de mundo.
    pub size: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// Angulo de la punta, en grados.
    pub angle: f32,
    /// Redondez 0..1 (1.0 = circulo; <1 aplana la punta).
    pub roundness: f32,
    /// Dureza 0..1 (solo punta redonda): borde duro (1) a difuso (0).
    pub hardness: f32,
    pub spacing_on: bool,
    /// Espaciado como fraccion del diametro (0.05 = 5%, 0.25 = 25%...).
    pub spacing: f32,

    // ---------------- Dinamica de forma ----------------
    pub shape_dyn: bool,
    /// Variacion del tamano 0..1.
    pub size_jitter: f32,
    pub size_control: DynControl,
    /// Diametro minimo 0..1 (a presion 0 cuando el control es presion).
    pub min_diameter: f32,
    /// Variacion del angulo 0..1 (fraccion de 360deg).
    pub angle_jitter: f32,
    pub angle_control: DynControl,
    /// Variacion de la redondez 0..1.
    pub roundness_jitter: f32,
    pub roundness_control: DynControl,
    pub min_roundness: f32,
    pub flip_x_jitter: bool,
    pub flip_y_jitter: bool,

    // ---------------- Dispersion ----------------
    pub scatter_on: bool,
    /// Dispersion 0..n (fraccion del diametro hacia los lados / ambos ejes).
    pub scatter: f32,
    pub scatter_both_axes: bool,
    pub scatter_control: DynControl,
    /// Cantidad de estampados por posicion (1..n).
    pub count: u32,
    pub count_jitter: f32,
    pub count_control: DynControl,

    // ---------------- Textura ----------------
    pub texture_on: bool,
    pub texture_scale: f32,
    pub texture_depth: f32,
    pub texture_each_tip: bool,
    pub texture_invert: bool,

    // ---------------- Pincel doble ----------------
    pub dual_on: bool,
    pub dual_tip: TipKind,
    pub dual_size: f32,
    pub dual_spacing: f32,
    pub dual_scatter: f32,
    pub dual_count: u32,
    pub dual_mode: BlendMode,

    // ---------------- Dinamica de color ----------------
    pub color_dyn: bool,
    pub hue_jitter: f32,
    pub sat_jitter: f32,
    pub bright_jitter: f32,
    pub fg_bg_jitter: f32,

    // ---------------- Transferencia ----------------
    pub transfer_on: bool,
    /// Variacion de opacidad 0..1.
    pub opacity_jitter: f32,
    pub opacity_control: DynControl,
    /// Variacion de flujo 0..1.
    pub flow_jitter: f32,
    pub flow_control: DynControl,

    // ---------------- Otras casillas ----------------
    /// Pose del pincel (forzar angulo/inclinacion fijos): guardamos el estado.
    pub pose_on: bool,
    pub noise: bool,
    pub wet_edges: bool,
    /// Concentracion (acumulacion tipo aerografo).
    pub buildup: bool,
    /// Suavizar el trazo (filtro de entrada).
    pub smoothing: bool,
    pub protect_texture: bool,

    // ---------------- Base (no son del panel "dinamicas" pero definen el pincel) ----------------
    /// Opacidad base 0..1 (tope del trazo).
    pub opacity: f32,
    /// Flujo base 0..1 (deposito por estampado).
    pub flow: f32,
    pub blend: BlendMode,
}

impl Default for BrushSettings {
    /// Por defecto: punta redonda dura de 20px, espaciado 25% (como el round de PS).
    fn default() -> Self {
        Self {
            name: "Redondo duro".into(),
            tip: TipKind::Round,
            size: 20.0,
            flip_x: false,
            flip_y: false,
            angle: 0.0,
            roundness: 1.0,
            hardness: 1.0,
            spacing_on: true,
            spacing: 0.25,

            shape_dyn: false,
            size_jitter: 0.0,
            size_control: DynControl::Off,
            min_diameter: 0.0,
            angle_jitter: 0.0,
            angle_control: DynControl::Off,
            roundness_jitter: 0.0,
            roundness_control: DynControl::Off,
            min_roundness: 0.25,
            flip_x_jitter: false,
            flip_y_jitter: false,

            scatter_on: false,
            scatter: 0.0,
            scatter_both_axes: false,
            scatter_control: DynControl::Off,
            count: 1,
            count_jitter: 0.0,
            count_control: DynControl::Off,

            texture_on: false,
            texture_scale: 1.0,
            texture_depth: 1.0,
            texture_each_tip: false,
            texture_invert: false,

            dual_on: false,
            dual_tip: TipKind::Round,
            dual_size: 20.0,
            dual_spacing: 0.25,
            dual_scatter: 0.0,
            dual_count: 1,
            dual_mode: BlendMode::Multiply,

            color_dyn: false,
            hue_jitter: 0.0,
            sat_jitter: 0.0,
            bright_jitter: 0.0,
            fg_bg_jitter: 0.0,

            transfer_on: false,
            opacity_jitter: 0.0,
            opacity_control: DynControl::Off,
            flow_jitter: 0.0,
            flow_control: DynControl::Off,

            pose_on: false,
            noise: false,
            wet_edges: false,
            buildup: false,
            smoothing: true,
            protect_texture: false,

            opacity: 1.0,
            flow: 1.0,
            blend: BlendMode::Normal,
        }
    }
}

/// Un estampado concreto de la punta sobre el lienzo (lo que el render dibuja).
#[derive(Clone, Copy, Debug)]
pub struct Stamp {
    /// Centro en coordenadas de mundo.
    pub pos: Vec2,
    /// Diametro efectivo (px de mundo).
    pub size: f32,
    /// Angulo en grados.
    pub angle: f32,
    /// Redondez 0..1 (escala el eje menor).
    pub roundness: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// Opacidad/flujo efectivo de este estampado 0..1.
    pub alpha: f32,
    /// Desplazamiento de tono/sat/brillo (dinamica de color), -1..1; 0 = sin cambio.
    pub hue_shift: f32,
    pub sat_shift: f32,
    pub bright_shift: f32,
}

/// PRNG determinista (hash) -> [0,1). Estable para re-teselar el mismo trazo.
#[inline]
fn hash01(i: u32, salt: u32) -> f32 {
    let mut h = i.wrapping_mul(0x9E37_79B1) ^ salt.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    (h & 0x00FF_FFFF) as f32 / 0x00FF_FFFF as f32
}

/// Jitter simetrico en [-amt, amt].
#[inline]
fn jitter(i: u32, salt: u32, amt: f32) -> f32 {
    (hash01(i, salt) * 2.0 - 1.0) * amt
}

/// Aplica un control de dinamica a un factor base. Devuelve 0..1 segun el control:
/// presion -> presion; fade -> rampa; off -> 1.0 (sin reduccion por control).
#[inline]
fn control_factor(ctrl: DynControl, pressure: f32, stamp_index: u32) -> f32 {
    match ctrl {
        DynControl::Off => 1.0,
        DynControl::PenPressure | DynControl::PenTilt | DynControl::StylusWheel => pressure.clamp(0.0, 1.0),
        DynControl::Fade(n) => {
            if n == 0 {
                0.0
            } else {
                (1.0 - stamp_index as f32 / n as f32).clamp(0.0, 1.0)
            }
        }
        // Direccion/Rotacion: el angulo lo maneja el llamador; aqui no reduce tamano.
        DynControl::Direction | DynControl::Rotation => 1.0,
    }
}

/// Resultado del estampado de un trazo: los estampados + cuanto del trazo ya se
/// "consumio" (para continuar de forma incremental en el siguiente punto).
pub struct StampOutput {
    pub stamps: Vec<Stamp>,
    /// Distancia sobrante hasta el proximo estampado (acumulador de espaciado).
    pub residual: f32,
    /// Indice global del proximo estampado (para que el jitter sea continuo).
    pub next_index: u32,
}

/// Genera los estampados de un tramo del trazo `samples`, empezando con un acumulador
/// de espaciado `residual` y un contador global `start_index` (para jitter continuo).
///
/// Determinista: el mismo trazo + ajustes producen siempre los mismos estampados.
pub fn stamp_path(samples: &[InputSample], s: &BrushSettings, start_index: u32, residual: f32) -> StampOutput {
    let mut stamps = Vec::new();
    if samples.is_empty() {
        return StampOutput { stamps, residual, next_index: start_index };
    }

    // Espaciado en px de mundo (minimo 1px de diametro para no dividir por ~0).
    let diameter = s.size.max(0.1);
    let step = if s.spacing_on {
        (s.spacing.max(0.01) * diameter).max(0.5)
    } else {
        // "Espaciado" desactivado: PS estampa muy densamente (~1px).
        1.0_f32.max(diameter * 0.02)
    };

    let mut index = start_index;
    let mut acc = residual; // distancia acumulada desde el ultimo estampado

    // Caso de una sola muestra (un toque): un estampado.
    if samples.len() == 1 {
        emit_stamps_at(&mut stamps, samples[0].pos, samples[0].pressure, 0.0, s, &mut index);
        return StampOutput { stamps, residual: 0.0, next_index: index };
    }

    for w in samples.windows(2) {
        let p0 = w[0].pos;
        let p1 = w[1].pos;
        let seg = p1 - p0;
        let seg_len = seg.length();
        if seg_len < 1e-5 {
            continue;
        }
        let dir = seg / seg_len;
        let seg_angle = dir.y.atan2(dir.x).to_degrees();
        let mut dist = 0.0_f32;
        // Avanzar por el segmento dejando un estampado cada `step`.
        loop {
            let need = step - acc;
            if dist + need > seg_len {
                acc += seg_len - dist;
                break;
            }
            dist += need;
            acc = 0.0;
            let t = dist / seg_len;
            let pos = p0 + seg * t;
            let pressure = w[0].pressure + (w[1].pressure - w[0].pressure) * t;
            emit_stamps_at(&mut stamps, pos, pressure, seg_angle, s, &mut index);
        }
    }

    StampOutput { stamps, residual: acc, next_index: index }
}

/// Emite los estampados de UNA posicion del trazo (puede ser >1 por "Cantidad" y
/// "Dispersion"), aplicando todas las dinamicas activas.
fn emit_stamps_at(out: &mut Vec<Stamp>, pos: Vec2, pressure: f32, seg_angle: f32, s: &BrushSettings, index: &mut u32) {
    // Cantidad de estampados (Dispersion -> Cantidad).
    let mut count = s.count.max(1);
    if s.scatter_on && s.count_jitter > 0.0 {
        let extra = (hash01(*index, 0xC0) * s.count_jitter * s.count as f32) as u32;
        count = (count + extra).max(1);
    }

    for _ in 0..count {
        let i = *index;
        *index = index.wrapping_add(1);

        // --- Tamano ---
        let mut size = s.size;
        if s.shape_dyn {
            // Jitter aleatorio.
            let mut f = 1.0 - hash01(i, 0x51) * s.size_jitter;
            // Control (presion/fade): interpola entre min_diameter y 1.
            let cf = control_factor(s.size_control, pressure, i);
            if s.size_control != DynControl::Off {
                f *= s.min_diameter + (1.0 - s.min_diameter) * cf;
            }
            size *= f.clamp(0.0, 4.0);
        }
        size = size.max(0.25);

        // --- Angulo ---
        let mut angle = s.angle;
        if s.angle_control == DynControl::Direction {
            angle += seg_angle;
        }
        if s.shape_dyn && s.angle_jitter > 0.0 {
            angle += jitter(i, 0x7A, s.angle_jitter) * 180.0;
        }

        // --- Redondez ---
        let mut roundness = s.roundness;
        if s.shape_dyn && s.roundness_jitter > 0.0 {
            let cf = control_factor(s.roundness_control, pressure, i);
            let base = if s.roundness_control != DynControl::Off { cf } else { 1.0 };
            let j = hash01(i, 0x3D) * s.roundness_jitter;
            roundness = (roundness * base * (1.0 - j)).clamp(s.min_roundness.max(0.01), 1.0);
        }

        // --- Volteo ---
        let mut flip_x = s.flip_x;
        let mut flip_y = s.flip_y;
        if s.flip_x_jitter && hash01(i, 0x11) > 0.5 {
            flip_x = !flip_x;
        }
        if s.flip_y_jitter && hash01(i, 0x22) > 0.5 {
            flip_y = !flip_y;
        }

        // --- Posicion (Dispersion) ---
        let mut p = pos;
        if s.scatter_on && s.scatter > 0.0 {
            let cf = control_factor(s.scatter_control, pressure, i);
            let amt = s.scatter * s.size * cf;
            // Perpendicular al trazo.
            let perp = Vec2::new(-(seg_angle.to_radians().sin()), seg_angle.to_radians().cos());
            p += perp * jitter(i, 0x9C, amt);
            if s.scatter_both_axes {
                let along = Vec2::new(seg_angle.to_radians().cos(), seg_angle.to_radians().sin());
                p += along * jitter(i, 0x9D, amt);
            }
        }

        // --- Opacidad / Flujo (Transferencia) ---
        // Base = flujo * opacidad (aprox. sin render-por-trazo: la opacidad atenua).
        let mut alpha = (s.flow * s.opacity).clamp(0.0, 1.0);
        if s.transfer_on {
            if s.flow_jitter > 0.0 {
                alpha *= 1.0 - hash01(i, 0xF1) * s.flow_jitter;
            }
            let cf = control_factor(s.flow_control, pressure, i);
            if s.flow_control != DynControl::Off {
                alpha *= cf;
            }
            // La variacion de opacidad se modela tambien como atenuacion del estampado.
            if s.opacity_jitter > 0.0 {
                alpha *= 1.0 - hash01(i, 0xF2) * s.opacity_jitter;
            }
            let co = control_factor(s.opacity_control, pressure, i);
            if s.opacity_control != DynControl::Off {
                alpha *= co;
            }
        }

        // --- Dinamica de color ---
        let (mut hue, mut sat, mut bri) = (0.0, 0.0, 0.0);
        if s.color_dyn {
            hue = jitter(i, 0xA1, s.hue_jitter);
            sat = jitter(i, 0xA2, s.sat_jitter);
            bri = jitter(i, 0xA3, s.bright_jitter);
        }

        out.push(Stamp {
            pos: p,
            size,
            angle,
            roundness,
            flip_x,
            flip_y,
            alpha: alpha.clamp(0.0, 1.0),
            hue_shift: hue,
            sat_shift: sat,
            bright_shift: bri,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec2;

    fn line(a: Vec2, b: Vec2, n: usize) -> Vec<InputSample> {
        (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                InputSample { pos: a + (b - a) * t, pressure: 1.0, erosion: 0.0 }
            })
            .collect()
    }

    #[test]
    fn spacing_controls_stamp_count() {
        let mut s = BrushSettings::default();
        s.size = 10.0;
        s.spacing = 0.25; // 2.5px de paso
        let samples = line(vec2(0.0, 0.0), vec2(100.0, 0.0), 10);
        let out = stamp_path(&samples, &s, 0, 0.0);
        // ~100px / 2.5px ≈ 40 estampados (+/- por el residuo).
        assert!(out.stamps.len() >= 35 && out.stamps.len() <= 45, "n={}", out.stamps.len());
    }

    #[test]
    fn pressure_size_dynamics_shrink_at_low_pressure() {
        let mut s = BrushSettings::default();
        s.size = 20.0;
        s.shape_dyn = true;
        s.size_control = DynControl::PenPressure;
        s.min_diameter = 0.0;
        let lo = vec![InputSample { pos: vec2(0.0, 0.0), pressure: 0.1, erosion: 0.0 }];
        let hi = vec![InputSample { pos: vec2(0.0, 0.0), pressure: 1.0, erosion: 0.0 }];
        let slo = stamp_path(&lo, &s, 0, 0.0).stamps[0].size;
        let shi = stamp_path(&hi, &s, 0, 0.0).stamps[0].size;
        assert!(slo < shi, "presion baja debe achicar: {slo} vs {shi}");
    }

    #[test]
    fn incremental_matches_residual() {
        // Estampar en dos tramos con residuo continuo debe equivaler a un solo tramo.
        let mut s = BrushSettings::default();
        s.size = 8.0;
        s.spacing = 0.3;
        let full = line(vec2(0.0, 0.0), vec2(60.0, 0.0), 12);
        let one = stamp_path(&full, &s, 0, 0.0);

        let part_a = line(vec2(0.0, 0.0), vec2(30.0, 0.0), 6);
        let part_b = line(vec2(30.0, 0.0), vec2(60.0, 0.0), 6);
        let oa = stamp_path(&part_a, &s, 0, 0.0);
        let ob = stamp_path(&part_b, &s, oa.next_index, oa.residual);
        // El total de estampados debe coincidir (±1 por bordes de segmento).
        let split = oa.stamps.len() + ob.stamps.len();
        assert!((split as i32 - one.stamps.len() as i32).abs() <= 1, "{} vs {}", split, one.stamps.len());
    }
}
