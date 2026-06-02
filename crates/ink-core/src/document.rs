//! El documento: la coleccion de trazos confirmados y su malla "horneada".
//!
//! Cuando un trazo se confirma, se tesela UNA vez y sus triangulos se anexan a un
//! buffer plano (`committed`). Asi en cada frame solo subimos/redibujamos geometria
//! ya calculada; el trazo activo se dibuja aparte y en vivo.
//!
//! NOTA de rendimiento (v0.1): aqui guardamos todos los trazos en un unico buffer.
//! El siguiente paso de optimizacion sera un indice espacial por mosaicos (tiles)
//! con cache de textura por mosaico, para el lienzo verdaderamente infinito.

use crate::stroke::{tessellate_stroke, Stroke, Vertex};
use crate::tools::{dist_point_segment, point_in_polygon, Aabb};
use glam::Vec2;

#[derive(Default)]
pub struct Document {
    pub strokes: Vec<Stroke>,
    committed: Vec<Vertex>,
    /// Pila de rehacer: trazos deshechos, listos para volver.
    undone: Vec<Stroke>,
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    /// Confirma un trazo nuevo: lo hornea y limpia la pila de rehacer.
    pub fn add_stroke(&mut self, stroke: Stroke) {
        self.undone.clear();
        self.bake(&stroke);
        self.strokes.push(stroke);
    }

    fn bake(&mut self, stroke: &Stroke) {
        tessellate_stroke(&stroke.samples, &stroke.brush, &mut self.committed);
    }

    fn rebuild(&mut self) {
        self.committed.clear();
        for s in &self.strokes {
            tessellate_stroke(&s.samples, &s.brush, &mut self.committed);
        }
    }

    /// Geometria de todos los trazos confirmados (para subir a la GPU).
    pub fn committed_vertices(&self) -> &[Vertex] {
        &self.committed
    }

    pub fn vertex_count(&self) -> usize {
        self.committed.len()
    }

    pub fn stroke_count(&self) -> usize {
        self.strokes.len()
    }

    /// Color del trazo mas reciente que pasa por el punto `p` (para el cuentagotas).
    pub fn color_at(&self, p: Vec2) -> Option<[f32; 4]> {
        for s in self.strokes.iter().rev() {
            let hw = (s.brush.width * 0.5).max(2.0);
            for sample in &s.samples {
                if (sample.pos - p).length() <= hw {
                    let mut c = s.brush.color;
                    c[3] = 1.0; // devolver opaco
                    return Some(c);
                }
            }
        }
        None
    }

    // ---------------------------------------------------------------------
    // Operaciones de herramientas (seleccion, mover, borrar, empujar).
    // ---------------------------------------------------------------------

    /// Caja envolvente de un trazo (None si no tiene muestras).
    pub fn stroke_bounds(&self, id: usize) -> Option<Aabb> {
        let s = self.strokes.get(id)?;
        let mut it = s.samples.iter();
        let first = it.next()?;
        let hw = (s.brush.width * 0.5).max(1.0);
        let mut bb = Aabb::from_points(first.pos, first.pos);
        for sm in s.samples.iter() {
            bb.expand(sm.pos);
        }
        bb.min -= Vec2::splat(hw);
        bb.max += Vec2::splat(hw);
        Some(bb)
    }

    /// Caja que engloba a un conjunto de trazos.
    pub fn bounds_of(&self, ids: &[usize]) -> Option<Aabb> {
        let mut acc: Option<Aabb> = None;
        for &id in ids {
            if let Some(b) = self.stroke_bounds(id) {
                acc = Some(match acc {
                    None => b,
                    Some(mut a) => {
                        a.expand(b.min);
                        a.expand(b.max);
                        a
                    }
                });
            }
        }
        acc
    }

    /// Indice del trazo mas cercano cuyo trazo pasa a <= `radius` de `p`.
    pub fn stroke_at(&self, p: Vec2, radius: f32) -> Option<usize> {
        let mut best = None;
        let mut bd = radius;
        for (i, s) in self.strokes.iter().enumerate() {
            let hw = (s.brush.width * 0.5).max(1.0);
            for w in s.samples.windows(2) {
                let d = dist_point_segment(p, w[0].pos, w[1].pos) - hw;
                if d < bd {
                    bd = d;
                    best = Some(i);
                }
            }
            if let [only] = s.samples.as_slice() {
                let d = (p - only.pos).length() - hw;
                if d < bd {
                    bd = d;
                    best = Some(i);
                }
            }
        }
        best
    }

    /// Indices de los trazos cuyo AABB intersecta el rectangulo (marquee).
    pub fn strokes_in_rect(&self, a: Vec2, b: Vec2) -> Vec<usize> {
        let rect = Aabb::from_points(a, b);
        (0..self.strokes.len())
            .filter(|&i| self.stroke_bounds(i).map_or(false, |bb| bb.intersects(&rect)))
            .collect()
    }

    /// Indices de los trazos con alguna muestra dentro del poligono (lazo/sector).
    pub fn strokes_in_polygon(&self, poly: &[Vec2]) -> Vec<usize> {
        if poly.len() < 3 {
            return Vec::new();
        }
        self.strokes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.samples.iter().any(|sm| point_in_polygon(sm.pos, poly)))
            .map(|(i, _)| i)
            .collect()
    }

    /// Desplaza los trazos indicados por `delta` (mundo) y rehornea.
    pub fn translate_strokes(&mut self, ids: &[usize], delta: Vec2) {
        if delta == Vec2::ZERO {
            return;
        }
        for &id in ids {
            if let Some(s) = self.strokes.get_mut(id) {
                for sm in s.samples.iter_mut() {
                    sm.pos += delta;
                }
            }
        }
        self.rebuild();
    }

    /// Borrado "duro" (mascara dura): elimina los trazos que el segmento de
    /// borrado `a`-`b` toca dentro de `radius`. Devuelve cuantos borro.
    pub fn erase_hard(&mut self, a: Vec2, b: Vec2, radius: f32) -> usize {
        let before = self.strokes.len();
        let strokes = std::mem::take(&mut self.strokes);
        self.strokes = strokes
            .into_iter()
            .filter(|s| {
                let hw = (s.brush.width * 0.5).max(1.0);
                let hit = s.samples.iter().any(|sm| dist_point_segment(sm.pos, a, b) <= radius + hw);
                !hit
            })
            .collect();
        let removed = before - self.strokes.len();
        if removed > 0 {
            self.undone.clear();
            self.rebuild();
        }
        removed
    }

    /// Borrado "suave" (mascara suave): baja el alfa de los trazos cercanos al
    /// segmento `a`-`b`. Devuelve `true` si cambio algo.
    pub fn erase_soft(&mut self, a: Vec2, b: Vec2, radius: f32, amount: f32) -> bool {
        let mut changed = false;
        let mut emptied = false;
        for s in self.strokes.iter_mut() {
            let hw = (s.brush.width * 0.5).max(1.0);
            let near = s.samples.iter().any(|sm| dist_point_segment(sm.pos, a, b) <= radius + hw);
            if near {
                let na = (s.brush.color[3] - amount).max(0.0);
                if (na - s.brush.color[3]).abs() > 1e-4 {
                    s.brush.color[3] = na;
                    changed = true;
                    if na <= 0.02 {
                        emptied = true;
                    }
                }
            }
        }
        if emptied {
            self.strokes.retain(|s| s.brush.color[3] > 0.02);
        }
        if changed {
            self.undone.clear();
            self.rebuild();
        }
        changed
    }

    /// Empujar (smudge): desplaza las muestras dentro de `radius` de `p` en la
    /// direccion `delta`, con caida suave segun la distancia. Rehornea.
    pub fn smudge(&mut self, p: Vec2, delta: Vec2, radius: f32) -> bool {
        if delta.length_squared() < 1e-9 {
            return false;
        }
        let r2 = radius * radius;
        let mut changed = false;
        for s in self.strokes.iter_mut() {
            for sm in s.samples.iter_mut() {
                let d2 = (sm.pos - p).length_squared();
                if d2 <= r2 {
                    // Caida suave (1 en el centro, 0 en el borde).
                    let f = 1.0 - (d2 / r2).sqrt();
                    let f = f * f * (3.0 - 2.0 * f); // smoothstep
                    sm.pos += delta * f;
                    changed = true;
                }
            }
        }
        if changed {
            self.rebuild();
        }
        changed
    }

    pub fn can_undo(&self) -> bool {
        !self.strokes.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
        self.committed.clear();
        self.undone.clear();
    }

    /// Deshace el ultimo trazo (lo manda a la pila de rehacer).
    pub fn undo(&mut self) {
        if let Some(s) = self.strokes.pop() {
            self.undone.push(s);
            self.rebuild();
        }
    }

    /// Rehace el ultimo trazo deshecho.
    pub fn redo(&mut self) {
        if let Some(s) = self.undone.pop() {
            self.bake(&s);
            self.strokes.push(s);
        }
    }
}
