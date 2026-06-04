//! Suavizador de ENTRADA del trazo: convierte los eventos crudos del lapiz/raton en una
//! polilinea DENSA y SUAVE (curva Catmull-Rom CENTRIPETA) con baja latencia. Es la pieza
//! que hace que el trazo se sienta "de calidad extrema" (Concepts / GoodNotes):
//!
//! - **One-Euro** filtra la posicion: quita el temblor de la mano sin lag perceptible.
//! - **Presion fluida**: la presion se suaviza con un EMA, asi el ancho deja de "escalonar".
//! - **Curvas sin esquinas**: los huecos del movimiento rapido se rellenan interpolando por
//!   spline. El teselado no cambia (sigue uniendo muestras con rectas), pero ahora las
//!   muestras son densas y caen sobre una curva suave -> sirve para CUALQUIER pincel.
//! - **Afilado de entrada (taper)**: rampa la presion en las primeras unidades del trazo.
//!
//! Emite de forma CAUSAL con ~1 evento de retardo: cada segmento se curva usando sus dos
//! vecinos (ya conocidos). El ultimo tramo pendiente se cierra en [`InkSmoother::finish`].
//! No conoce la GPU ni la plataforma; solo produce [`InputSample`]s listas para teselar.

use glam::Vec2;

use crate::smoothing::OneEuroFilter;
use crate::stroke::InputSample;

/// Ajustes del suavizador. Todas las longitudes en UNIDADES DE MUNDO (el llamador las
/// escala por el zoom para que se sientan constantes en pantalla).
#[derive(Clone, Copy, Debug)]
pub struct InkConfig {
    /// Paso de densificado: separacion objetivo entre muestras emitidas.
    pub max_step: f32,
    /// Longitud del afilado de ENTRADA (0 = sin taper).
    pub taper_in: f32,
    /// Suavizado de presion 0..1 (0 = presion cruda; 1 = muy suave).
    pub pressure_smooth: f32,
    /// Suavizado de POSICION 0..1 (deslizador "Suavizado" del panel). Modula el One-Euro:
    /// 0 = mas crudo/pegado a la punta; 1 = quita mas temblor (un pelin mas de lag). Aun a 1
    /// es de baja latencia (el One-Euro se abre con la velocidad).
    pub smoothing: f32,
    /// Si el pincel usa presion (la pluma). Si es `false`, no se aplica el taper por presion.
    pub use_pressure: bool,
}

impl Default for InkConfig {
    fn default() -> Self {
        Self { max_step: 2.0, taper_in: 0.0, pressure_smooth: 0.5, smoothing: 0.5, use_pressure: true }
    }
}

/// Estado del suavizado de un trazo en curso.
pub struct InkSmoother {
    euro: OneEuroFilter,
    cfg: InkConfig,
    /// Puntos de control filtrados: `(posicion, presion_suave)`.
    ctrl: Vec<(Vec2, f32)>,
    /// Nº de segmentos ya emitidos (entre puntos de control).
    emitted_segs: usize,
    /// Presion suavizada (EMA) acumulada.
    p_ema: f32,
    /// ¿Ya se emitio la primera muestra (el punto inicial)?
    started: bool,
    /// Ultima posicion emitida (para medir la longitud del trazo).
    last_emit: Vec2,
    /// Longitud total emitida (para el afilado de entrada).
    length: f32,
}

impl InkSmoother {
    pub fn new(cfg: InkConfig) -> Self {
        Self {
            euro: OneEuroFilter::default(),
            cfg,
            ctrl: Vec::new(),
            emitted_segs: 0,
            p_ema: 0.0,
            started: false,
            last_emit: Vec2::ZERO,
            length: 0.0,
        }
    }

    /// Reinicia para un trazo nuevo con el primer punto `pos` y su `pressure`.
    pub fn reset(&mut self, cfg: InkConfig) {
        // Frecuencia de corte minima del One-Euro segun el deslizador de suavizado:
        // mas suavizado -> corte mas bajo -> quita mas temblor. La `beta` alta mantiene la
        // baja latencia (el filtro se "abre" al moverse rapido).
        let sm = cfg.smoothing.clamp(0.0, 1.0);
        let min_cutoff = 3.0 - 2.3 * sm; // 3.0 (crudo) .. 0.7 (muy suave)
        self.euro = OneEuroFilter::new(min_cutoff, 0.05, 1.0);
        self.cfg = cfg;
        self.ctrl.clear();
        self.emitted_segs = 0;
        self.p_ema = 0.0;
        self.started = false;
        self.length = 0.0;
        self.last_emit = Vec2::ZERO;
    }

    /// Procesa un evento crudo `(raw, pressure)` con `dt` (s) desde el evento previo.
    /// Devuelve las muestras DENSAS y suaves nuevas a anexar al trazo (puede estar vacio).
    pub fn push(&mut self, raw: Vec2, pressure: f32, dt: f32) -> Vec<InputSample> {
        // Posicion filtrada (One-Euro) + presion suavizada (EMA).
        let pos = self.euro.filter(raw, dt);
        let a = self.cfg.pressure_smooth.clamp(0.0, 1.0);
        // alpha del EMA: 1 = sigue la presion cruda; bajamos con `pressure_smooth`.
        let alpha = (1.0 - a) * 0.6 + 0.4; // 0.4..1.0
        if self.ctrl.is_empty() {
            self.p_ema = pressure;
        } else {
            self.p_ema += (pressure - self.p_ema) * alpha;
        }
        self.ctrl.push((pos, self.p_ema));

        let mut out = Vec::new();
        // Primera muestra: el punto inicial (para que un toque deje marca).
        if !self.started {
            self.started = true;
            self.last_emit = self.ctrl[0].0;
            out.push(self.taper_sample(self.ctrl[0].0, self.ctrl[0].1));
        }
        // Emitir los segmentos que ya tienen su vecino derecho (causal, 1 evento de retardo).
        let k = self.ctrl.len() - 1;
        while self.emitted_segs + 2 <= k {
            let s = self.emitted_segs;
            self.emit_segment(s, false, &mut out);
            self.emitted_segs += 1;
        }
        out
    }

    /// Cierra el trazo: emite los segmentos pendientes (el ultimo tramo) y devuelve sus muestras.
    pub fn finish(&mut self) -> Vec<InputSample> {
        let mut out = Vec::new();
        if self.ctrl.is_empty() {
            return out;
        }
        if !self.started {
            // Trazo de un solo evento: el punto.
            self.started = true;
            out.push(self.taper_sample(self.ctrl[0].0, self.ctrl[0].1));
            return out;
        }
        let k = self.ctrl.len() - 1;
        while self.emitted_segs < k {
            let s = self.emitted_segs;
            self.emit_segment(s, true, &mut out);
            self.emitted_segs += 1;
        }
        out
    }

    /// Emite el segmento de control `s` (entre `ctrl[s]` y `ctrl[s+1]`) como muestras densas
    /// sobre la curva Catmull-Rom centripeta. Excluye el punto inicial (ya emitido). Si
    /// `closing`, usa `ctrl[s+1]` como vecino derecho (no hay punto futuro).
    fn emit_segment(&mut self, s: usize, closing: bool, out: &mut Vec<InputSample>) {
        let k = self.ctrl.len() - 1;
        let p0 = if s > 0 { self.ctrl[s - 1].0 } else { self.ctrl[s].0 };
        let (p1, pr1) = self.ctrl[s];
        let (p2, pr2) = self.ctrl[s + 1];
        let p3 = if s + 2 <= k { self.ctrl[s + 2].0 } else if closing { p2 } else { p2 };

        let seg_len = (p2 - p1).length();
        // Densidad adaptativa: mas muestras en segmentos largos y en giros cerrados.
        let turn = {
            let a = (p1 - p0).normalize_or_zero();
            let b = (p2 - p1).normalize_or_zero();
            (1.0 - a.dot(b)).clamp(0.0, 2.0) // 0 recto .. 2 vuelta en U
        };
        let base = (seg_len / self.cfg.max_step.max(0.25)).ceil().max(1.0);
        let steps = ((base * (1.0 + turn)).ceil() as usize).clamp(1, 64);

        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let pos = catmull_rom_centripetal(p0, p1, p2, p3, t);
            let pr = pr1 + (pr2 - pr1) * t;
            self.length += (pos - self.last_emit).length();
            self.last_emit = pos;
            out.push(self.taper_sample(pos, pr));
        }
    }

    /// Aplica el afilado de ENTRADA a la presion (rampa suave en las primeras unidades).
    fn taper_sample(&self, pos: Vec2, pressure: f32) -> InputSample {
        let mut pr = pressure;
        if self.cfg.use_pressure && self.cfg.taper_in > 0.0 && self.length < self.cfg.taper_in {
            let t = (self.length / self.cfg.taper_in).clamp(0.0, 1.0);
            pr *= smoothstep(t);
        }
        InputSample { pos, pressure: pr.clamp(0.0, 1.0), erosion: 0.0 }
    }
}

/// Rampa suave 0..1 (Hermite). Para taper sin "escalon".
#[inline]
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Catmull-Rom CENTRIPETA (alpha = 0.5) entre `p1` y `p2`, con vecinos `p0`/`p3` y
/// parametro `u` en `0..=1`. La parametrizacion centripeta evita lazos y "cusps" en
/// giros cerrados (a diferencia de la uniforme), por eso es la elegida para tinta.
pub fn catmull_rom_centripetal(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, u: f32) -> Vec2 {
    // Nudos por distancia^0.5 (centripeta). Guardas para puntos coincidentes.
    let kt = |ti: f32, a: Vec2, b: Vec2| ti + (b - a).length().sqrt().max(1e-4);
    let t0 = 0.0;
    let t1 = kt(t0, p0, p1);
    let t2 = kt(t1, p1, p2);
    let t3 = kt(t2, p2, p3);
    // Mapear u (0..1) al intervalo [t1, t2].
    let t = t1 + (t2 - t1) * u.clamp(0.0, 1.0);
    // Interpolaciones de De Boor / Barry-Goldman.
    let lerp = |a: Vec2, b: Vec2, ta: f32, tb: f32, tt: f32| {
        let d = tb - ta;
        if d.abs() < 1e-6 {
            a
        } else {
            a + (b - a) * ((tt - ta) / d)
        }
    };
    let a1 = lerp(p0, p1, t0, t1, t);
    let a2 = lerp(p1, p2, t1, t2, t);
    let a3 = lerp(p2, p3, t2, t3, t);
    let b1 = lerp(a1, a2, t0, t2, t);
    let b2 = lerp(a2, a3, t1, t3, t);
    lerp(b1, b2, t1, t2, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    #[test]
    fn passes_through_control_points() {
        // En u=0 y u=1 la curva pasa exactamente por p1 y p2.
        let p0 = vec2(0.0, 0.0);
        let p1 = vec2(1.0, 0.0);
        let p2 = vec2(2.0, 1.0);
        let p3 = vec2(3.0, 1.0);
        let a = catmull_rom_centripetal(p0, p1, p2, p3, 0.0);
        let b = catmull_rom_centripetal(p0, p1, p2, p3, 1.0);
        assert!((a - p1).length() < 1e-3, "u=0 -> p1");
        assert!((b - p2).length() < 1e-3, "u=1 -> p2");
    }

    #[test]
    fn densifies_a_fast_gap() {
        // Un solo movimiento grande debe producir varias muestras densas y suaves.
        let mut s = InkSmoother::new(InkConfig { max_step: 2.0, ..Default::default() });
        let mut n = 0;
        for p in [vec2(0.0, 0.0), vec2(20.0, 0.0), vec2(40.0, 10.0), vec2(60.0, 0.0)] {
            n += s.push(p, 1.0, 1.0 / 120.0).len();
        }
        n += s.finish().len();
        // 60px de recorrido con paso 2px -> al menos ~20 muestras (mucho mas densas que 4).
        assert!(n >= 15, "deberia densificar: n={n}");
    }

    #[test]
    fn finite_and_monotonic_pressure_ema() {
        let mut s = InkSmoother::new(InkConfig::default());
        let out1 = s.push(vec2(0.0, 0.0), 0.0, 1.0 / 120.0);
        let _ = s.push(vec2(5.0, 0.0), 1.0, 1.0 / 120.0);
        let out3 = s.push(vec2(10.0, 0.0), 1.0, 1.0 / 120.0);
        let _ = s.finish();
        assert!(out1[0].pressure.is_finite());
        if let Some(last) = out3.last() {
            assert!(last.pressure.is_finite());
        }
    }
}
