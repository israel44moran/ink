//! Herramientas del lienzo (su "motor"): seleccion, empujar (smudge), sector
//! (lazo), mascaras (borrado duro/suave) y texto.
//!
//! La logica geometrica vive aqui (portable, sin GPU). El shell de cada
//! plataforma solo traduce los gestos del puntero a estas operaciones; el
//! `Document` (ver `document.rs`) aplica los cambios sobre los trazos.

use glam::Vec2;

/// Herramienta activa. Cada variante define un comportamiento real distinto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Seleccion por rectangulo (marquee) y mover lo seleccionado.
    Select,
    /// Empujar: desplaza ("smudge") las muestras cercanas al arrastrar.
    Push,
    /// Sector: seleccion a mano alzada (lazo poligonal) de trazos COMPLETOS.
    Sector,
    /// Lazo: selecciona a mano alzada RECORTANDO los trazos por el contorno; solo la parte
    /// DENTRO del area se selecciona (y se puede mover), no el trazo entero.
    Lasso,
    /// Lazo POLIGONAL: como el Lazo (recorta por el contorno) pero el contorno se traza con
    /// CLICS, en lineas rectas entre vertices (igual que el lazo poligonal de Photoshop).
    PolyLasso,
    /// Mascara dura: borra por completo los trazos que toca el trazo de borrado.
    MaskHard,
    /// Mascara suave: atenua (baja el alfa) de los trazos cercanos.
    MaskSoft,
    /// Texto: coloca y edita etiquetas de texto en el lienzo.
    Text,
}

/// Caja envolvente (AABB) en coordenadas de mundo.
#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: Vec2,
    pub max: Vec2,
}

impl Aabb {
    pub fn from_points(a: Vec2, b: Vec2) -> Self {
        Self { min: a.min(b), max: a.max(b) }
    }
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }
    pub fn intersects(&self, o: &Aabb) -> bool {
        self.min.x <= o.max.x && self.max.x >= o.min.x && self.min.y <= o.max.y && self.max.y >= o.min.y
    }
    /// Crece la caja para incluir un punto.
    pub fn expand(&mut self, p: Vec2) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }
    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }
}

/// Test punto-en-poligono por cruce de rayos (regla par/impar). Para el lazo.
pub fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let pi = poly[i];
        let pj = poly[j];
        if ((pi.y > p.y) != (pj.y > p.y))
            && (p.x < (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y) + pi.x)
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Parametro `t` en [0,1] del cruce del segmento `a`->`b` con el segmento `c`->`d`, o
/// `None` si no se cruzan dentro de ambos.
fn segment_segment_t(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<f32> {
    let r = b - a;
    let s = d - c;
    let denom = r.x * s.y - r.y * s.x;
    if denom.abs() < 1e-9 {
        return None; // paralelos
    }
    let ca = c - a;
    let t = (ca.x * s.y - ca.y * s.x) / denom;
    let u = (ca.x * r.y - ca.y * r.x) / denom;
    if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
        Some(t)
    } else {
        None
    }
}

/// Parametro `t` en [0,1] del PRIMER cruce del segmento `a`->`b` con el CONTORNO del
/// poligono `poly` (cualquiera de sus aristas). Sirve para recortar un trazo exactamente
/// en el borde de un lazo. `None` si el segmento no cruza el contorno.
pub fn segment_polygon_cross(a: Vec2, b: Vec2, poly: &[Vec2]) -> Option<f32> {
    let n = poly.len();
    if n < 2 {
        return None;
    }
    let mut best: Option<f32> = None;
    for j in 0..n {
        let c = poly[j];
        let d = poly[(j + 1) % n];
        if let Some(t) = segment_segment_t(a, b, c, d) {
            best = Some(best.map_or(t, |bt| bt.min(t)));
        }
    }
    best
}

/// Distancia minima de un punto al segmento `a`-`b` (para borrado/empujar).
pub fn dist_point_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-9 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Una etiqueta de texto colocada en el lienzo (coordenadas de mundo).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TextItem {
    pub pos: Vec2,
    pub content: String,
    /// Tamano de fuente en unidades de mundo (escala con el zoom).
    pub size: f32,
    pub color: [f32; 4],
}

impl TextItem {
    pub fn new(pos: Vec2, size: f32, color: [f32; 4]) -> Self {
        Self { pos, content: String::new(), size, color }
    }
}
