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
pub mod stamping;
pub mod stroke;
pub mod tools;

pub use glam::{vec2, Vec2};

pub use camera::Camera;
pub use document::{Document, Layer};
pub use grid::{build_grid, GridKind};
pub use smoothing::OneEuroFilter;
pub use stamping::{push_stamp_quad, stamp_path, BlendMode, BrushSettings, DynControl, Stamp, StampOutput, StampVertex, TipKind};
pub use stroke::{tessellate_incremental, tessellate_stroke, Brush, BrushKind, InputSample, Stroke, Vertex};
pub use tools::{point_in_polygon, Aabb, TextItem, Tool};

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
            InputSample { pos: vec2(0.0, 0.0), pressure: 1.0, erosion: 0.0 },
            InputSample { pos: vec2(10.0, 0.0), pressure: 1.0, erosion: 0.0 },
            InputSample { pos: vec2(20.0, 5.0), pressure: 0.5, erosion: 0.0 },
        ];
        let mut out = Vec::new();
        tessellate_stroke(&samples, &brush, &mut out);
        assert!(out.len() % 3 == 0, "deben salir triangulos completos");
        assert!(!out.is_empty());
    }

    #[test]
    fn incremental_tessellation_matches_full() {
        // El teselado incremental (punto a punto, en vivo) debe producir EXACTAMENTE
        // la misma geometria que el teselado completo (mismo multiset de vertices).
        let kinds = [
            BrushKind::Pen,
            BrushKind::FixedWidth,
            BrushKind::Marker,
            BrushKind::Pencil,
            BrushKind::Watercolor,
            BrushKind::Airbrush,
        ];
        let pts = [
            vec2(0.0, 0.0),
            vec2(10.0, 2.0),
            vec2(22.0, -3.0),
            vec2(30.0, 5.0),
            vec2(45.0, 0.5),
        ];
        let key = |v: &Vertex| {
            (
                v.pos[0].to_bits(),
                v.pos[1].to_bits(),
                v.color[0].to_bits(),
                v.color[1].to_bits(),
                v.color[2].to_bits(),
                v.color[3].to_bits(),
            )
        };
        for kind in kinds {
            let brush = Brush { kind, ..Brush::default() };
            // Completo.
            let mut s = Stroke::new(brush);
            for p in pts {
                s.push(InputSample { pos: p, pressure: 0.8, erosion: 0.0 });
            }
            let mut full = Vec::new();
            s.tessellate(&mut full);
            // Incremental: agregar muestra por muestra.
            let mut samples = Vec::new();
            let mut inc = Vec::new();
            for p in pts {
                samples.push(InputSample { pos: p, pressure: 0.8, erosion: 0.0 });
                assert!(
                    tessellate_incremental(&samples, &brush, &mut inc),
                    "{:?} deberia soportar incremental",
                    kind
                );
            }
            assert_eq!(full.len(), inc.len(), "conteo de vertices difiere en {:?}", kind);
            let mut a: Vec<_> = full.iter().map(key).collect();
            let mut b: Vec<_> = inc.iter().map(key).collect();
            a.sort();
            b.sort();
            assert_eq!(a, b, "geometria (multiset) difiere en {:?}", kind);
        }
    }

    #[test]
    fn document_undo_rebuilds_mesh() {
        let mut doc = Document::new();
        let mut s = Stroke::new(Brush::default());
        s.push(InputSample { pos: vec2(0.0, 0.0), pressure: 1.0, erosion: 0.0 });
        s.push(InputSample { pos: vec2(5.0, 0.0), pressure: 1.0, erosion: 0.0 });
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
        s.push(InputSample { pos: a, pressure: 1.0, erosion: 0.0 });
        s.push(InputSample { pos: b, pressure: 1.0, erosion: 0.0 });
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
        let s = &doc.strokes()[0];
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
        assert!((doc.strokes()[0].samples[0].pos.y - 50.0).abs() < 1e-4);
    }

    #[test]
    fn tool_smudge_displaces_near_only() {
        let mut doc = Document::new();
        doc.add_stroke(line_stroke(vec2(0.0, 0.0), vec2(100.0, 0.0)));
        // Empujar cerca del extremo izquierdo.
        let changed = doc.smudge(vec2(0.0, 0.0), vec2(0.0, 20.0), 10.0);
        assert!(changed);
        let s = &doc.strokes()[0];
        assert!(s.samples[0].pos.y > 1.0, "la muestra cercana se desplaza");
        assert!(s.samples[1].pos.y.abs() < 1e-3, "la muestra lejana no se mueve");
    }

    #[test]
    fn erase_region_erodes_center_keeps_edges() {
        let mut doc = Document::new();
        let mut b = Brush::default();
        b.width = 2.0; // punta fina: solo se borra justo bajo el disco
        let mut s = Stroke::new(b);
        for i in 0..5 {
            s.push(InputSample { pos: vec2(i as f32 * 10.0, 0.0), pressure: 1.0, erosion: 0.0 });
        }
        doc.add_stroke(s);
        assert_eq!(doc.stroke_count(), 1);
        // Goma a fuerza plena en el centro (x=20): erosiona esa muestra, deja las lejanas.
        let changed = doc.erase_region(vec2(20.0, 0.0), 3.0, 1.0);
        assert!(changed);
        assert_eq!(doc.stroke_count(), 1, "la goma no fragmenta el trazo (alfa por-muestra)");
        let s = &doc.strokes()[0];
        assert!(s.samples[2].erosion > 0.9, "la muestra bajo la goma se borra");
        assert!(s.samples[0].erosion < 0.1 && s.samples[4].erosion < 0.1, "las lejanas quedan intactas");
        // Tocar lejos de cualquier trazo no cambia nada.
        let again = doc.erase_region(vec2(500.0, 500.0), 3.0, 1.0);
        assert!(!again);
    }

    #[test]
    fn erase_region_soft_sets_erosion_without_split() {
        let mut doc = Document::new();
        let mut b = Brush::default();
        b.width = 2.0;
        let mut s = Stroke::new(b);
        for i in 0..5 {
            s.push(InputSample { pos: vec2(i as f32 * 10.0, 0.0), pressure: 1.0, erosion: 0.0 });
        }
        doc.add_stroke(s);
        // Goma SUAVE (40%) en el centro: NO fragmenta; sube el erosion de la zona tocada.
        let changed = doc.erase_region(vec2(20.0, 0.0), 3.0, 0.4);
        assert!(changed);
        assert_eq!(doc.stroke_count(), 1, "la goma suave no parte el trazo");
        let eroded = doc.strokes()[0].samples.iter().any(|sm| sm.erosion > 0.0);
        assert!(eroded, "la goma suave marca erosion en la zona tocada");
    }

    #[test]
    fn add_stroke_incremental_matches_full_tessellation() {
        // El committed construido por anexado incremental debe ser identico a teselar
        // todos los trazos en orden (la malla no se corrompe al optimizar add_stroke).
        let mut doc = Document::new();
        let mut expected: Vec<Vertex> = Vec::new();
        for i in 0..6 {
            let mut s = Stroke::new(Brush::default());
            s.push(InputSample { pos: vec2(i as f32 * 5.0, 0.0), pressure: 1.0, erosion: 0.0 });
            s.push(InputSample { pos: vec2(i as f32 * 5.0 + 4.0, 3.0), pressure: 0.7, erosion: 0.0 });
            doc.add_stroke(s.clone());
            assert!(doc.last_add_was_incremental(), "una sola capa visible -> incremental");
            s.tessellate(&mut expected);
        }
        let got = doc.committed_vertices();
        assert_eq!(got.len(), expected.len(), "mismo numero de vertices");
        for (a, b) in got.iter().zip(expected.iter()) {
            assert!((a.pos[0] - b.pos[0]).abs() < 1e-4 && (a.pos[1] - b.pos[1]).abs() < 1e-4);
        }
    }
}
