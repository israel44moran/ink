//! Filtro "One-Euro" (Casiez et al., 2012) para suavizar la entrada del lapiz.
//!
//! Es el estandar de oro en apps de dibujo: quita el temblor cuando la mano va
//! lenta, pero deja pasar el movimiento rapido sin agregar retraso perceptible.
//! Esto es justo lo que necesitamos para que la tinta se sienta "pegada" a la punta.

use glam::Vec2;

/// Paso bajo exponencial de un punto 2D.
#[derive(Clone, Copy)]
struct LowPass {
    y: Vec2,
    initialized: bool,
}

impl LowPass {
    fn new() -> Self {
        Self { y: Vec2::ZERO, initialized: false }
    }

    fn filter(&mut self, x: Vec2, alpha: f32) -> Vec2 {
        if self.initialized {
            self.y = alpha * x + (1.0 - alpha) * self.y;
        } else {
            self.y = x;
            self.initialized = true;
        }
        self.y
    }
}

/// `alpha` del paso bajo en funcion de la frecuencia de corte y el delta de tiempo.
#[inline]
fn smoothing_alpha(cutoff: f32, dt: f32) -> f32 {
    let tau = 1.0 / (core::f32::consts::TAU * cutoff);
    1.0 / (1.0 + tau / dt)
}

/// Filtro One-Euro para puntos 2D.
pub struct OneEuroFilter {
    /// Frecuencia de corte minima: a menor valor, mas suavizado en reposo.
    min_cutoff: f32,
    /// Cuanto se "abre" el filtro con la velocidad: a mayor valor, menos lag al moverse rapido.
    beta: f32,
    /// Frecuencia de corte para la derivada (velocidad).
    dcutoff: f32,
    x: LowPass,
    dx: LowPass,
    last: Option<Vec2>,
}

impl OneEuroFilter {
    pub fn new(min_cutoff: f32, beta: f32, dcutoff: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            dcutoff,
            x: LowPass::new(),
            dx: LowPass::new(),
            last: None,
        }
    }

    /// Reinicia el estado (llamar al empezar un trazo nuevo).
    pub fn reset(&mut self) {
        self.x = LowPass::new();
        self.dx = LowPass::new();
        self.last = None;
    }

    /// Filtra `value` dado el delta de tiempo `dt` (en segundos) desde la muestra previa.
    pub fn filter(&mut self, value: Vec2, dt: f32) -> Vec2 {
        let dt = dt.max(1e-4);
        let prev = self.last.unwrap_or(value);
        let dvalue = (value - prev) / dt;
        self.last = Some(value);

        let edvalue = self.dx.filter(dvalue, smoothing_alpha(self.dcutoff, dt));
        let cutoff = self.min_cutoff + self.beta * edvalue.length();
        self.x.filter(value, smoothing_alpha(cutoff, dt))
    }
}

impl Default for OneEuroFilter {
    /// Parametros afinados para escritura a mano: responde rapido, tiembla poco.
    fn default() -> Self {
        Self::new(1.7, 0.015, 1.0)
    }
}
