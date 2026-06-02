//! El documento por CAPAS (estilo Photoshop).
//!
//! Cada capa tiene sus propios trazos, nombre, visibilidad y opacidad. La malla
//! "horneada" (`committed`) concatena los trazos de las capas VISIBLES en orden
//! (fondo -> frente) aplicando la opacidad de cada capa. Las herramientas
//! (seleccion, borrar, mover, empujar) operan sobre la capa ACTIVA.

use crate::stroke::{tessellate_stroke, Stroke, Vertex};
use crate::tools::{dist_point_segment, point_in_polygon, Aabb};
use glam::Vec2;

/// Una capa del documento.
#[derive(Clone)]
pub struct Layer {
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub locked: bool,
    pub strokes: Vec<Stroke>,
}

impl Layer {
    fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), visible: true, opacity: 1.0, locked: false, strokes: Vec::new() }
    }
}

pub struct Document {
    pub layers: Vec<Layer>,
    pub active: usize,
    committed: Vec<Vertex>,
    /// Pila de rehacer: (indice de capa, trazo deshecho).
    undone: Vec<(usize, Stroke)>,
}

impl Default for Document {
    fn default() -> Self {
        Self { layers: vec![Layer::new("Capa 1")], active: 0, committed: Vec::new(), undone: Vec::new() }
    }
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    fn ai(&self) -> usize {
        self.active.min(self.layers.len().saturating_sub(1))
    }
    fn active_layer(&self) -> &Layer {
        &self.layers[self.ai()]
    }
    fn active_layer_mut(&mut self) -> &mut Layer {
        let i = self.ai();
        &mut self.layers[i]
    }

    /// Trazos de la capa activa (para las herramientas y los tests).
    pub fn strokes(&self) -> &[Stroke] {
        &self.active_layer().strokes
    }

    // ------------------------------- Capas -------------------------------

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Anade una capa nueva ENCIMA de la activa y la vuelve activa.
    pub fn add_layer(&mut self) {
        let n = self.layers.len() + 1;
        let i = (self.active + 1).min(self.layers.len());
        self.layers.insert(i, Layer::new(format!("Capa {}", n)));
        self.active = i;
        self.undone.clear();
    }

    /// Duplica la capa `i`.
    pub fn duplicate_layer(&mut self, i: usize) {
        if let Some(src) = self.layers.get(i).cloned() {
            let mut dup = src;
            dup.name = format!("{} copia", dup.name);
            self.layers.insert(i + 1, dup);
            self.active = i + 1;
            self.undone.clear();
            self.rebuild();
        }
    }

    /// Elimina la capa `i` (no si es la unica).
    pub fn delete_layer(&mut self, i: usize) {
        if self.layers.len() <= 1 || i >= self.layers.len() {
            return;
        }
        self.layers.remove(i);
        if self.active >= self.layers.len() {
            self.active = self.layers.len() - 1;
        }
        self.undone.clear();
        self.rebuild();
    }

    /// Mueve la capa `i` hacia el frente (up = true) o hacia el fondo.
    pub fn move_layer(&mut self, i: usize, up: bool) {
        let n = self.layers.len();
        let j = if up && i + 1 < n {
            i + 1
        } else if !up && i > 0 {
            i - 1
        } else {
            return;
        };
        self.layers.swap(i, j);
        if self.active == i {
            self.active = j;
        } else if self.active == j {
            self.active = i;
        }
        self.rebuild();
    }

    pub fn set_active(&mut self, i: usize) {
        if i < self.layers.len() {
            self.active = i;
        }
    }

    pub fn set_visible(&mut self, i: usize, v: bool) {
        if let Some(l) = self.layers.get_mut(i) {
            l.visible = v;
            self.rebuild();
        }
    }

    pub fn set_layer_opacity(&mut self, i: usize, op: f32) {
        if let Some(l) = self.layers.get_mut(i) {
            l.opacity = op.clamp(0.0, 1.0);
            self.rebuild();
        }
    }

    pub fn set_locked(&mut self, i: usize, locked: bool) {
        if let Some(l) = self.layers.get_mut(i) {
            l.locked = locked;
        }
    }

    pub fn rename_layer(&mut self, i: usize, name: String) {
        if let Some(l) = self.layers.get_mut(i) {
            l.name = name;
        }
    }

    pub fn active_locked(&self) -> bool {
        self.active_layer().locked
    }

    // ------------------------------ Trazos -------------------------------

    /// Confirma un trazo nuevo en la capa activa (si no esta bloqueada).
    pub fn add_stroke(&mut self, stroke: Stroke) {
        if self.active_layer().locked {
            return;
        }
        self.undone.clear();
        self.active_layer_mut().strokes.push(stroke);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.committed.clear();
        let mut tmp = Vec::new();
        for layer in &self.layers {
            if !layer.visible {
                continue;
            }
            tmp.clear();
            for s in &layer.strokes {
                tessellate_stroke(&s.samples, &s.brush, &mut tmp);
            }
            if layer.opacity < 0.999 {
                for v in tmp.iter_mut() {
                    v.color[3] *= layer.opacity;
                }
            }
            self.committed.extend_from_slice(&tmp);
        }
    }

    pub fn committed_vertices(&self) -> &[Vertex] {
        &self.committed
    }

    pub fn vertex_count(&self) -> usize {
        self.committed.len()
    }

    /// Total de trazos en todas las capas.
    pub fn stroke_count(&self) -> usize {
        self.layers.iter().map(|l| l.strokes.len()).sum()
    }

    /// Color del trazo visible mas al frente que pasa por `p` (para el cuentagotas).
    pub fn color_at(&self, p: Vec2) -> Option<[f32; 4]> {
        for layer in self.layers.iter().rev() {
            if !layer.visible {
                continue;
            }
            for s in layer.strokes.iter().rev() {
                let hw = (s.brush.width * 0.5).max(2.0);
                if s.samples.iter().any(|sm| (sm.pos - p).length() <= hw) {
                    let mut c = s.brush.color;
                    c[3] = 1.0;
                    return Some(c);
                }
            }
        }
        None
    }

    // ----------------- Herramientas (sobre la capa activa) ----------------

    pub fn stroke_bounds(&self, id: usize) -> Option<Aabb> {
        let s = self.active_layer().strokes.get(id)?;
        let first = s.samples.first()?;
        let hw = (s.brush.width * 0.5).max(1.0);
        let mut bb = Aabb::from_points(first.pos, first.pos);
        for sm in &s.samples {
            bb.expand(sm.pos);
        }
        bb.min -= Vec2::splat(hw);
        bb.max += Vec2::splat(hw);
        Some(bb)
    }

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

    pub fn stroke_at(&self, p: Vec2, radius: f32) -> Option<usize> {
        let mut best = None;
        let mut bd = radius;
        for (i, s) in self.active_layer().strokes.iter().enumerate() {
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

    pub fn strokes_in_rect(&self, a: Vec2, b: Vec2) -> Vec<usize> {
        let rect = Aabb::from_points(a, b);
        (0..self.active_layer().strokes.len())
            .filter(|&i| self.stroke_bounds(i).map_or(false, |bb| bb.intersects(&rect)))
            .collect()
    }

    pub fn strokes_in_polygon(&self, poly: &[Vec2]) -> Vec<usize> {
        if poly.len() < 3 {
            return Vec::new();
        }
        self.active_layer()
            .strokes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.samples.iter().any(|sm| point_in_polygon(sm.pos, poly)))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn translate_strokes(&mut self, ids: &[usize], delta: Vec2) {
        if delta == Vec2::ZERO {
            return;
        }
        {
            let layer = self.active_layer_mut();
            for &id in ids {
                if let Some(s) = layer.strokes.get_mut(id) {
                    for sm in s.samples.iter_mut() {
                        sm.pos += delta;
                    }
                }
            }
        }
        self.rebuild();
    }

    pub fn erase_hard(&mut self, a: Vec2, b: Vec2, radius: f32) -> usize {
        let removed;
        {
            let layer = self.active_layer_mut();
            let before = layer.strokes.len();
            let strokes = std::mem::take(&mut layer.strokes);
            layer.strokes = strokes
                .into_iter()
                .filter(|s| {
                    let hw = (s.brush.width * 0.5).max(1.0);
                    !s.samples.iter().any(|sm| dist_point_segment(sm.pos, a, b) <= radius + hw)
                })
                .collect();
            removed = before - layer.strokes.len();
        }
        if removed > 0 {
            self.undone.clear();
            self.rebuild();
        }
        removed
    }

    pub fn erase_soft(&mut self, a: Vec2, b: Vec2, radius: f32, amount: f32) -> bool {
        let mut changed = false;
        let mut emptied = false;
        {
            let layer = self.active_layer_mut();
            for s in layer.strokes.iter_mut() {
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
                layer.strokes.retain(|s| s.brush.color[3] > 0.02);
            }
        }
        if changed {
            self.undone.clear();
            self.rebuild();
        }
        changed
    }

    pub fn smudge(&mut self, p: Vec2, delta: Vec2, radius: f32) -> bool {
        if delta.length_squared() < 1e-9 {
            return false;
        }
        let r2 = radius * radius;
        let mut changed = false;
        {
            let layer = self.active_layer_mut();
            for s in layer.strokes.iter_mut() {
                for sm in s.samples.iter_mut() {
                    let d2 = (sm.pos - p).length_squared();
                    if d2 <= r2 {
                        let f = 1.0 - (d2 / r2).sqrt();
                        let f = f * f * (3.0 - 2.0 * f); // smoothstep
                        sm.pos += delta * f;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.rebuild();
        }
        changed
    }

    pub fn can_undo(&self) -> bool {
        !self.active_layer().strokes.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        self.undone.iter().any(|(li, _)| *li == self.active)
    }

    /// Borra TODO (todas las capas) y deja una capa vacia.
    pub fn clear(&mut self) {
        self.layers = vec![Layer::new("Capa 1")];
        self.active = 0;
        self.committed.clear();
        self.undone.clear();
    }

    /// Deshace el ultimo trazo de la capa activa.
    pub fn undo(&mut self) {
        let i = self.active;
        if let Some(s) = self.layers.get_mut(i).and_then(|l| l.strokes.pop()) {
            self.undone.push((i, s));
            self.rebuild();
        }
    }

    /// Rehace el ultimo trazo deshecho de la capa activa.
    pub fn redo(&mut self) {
        if let Some(pos) = self.undone.iter().rposition(|(li, _)| *li == self.active) {
            let (li, s) = self.undone.remove(pos);
            if let Some(l) = self.layers.get_mut(li) {
                l.strokes.push(s);
            }
            self.rebuild();
        }
    }
}
