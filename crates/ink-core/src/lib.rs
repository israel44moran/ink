//! # ink-core
//!
//! Nucleo del motor de tinta. Es 100% portable (sin dependencias de GPU ni de
//! plataforma) para poder compilarse igual en Windows, Android e iPad. Aqui vive
//! la parte dificil y critica en rendimiento:
//!
//! - [`stroke`]   : modelo de trazos + teselado de ancho variable (presion -> grosor).
//! - [`smoothing`]: filtro One-Euro para quitar el "temblor" del lapiz con baja latencia.
//! - [`camera`]   : transformaciones del lienzo infinito (pan / zoom).
//! - [`document`] : el documento (coleccion de trazos + malla horneada).
//!
//! El render (wgpu) vive en la capa de aplicacion; este crate solo *produce* la
//! geometria ([`Vertex`]) que la GPU dibuja.

pub mod camera;
pub mod document;
pub mod grid;
pub mod smoothing;
pub mod stroke;
pub mod tools;

pub use glam::{vec2, Vec2};

pub use camera::Camera;
pub use document::Document;
pub use grid::{build_grid, GridKind};
pub use smoothing::OneEuroFilter;
pub use stroke::{tessellate_stroke, Brush, BrushKind, InputSample, Stroke, Vertex};
pub use tools::{Aabb, TextItem, Tool};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_screen_world_roundtrip() {
        let mut cam = Camera::new(vec2(800.0, 600.0));
        cam.zoom = 2.5;
        cam.center = vec2(120.0, -40.0);
        let p = vec2(345.0, 210.0);
        let w = cam.screen_to_world(p);
        // El centro de pantalla debe mapear al centro del mundo.
        let mid = cam.screen_to_world(cam.viewport * 0.5);
        assert!((mid - cam.center).length() < 1e-3);
        // Mover 'zoom' pixeles en pantalla = mover 1 unidad de mundo.
        let w2 = cam.screen_to_world(p + vec2(cam.zoom, 0.0));
        assert!((w2.x - w.x - 1.0).abs() < 1e-3);
    }

    #[test]
    fn tessellation_produces_triangles() {
        let brush = Brush::default();
        let samples = vec![
            InputSample { pos: vec2(0.0, 0.0), pressure: 1.0 },
            InputSample { pos: vec2(10.0, 0.0), pressure: 1.0 },
            InputSample { pos: vec2(20.0, 5.0), pressure: 0.5 },
        ];
        let mut out = Vec::new();
        tessellate_stroke(&samples, &brush, &mut out);
        assert!(out.len() % 3 == 0, "deben salir triangulos completos");
        assert!(!out.is_empty());
    }

    #[test]
    fn document_undo_rebuilds_mesh() {
        let mut doc = Document::new();
        let mut s = Stroke::new(Brush::default());
        s.push(InputSample { pos: vec2(0.0, 0.0), pressure: 1.0 });
        s.push(InputSample { pos: vec2(5.0, 0.0), pressure: 1.0 });
        doc.add_stroke(s);
        assert!(doc.vertex_count() > 0);
        doc.undo();
        assert_eq!(doc.stroke_count(), 0);
        assert_eq!(doc.vertex_count(), 0);
    }

    #[test]
    fn one_euro_outputs_finite() {
        let mut f = OneEuroFilter::default();
        let mut p = f.filter(vec2(0.0, 0.0), 1.0 / 120.0);
        for i in 1..50 {
            p = f.filter(vec2(i as f32, (i as f32 * 0.3).sin()), 1.0 / 120.0);
        }
        assert!(p.x.is_finite() && p.y.is_finite());
    }

    // --- Motores de herramientas ---

    fn line_stroke(a: Vec2, b: Vec2) -> Stroke {
        let mut s = Stroke::new(Brush::default());
        s.push(InputSample { pos: a, pressure: 1.0 });
        s.push(InputSample { pos: b, pressure: 1.0 });
        s
    }

    #[test]
    fn tool_select_rect_and_polygon() {
        let mut doc = Document::new();
        doc.add_stroke(line_stroke(vec2(0.0, 0.0), vec2(10.0, 0.0))); // 0
        doc.add_stroke(line_stroke(vec2(100.0, 100.0), vec2(110.0, 100.0))); // 1
        // Rectangulo que solo cubre el primer trazo.
        let sel = doc.strokes_in_rect(vec2(-5.0, -5.0), vec2(20.0, 20.0));
        assert_eq!(sel, vec![0]);
        // Poligono (cuadrado) alrededor del segundo trazo.
        let poly = [vec2(90.0, 90.0), vec2(120.0, 90.0), vec2(120.0, 120.0), vec2(90.0, 120.0)];
        assert_eq!(doc.strokes_in_polygon(&poly), vec![1]);
    }

    #[test]
    fn tool_translate_moves_samples() {
        let mut doc = Document::new();
        doc.add_stroke(line_stroke(vec2(0.0, 0.0), vec2(10.0, 0.0)));
        doc.translate_strokes(&[0], vec2(5.0, -3.0));
        let s = &doc.strokes[0];
        assert!((s.samples[0].pos - vec2(5.0, -3.0)).length() < 1e-4);
        assert!((s.samples[1].pos - vec2(15.0, -3.0)).length() < 1e-4);
    }

    #[test]
    fn tool_erase_hard_removes_touched() {
        let mut doc = Document::new();
        doc.add_stroke(line_stroke(vec2(0.0, 0.0), vec2(10.0, 0.0))); // sera borrado
        doc.add_stroke(line_stroke(vec2(0.0, 50.0), vec2(10.0, 50.0))); // intacto
        let removed = doc.erase_hard(vec2(5.0, 0.0), vec2(5.0, 0.0), 3.0);
        assert_eq!(removed, 1);
        assert_eq!(doc.stroke_count(), 1);
        // El que queda es el de y=50.
        assert!((doc.strokes[0].samples[0].pos.y - 50.0).abs() < 1e-4);
    }

    #[test]
    fn tool_smudge_displaces_near_only() {
        let mut doc = Document::new();
        doc.add_stroke(line_stroke(vec2(0.0, 0.0), vec2(100.0, 0.0)));
        // Empujar cerca del extremo izquierdo.
        let changed = doc.smudge(vec2(0.0, 0.0), vec2(0.0, 20.0), 10.0);
        assert!(changed);
        let s = &doc.strokes[0];
        assert!(s.samples[0].pos.y > 1.0, "la muestra cercana se desplaza");
        assert!(s.samples[1].pos.y.abs() < 1e-3, "la muestra lejana no se mueve");
    }
}
