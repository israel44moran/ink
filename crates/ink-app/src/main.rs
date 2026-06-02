//! ink-app — cascaron de escritorio (Windows) del motor de tinta.
//!
//! Responsabilidades del cascaron (lo unico que NO es portable):
//!   1. Abrir una ventana nativa y capturar la entrada (lapiz/raton/tactil).
//!   2. Presentar en pantalla via wgpu con baja latencia.
//!   3. Dibujar la UI (egui) encima del lienzo en el mismo frame.
//! Toda la logica de tinta vive en `ink-core`, reusable en Android e iPad.
//!
//! Controles:
//!   - Boton izquierdo: dibujar.
//!   - Boton central o Espacio + arrastrar: mover el lienzo (pan).
//!   - Rueda del raton: zoom hacia el cursor.
//!   - 1..8: color | [ ]: grosor | Z: deshacer | Y: rehacer | C: limpiar | V: present mode | Esc: salir.
//!   - El panel de la derecha se puede ocultar/mostrar con su boton.

mod copic;
mod renderer;
mod settings;
mod ui;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use ink_core::{
    push_stamp_quad, stamp_path, vec2, Brush, BrushSettings, Camera, Document, InputSample,
    OneEuroFilter, StampVertex, Stroke, TextItem, TipKind, Tool, Vec2, Vertex,
};
use renderer::{present_mode_name, GpuState};
use settings::Settings;
use ui::{Stats, UiActions, UiState};

/// Gesto de herramienta en curso (coordenadas de mundo).
enum Gesture {
    /// Seleccion por rectangulo (marquee).
    Marquee { start: Vec2, end: Vec2 },
    /// Moviendo los trazos seleccionados.
    Move { last: Vec2 },
    /// Lazo a mano alzada (Sector).
    Lasso { pts: Vec<Vec2> },
    /// Borrado duro a lo largo del arrastre.
    EraseHard { last: Vec2 },
    /// Borrado suave (atenuar) a lo largo del arrastre.
    EraseSoft { last: Vec2 },
    /// Empujar/smudge a lo largo del arrastre.
    Smudge { last: Vec2 },
}
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Force, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

struct App {
    gpu: Option<GpuState>,
    window: Option<Arc<Window>>,

    doc: Document,
    camera: Camera,
    brush: Brush,

    // Estado de entrada
    cursor: Vec2,
    last_cursor: Vec2,
    drawing: bool,
    panning: bool,
    space_down: bool,
    last_move_time: Instant,
    /// Instante del ultimo evento tactil/lapiz. Windows genera ademas eventos de
    /// raton SINTETICOS a partir del tacto; si llegan justo despues de un Touch los
    /// ignoramos para no dibujar el trazo DOS veces (los "garabatos extra").
    last_touch: Option<Instant>,

    // Trazo activo
    filter: OneEuroFilter,
    active: Option<Stroke>,
    active_mesh: Vec<Vertex>,
    last_sample_pos: Vec2,
    last_sample_time: Instant,

    // FPS
    last_frame: Instant,
    fps_timer: f32,
    fps_frames: u32,
    last_fps: f32,

    // Herramientas (seleccion / empujar / mascaras / texto)
    gesture: Option<Gesture>,
    selected: Vec<usize>,
    texts: Vec<TextItem>,
    active_text: Option<usize>,

    // --- Pinceles texturizados estilo Photoshop (estampados) ---
    /// Catalogo de puntas cargadas de los .abr (mascara alfa de cada una).
    ps_brushes: Vec<ink_brush::SampledBrush>,
    /// Pincel PS activo (None = usar el pincel procedural normal de la rueda).
    ps_settings: Option<BrushSettings>,
    /// Trazo PS en curso.
    ps_drawing: bool,
    ps_samples: Vec<InputSample>,
    ps_index: u32,
    ps_residual: f32,
    ps_active_verts: Vec<StampVertex>,
    /// Estampados confirmados por punta (id -> geometria).
    ps_committed: HashMap<u32, Vec<StampVertex>>,
    /// Puntas ya subidas a la GPU.
    ps_uploaded: HashSet<u32>,
    /// Miniaturas (imagen RGBA) de cada punta, para el selector (estilo PS).
    ps_thumb_imgs: Vec<egui::ColorImage>,
    /// Texturas egui de las miniaturas (carga diferida en el primer frame).
    ps_thumbs: Vec<Option<egui::TextureHandle>>,
    /// Packs .abr disponibles en el disco: (nombre, ruta, cargado?).
    ps_packs: Vec<(String, String, bool)>,

    // Ajustes (area de trabajo + interaccion) y geometria de rejilla.
    settings: Settings,
    grid_mesh: Vec<Vertex>,

    // Onda de confirmacion del cuentagotas: (posicion en pantalla, color, instante).
    eyedropper_ripple: Option<(Vec2, [f32; 4], Instant)>,

    // UI (egui)
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    ui: UiState,
}

impl App {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            gpu: None,
            window: None,
            doc: Document::new(),
            camera: Camera::new(vec2(1280.0, 800.0)),
            brush: Brush { color: ui::PALETTE[0], width: 4.0, ..Brush::default() },
            cursor: Vec2::ZERO,
            last_cursor: Vec2::ZERO,
            drawing: false,
            panning: false,
            space_down: false,
            last_move_time: now,
            last_touch: None,
            filter: OneEuroFilter::default(),
            active: None,
            active_mesh: Vec::new(),
            last_sample_pos: Vec2::ZERO,
            last_sample_time: now,
            last_frame: now,
            fps_timer: 0.0,
            fps_frames: 0,
            last_fps: 0.0,
            gesture: None,
            selected: Vec::new(),
            texts: Vec::new(),
            active_text: None,
            ps_brushes: Vec::new(),
            ps_settings: None,
            ps_drawing: false,
            ps_samples: Vec::new(),
            ps_index: 0,
            ps_residual: 0.0,
            ps_active_verts: Vec::new(),
            ps_committed: HashMap::new(),
            ps_uploaded: HashSet::new(),
            ps_thumb_imgs: Vec::new(),
            ps_thumbs: Vec::new(),
            ps_packs: Vec::new(),
            settings: Settings::default(),
            grid_mesh: Vec::new(),
            eyedropper_ripple: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            ui: UiState::default(),
        }
    }

    /// ¿Hubo un evento tactil/lapiz hace muy poco? Los eventos de raton que llegan en
    /// esa ventana son SINTETICOS de Windows (eco del tacto) y deben ignorarse.
    fn touch_recent(&self) -> bool {
        self.last_touch.map_or(false, |t| t.elapsed().as_millis() < 300)
    }

    fn start_stroke(&mut self, initial_pressure: f32) {
        self.commit_text();
        // Pincel de Photoshop activo: dibujar con estampados texturizados.
        if self.ps_settings.is_some() {
            self.start_stroke_ps(initial_pressure);
            return;
        }
        // Las herramientas (seleccion, etc.) y los slots vacios no dibujan tinta.
        if !self.ui.drawing_enabled() {
            return;
        }
        // Suavidad -> frecuencia de corte del filtro One-Euro (mas suave = menor corte).
        let min_cutoff = 3.0 - 2.6 * self.brush.smoothing.clamp(0.0, 1.0);
        self.filter = OneEuroFilter::new(min_cutoff, 0.015, 1.0);
        self.last_sample_time = Instant::now();
        let world = self.camera.screen_to_world(self.cursor);
        let filtered = self.filter.filter(world, 1.0 / 120.0);
        // Opacidad -> alfa del color del trazo.
        let mut b = self.brush;
        b.color[3] = self.brush.opacity.clamp(0.0, 1.0);
        let mut s = Stroke::new(b);
        s.push(InputSample {
            pos: filtered,
            pressure: initial_pressure.clamp(0.05, 1.0),
        });
        self.last_sample_pos = filtered;
        self.active = Some(s);
        self.active_mesh.clear();
        if let Some(st) = &self.active {
            st.tessellate(&mut self.active_mesh);
        }
        if let Some(g) = self.gpu.as_mut() {
            g.set_active(&self.active_mesh);
        }
        self.drawing = true;
    }

    /// Si el cuentagotas esta activo, toma el color del trazo bajo el cursor y se
    /// desactiva. Devuelve `true` si consumio el clic (no debe dibujarse nada).
    fn try_eyedropper(&mut self) -> bool {
        if !self.ui.eyedropper {
            return false;
        }
        let world = self.camera.screen_to_world(self.cursor);
        if let Some(c) = self.doc.color_at(world) {
            self.brush.color = [c[0], c[1], c[2], self.brush.opacity.clamp(0.0, 1.0)];
            // Onda de confirmacion en el punto del clic, con el color tomado.
            self.eyedropper_ripple = Some((self.cursor, c, Instant::now()));
        }
        self.ui.eyedropper = false;
        true
    }

    fn add_point(&mut self, raw_world: Vec2, pressure: f32) {
        if self.ps_drawing {
            self.add_point_ps(raw_world, pressure);
            return;
        }
        // Guarda ANTI-SALTO: si el punto nuevo salta una distancia descomunal en
        // PANTALLA (el cursor "reaparecio" lejos: lapiz que se levanta y baja en otro
        // sitio, evento perdido, re-entrada a la ventana), no trazamos la recta que
        // cruzaria el lienzo. Cerramos el trazo actual; el siguiente movimiento o toque
        // empezara uno nuevo. El umbral es alto para no cortar trazos rapidos legitimos.
        if self.active.as_ref().map_or(false, |s| !s.samples.is_empty()) {
            let jump_px = (raw_world - self.last_sample_pos).length() * self.camera.zoom;
            let limit = self.camera.viewport.min_element() * 0.5;
            if jump_px > limit {
                self.finish_stroke();
                return;
            }
        }

        let now = Instant::now();
        let dt = (now - self.last_sample_time).as_secs_f32().max(1e-4);
        let filtered = self.filter.filter(raw_world, dt);

        let mut changed = false;
        if let Some(stroke) = self.active.as_mut() {
            // Espaciado minimo ~1.2 px en pantalla, convertido a unidades de mundo.
            let min_d = (1.2 / self.camera.zoom).max(1e-4);
            if stroke.samples.is_empty() || (filtered - self.last_sample_pos).length() >= min_d {
                stroke.push(InputSample { pos: filtered, pressure });
                self.last_sample_pos = filtered;
                self.last_sample_time = now;
                changed = true;
            }
        }

        if changed {
            self.upload_active_incremental();
        }
    }

    /// Sube la geometria del trazo en vivo a la GPU de forma INCREMENTAL: tesela solo
    /// la ultima muestra y la anexa (O(1) por punto). Si el pincel tiene estado entre
    /// segmentos, o el buffer tuvo que crecer, re-tesela/re-sube el trazo completo.
    fn upload_active_incremental(&mut self) {
        let Some(st) = self.active.as_ref() else { return };
        let prev = self.active_mesh.len();
        let incremental = ink_core::tessellate_incremental(&st.samples, &st.brush, &mut self.active_mesh);
        if let Some(g) = self.gpu.as_mut() {
            if incremental {
                // Anexar solo lo nuevo; si el buffer crecio, re-subir todo.
                if !g.append_active(&self.active_mesh[prev..]) {
                    g.set_active(&self.active_mesh);
                }
            } else {
                self.active_mesh.clear();
                st.tessellate(&mut self.active_mesh);
                g.set_active(&self.active_mesh);
            }
        }
    }

    fn finish_stroke(&mut self) {
        if self.ps_drawing {
            self.finish_stroke_ps();
            return;
        }
        self.drawing = false;
        if let Some(stroke) = self.active.take() {
            if !stroke.samples.is_empty() {
                self.doc.add_stroke(stroke);
                if let Some(g) = self.gpu.as_mut() {
                    g.set_committed(self.doc.committed_vertices());
                    g.set_active(&[]);
                }
            }
        }
        self.active_mesh.clear();
    }

    fn sync_committed(&mut self) {
        if let Some(g) = self.gpu.as_mut() {
            g.set_committed(self.doc.committed_vertices());
        }
    }

    // =====================================================================
    // Pinceles texturizados estilo Photoshop (estampados)
    // =====================================================================

    /// Carga un pack .abr al catalogo (parsea las puntas; reduce el alfa para controlar
    /// la RAM y genera las miniaturas; las texturas se suben luego bajo demanda).
    fn load_ps_pack(&mut self, path: &str) -> usize {
        // Evitar recargar un pack ya cargado.
        if self.ps_packs.iter().any(|(_, pp, loaded)| pp == path && *loaded) {
            return 0;
        }
        match std::fs::read(path) {
            Ok(bytes) => match ink_brush::parse_abr(&bytes) {
                Ok(brushes) => {
                    let n = brushes.len();
                    for mut b in brushes {
                        // Reducir a un maximo de 512px: ahorra mucha RAM con cientos de
                        // puntas grandes y a tamanos de pincel normales no se nota.
                        let (w, h, data) = downscale_alpha(b.width, b.height, &b.alpha, 512);
                        b.width = w;
                        b.height = h;
                        b.alpha = data;
                        self.ps_thumb_imgs.push(make_thumb(&b, 46));
                        self.ps_thumbs.push(None);
                        self.ps_brushes.push(b);
                    }
                    if let Some(p) = self.ps_packs.iter_mut().find(|(_, pp, _)| pp == path) {
                        p.2 = true;
                    }
                    log::info!("Pincel PS: cargadas {n} puntas de {path}");
                    n
                }
                Err(e) => {
                    log::warn!("No se pudo parsear {path}: {e}");
                    0
                }
            },
            Err(e) => {
                log::warn!("No se pudo leer {path}: {e}");
                0
            }
        }
    }

    /// Crea un pincel REDONDO procedural (punta generada por dureza) y lo activa.
    /// Es el caso mas simple de "crear pincel nuevo".
    fn create_round_brush(&mut self, hardness: f32) {
        let size = 128usize;
        let r = size as f32 / 2.0;
        let mut alpha = vec![0u8; size * size];
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 - r + 0.5;
                let dy = y as f32 - r + 0.5;
                let d = (dx * dx + dy * dy).sqrt() / r;
                // Falloff: opaco hasta `hardness`, decae a 0 en el borde.
                let a = if d >= 1.0 {
                    0.0
                } else if d <= hardness {
                    1.0
                } else {
                    1.0 - (d - hardness) / (1.0 - hardness).max(1e-3)
                };
                alpha[y * size + x] = (a.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
        let idx = self.ps_brushes.len();
        let b = ink_brush::SampledBrush {
            id: format!("round-{idx}"),
            name: Some(if hardness > 0.5 { "Redondo duro".into() } else { "Redondo suave".into() }),
            width: size as u32,
            height: size as u32,
            alpha,
        };
        self.ps_thumb_imgs.push(make_thumb(&b, 46));
        self.ps_thumbs.push(None);
        self.ps_brushes.push(b);
        self.select_ps_brush(idx as u32);
    }

    /// Sube la punta `i` del catalogo a la GPU si aun no esta (con reduccion de tamano).
    fn ensure_tip(&mut self, i: u32) -> bool {
        if self.ps_uploaded.contains(&i) {
            return true;
        }
        let Some(b) = self.ps_brushes.get(i as usize) else { return false };
        let (w, h, data) = downscale_alpha(b.width, b.height, &b.alpha, 1024);
        if let Some(g) = self.gpu.as_mut() {
            g.upload_tip(i, w, h, &data);
            self.ps_uploaded.insert(i);
            true
        } else {
            false
        }
    }

    /// Activa un pincel PS del catalogo (lo deja listo para dibujar con stamps).
    fn select_ps_brush(&mut self, i: u32) {
        if !self.ensure_tip(i) {
            return;
        }
        let mut s = BrushSettings::default();
        s.tip = TipKind::Sampled(i);
        s.name = self.ps_brushes[i as usize].name.clone().unwrap_or_else(|| format!("Pincel {}", i + 1));
        s.size = 40.0;
        s.spacing = 0.10;
        // Dinamicas por defecto tipo PS: presion -> tamano y flujo.
        s.shape_dyn = true;
        s.size_control = ink_core::DynControl::PenPressure;
        s.min_diameter = 0.0;
        s.transfer_on = true;
        s.flow_control = ink_core::DynControl::PenPressure;
        self.ps_settings = Some(s);
    }

    fn start_stroke_ps(&mut self, pressure: f32) {
        self.ps_drawing = true;
        self.ps_samples.clear();
        self.ps_index = 0;
        self.ps_residual = 0.0;
        self.ps_active_verts.clear();
        let world = self.camera.screen_to_world(self.cursor);
        let min_cutoff = 3.0 - 2.6 * 0.4;
        self.filter = OneEuroFilter::new(min_cutoff, 0.015, 1.0);
        self.last_sample_time = Instant::now();
        let f = self.filter.filter(world, 1.0 / 120.0);
        self.last_sample_pos = f;
        self.ps_samples.push(InputSample { pos: f, pressure: pressure.clamp(0.05, 1.0) });
        if let Some(g) = self.gpu.as_mut() {
            g.clear_active_stamps();
        }
    }

    fn add_point_ps(&mut self, raw_world: Vec2, pressure: f32) {
        // Guarda anti-salto (igual que el motor procedural).
        if !self.ps_samples.is_empty() {
            let jump = (raw_world - self.last_sample_pos).length() * self.camera.zoom;
            if jump > self.camera.viewport.min_element() * 0.5 {
                self.finish_stroke_ps();
                return;
            }
        }
        let now = Instant::now();
        let dt = (now - self.last_sample_time).as_secs_f32().max(1e-4);
        let f = self.filter.filter(raw_world, dt);
        let min_d = (1.0 / self.camera.zoom).max(1e-4);
        if self.ps_samples.len() > 1 && (f - self.last_sample_pos).length() < min_d {
            return;
        }
        self.last_sample_pos = f;
        self.last_sample_time = now;
        self.ps_samples.push(InputSample { pos: f, pressure });

        let Some(s) = self.ps_settings.clone() else { return };
        let n = self.ps_samples.len();
        let out = stamp_path(&self.ps_samples[n - 2..n], &s, self.ps_index, self.ps_residual);
        self.ps_index = out.next_index;
        self.ps_residual = out.residual;
        if out.stamps.is_empty() {
            return;
        }
        let tip = match s.tip {
            TipKind::Sampled(id) => id,
            _ => 0,
        };
        let aspect = self.gpu.as_ref().map_or(1.0, |g| g.tip_aspect(tip));
        let rgb = [self.brush.color[0], self.brush.color[1], self.brush.color[2]];
        let prev_len = self.ps_active_verts.len();
        for st in &out.stamps {
            push_stamp_quad(&mut self.ps_active_verts, st, aspect, rgb);
        }
        if let Some(g) = self.gpu.as_mut() {
            if prev_len == 0 {
                g.set_active_stamps(tip, &self.ps_active_verts);
            } else if !g.append_active_stamps(&self.ps_active_verts[prev_len..]) {
                g.set_active_stamps(tip, &self.ps_active_verts);
            }
        }
    }

    fn finish_stroke_ps(&mut self) {
        self.ps_drawing = false;
        let s = match self.ps_settings.clone() {
            Some(s) => s,
            None => return,
        };
        let tip = match s.tip {
            TipKind::Sampled(id) => id,
            _ => 0,
        };
        // Un solo toque sin movimiento: estampar un punto.
        if self.ps_active_verts.is_empty() && self.ps_samples.len() == 1 {
            let out = stamp_path(&self.ps_samples, &s, 0, 0.0);
            let aspect = self.gpu.as_ref().map_or(1.0, |g| g.tip_aspect(tip));
            let rgb = [self.brush.color[0], self.brush.color[1], self.brush.color[2]];
            for st in &out.stamps {
                push_stamp_quad(&mut self.ps_active_verts, st, aspect, rgb);
            }
        }
        if !self.ps_active_verts.is_empty() {
            let v = self.ps_committed.entry(tip).or_default();
            v.extend_from_slice(&self.ps_active_verts);
            let verts = v.clone();
            if let Some(g) = self.gpu.as_mut() {
                g.set_committed_stamps(tip, &verts);
            }
        }
        self.ps_active_verts.clear();
        self.ps_samples.clear();
        if let Some(g) = self.gpu.as_mut() {
            g.clear_active_stamps();
        }
    }


    // =====================================================================
    // Motores de las herramientas (seleccion, empujar, sector, mascaras, texto)
    // =====================================================================

    /// Radio en MUNDO para que el gesto tenga ~`px` pixeles fisicos en pantalla.
    fn world_radius(&self, px: f32) -> f32 {
        (px / self.camera.zoom).max(1e-3)
    }

    /// Cierra la edicion de texto activa; descarta el texto si quedo vacio.
    fn commit_text(&mut self) {
        if let Some(ti) = self.active_text.take() {
            if self.texts.get(ti).map_or(false, |t| t.content.is_empty()) {
                self.texts.remove(ti);
            }
        }
    }

    /// Indice del texto bajo el punto de mundo `w` (el de encima primero).
    fn text_at(&self, w: Vec2) -> Option<usize> {
        for (i, t) in self.texts.iter().enumerate().rev() {
            let h = t.size.max(1.0);
            let wd = (t.content.chars().count().max(1) as f32) * t.size * 0.55;
            if w.x >= t.pos.x && w.x <= t.pos.x + wd && w.y >= t.pos.y && w.y <= t.pos.y + h {
                return Some(i);
            }
        }
        None
    }

    /// Comienza un gesto de herramienta en el cursor actual.
    fn tool_press(&mut self, tool: Tool) {
        let w = self.camera.screen_to_world(self.cursor);
        if tool != Tool::Text {
            self.commit_text();
        }
        match tool {
            Tool::Select | Tool::Sector => {
                // Si presiono dentro de la seleccion existente, la muevo.
                if !self.selected.is_empty() {
                    if let Some(bb) = self.doc.bounds_of(&self.selected) {
                        if bb.contains(w) {
                            self.gesture = Some(Gesture::Move { last: w });
                            return;
                        }
                    }
                }
                self.selected.clear();
                self.gesture = Some(if tool == Tool::Select {
                    Gesture::Marquee { start: w, end: w }
                } else {
                    Gesture::Lasso { pts: vec![w] }
                });
            }
            Tool::Push => {
                self.gesture = Some(Gesture::Smudge { last: w });
            }
            Tool::MaskHard => {
                let r = self.world_radius(16.0);
                if self.doc.erase_hard(w, w, r) > 0 {
                    self.sync_committed();
                }
                self.gesture = Some(Gesture::EraseHard { last: w });
            }
            Tool::MaskSoft => {
                let r = self.world_radius(18.0);
                if self.doc.erase_soft(w, w, r, 0.18) {
                    self.sync_committed();
                }
                self.gesture = Some(Gesture::EraseSoft { last: w });
            }
            Tool::Text => {
                if let Some(i) = self.text_at(w) {
                    self.commit_text();
                    self.active_text = Some(i);
                } else {
                    self.commit_text();
                    let size = self.world_radius(24.0);
                    let col = [self.brush.color[0], self.brush.color[1], self.brush.color[2], 1.0];
                    self.texts.push(TextItem::new(w, size, col));
                    self.active_text = Some(self.texts.len() - 1);
                }
                self.gesture = None;
            }
        }
    }

    /// Continua el gesto de herramienta (llamado al mover con el boton abajo).
    fn tool_drag(&mut self) {
        let w = self.camera.screen_to_world(self.cursor);
        let zoom = self.camera.zoom;
        // Extraemos la accion sin retener el prestamo de `self.gesture`.
        enum Act {
            None,
            Move(Vec2),
            Smudge(Vec2, Vec2),
            Erase(Vec2, Vec2, bool),
        }
        let act = match self.gesture.as_mut() {
            Some(Gesture::Marquee { end, .. }) => {
                *end = w;
                Act::None
            }
            Some(Gesture::Lasso { pts }) => {
                let far = pts.last().map_or(true, |l| (w - *l).length() > 3.0 / zoom);
                if far {
                    pts.push(w);
                }
                Act::None
            }
            Some(Gesture::Move { last }) => {
                let d = w - *last;
                *last = w;
                Act::Move(d)
            }
            Some(Gesture::Smudge { last }) => {
                let p = *last;
                *last = w;
                Act::Smudge(p, w - p)
            }
            Some(Gesture::EraseHard { last }) => {
                let a = *last;
                *last = w;
                Act::Erase(a, w, false)
            }
            Some(Gesture::EraseSoft { last }) => {
                let a = *last;
                *last = w;
                Act::Erase(a, w, true)
            }
            None => Act::None,
        };
        match act {
            Act::None => {}
            Act::Move(d) => {
                let sel = self.selected.clone();
                self.doc.translate_strokes(&sel, d);
                self.sync_committed();
            }
            Act::Smudge(p, d) => {
                let r = self.world_radius(22.0);
                if self.doc.smudge(p, d, r) {
                    self.sync_committed();
                }
            }
            Act::Erase(a, b, soft) => {
                let changed = if soft {
                    self.doc.erase_soft(a, b, self.world_radius(18.0), 0.18)
                } else {
                    self.doc.erase_hard(a, b, self.world_radius(16.0)) > 0
                };
                if changed {
                    self.sync_committed();
                }
            }
        }
    }

    /// Finaliza el gesto de herramienta (al soltar el boton).
    fn tool_release(&mut self) {
        match self.gesture.take() {
            Some(Gesture::Marquee { start, end }) => {
                self.selected = self.doc.strokes_in_rect(start, end);
            }
            Some(Gesture::Lasso { pts }) => {
                self.selected = self.doc.strokes_in_polygon(&pts);
            }
            _ => {}
        }
    }

    /// Dibuja, encima del lienzo, lo propio de las herramientas: texto colocado,
    /// caja de seleccion, marquee/lazo en curso y el anillo del cursor de borrado.
    /// Todo en *puntos* de egui (= pixeles fisicos / ppp).
    fn draw_overlays(&self, ctx: &egui::Context) {
        use egui::{Align2, Color32, FontId, LayerId, Order, Pos2, Rect, Shape, Stroke, StrokeKind};
        let ppp = ctx.pixels_per_point().max(0.01);
        let cam = &self.camera;
        let to_pt = |w: Vec2| {
            let s = cam.world_to_screen(w) / ppp;
            Pos2::new(s.x, s.y)
        };
        let accent = Color32::from_rgb(70, 140, 230);
        // Painter de capa: cubre toda la pantalla (sin recorte de Area), debajo del panel.
        let p = ctx.layer_painter(LayerId::new(Order::Middle, egui::Id::new("tool_overlay")));

        // Marco de la mesa de trabajo (si su tamano no es infinito), centrado en el origen.
        if let Some((aw, ah)) = self.settings.artboard_size() {
            let r = Rect::from_two_pos(to_pt(Vec2::new(-aw * 0.5, -ah * 0.5)), to_pt(Vec2::new(aw * 0.5, ah * 0.5)));
            p.rect_stroke(r, egui::CornerRadius::ZERO, Stroke::new(1.5, Color32::from_gray(160)), StrokeKind::Outside);
        }

        // Longitud del trazo en curso (si "Mostrar la longitud del trazo" esta activo).
        if self.settings.show_stroke_length {
            if let Some(st) = &self.active {
                let len: f32 = st.samples.windows(2).map(|w| (w[1].pos - w[0].pos).length()).sum();
                if len > 0.5 {
                    let sr = ctx.content_rect();
                    p.text(
                        egui::pos2(sr.right() - 16.0, sr.top() + 16.0),
                        Align2::RIGHT_TOP,
                        self.settings.format_measure(len),
                        FontId::proportional(15.0),
                        Color32::from_gray(60),
                    );
                }
            }
        }

        // Texto del lienzo (con cursor "|" en el que se edita).
        for (i, t) in self.texts.iter().enumerate() {
            if t.content.is_empty() && Some(i) != self.active_text {
                continue;
            }
            let px = (t.size * cam.zoom / ppp).clamp(6.0, 600.0);
            let col = Color32::from_rgb(
                (t.color[0] * 255.0) as u8,
                (t.color[1] * 255.0) as u8,
                (t.color[2] * 255.0) as u8,
            );
            let shown = if Some(i) == self.active_text {
                format!("{}|", t.content)
            } else {
                t.content.clone()
            };
            p.text(to_pt(t.pos), Align2::LEFT_TOP, shown, FontId::proportional(px), col);
        }

        // Caja de la seleccion actual (si "Resaltar seleccion" esta activo).
        if self.settings.highlight_selection && !self.selected.is_empty() {
            if let Some(bb) = self.doc.bounds_of(&self.selected) {
                let r = Rect::from_two_pos(to_pt(bb.min), to_pt(bb.max));
                p.rect_filled(r, egui::CornerRadius::same(2), Color32::from_rgba_unmultiplied(70, 140, 230, 20));
                p.rect_stroke(r, egui::CornerRadius::same(2), Stroke::new(1.5, accent), StrokeKind::Outside);
            }
        }

        // Gesto en curso (marquee o lazo).
        match &self.gesture {
            Some(Gesture::Marquee { start, end }) => {
                let r = Rect::from_two_pos(to_pt(*start), to_pt(*end));
                p.rect_filled(r, egui::CornerRadius::same(2), Color32::from_rgba_unmultiplied(70, 140, 230, 24));
                p.rect_stroke(r, egui::CornerRadius::same(2), Stroke::new(1.0, accent), StrokeKind::Outside);
            }
            Some(Gesture::Lasso { pts }) => {
                if pts.len() >= 2 {
                    let sp: Vec<Pos2> = pts.iter().map(|w| to_pt(*w)).collect();
                    p.add(Shape::line(sp, Stroke::new(1.5, accent)));
                }
            }
            _ => {}
        }

        // Anillo del cursor para borrar/empujar (muestra el radio real).
        if let Some(rpx) = match self.ui.active_tool() {
            Some(Tool::MaskHard) => Some(16.0),
            Some(Tool::MaskSoft) => Some(18.0),
            Some(Tool::Push) => Some(22.0),
            _ => None,
        } {
            let c = Pos2::new(self.cursor.x / ppp, self.cursor.y / ppp);
            p.circle_stroke(c, rpx / ppp, Stroke::new(1.0, Color32::from_gray(140)));
        }

        // Cursor de cuentagotas: el icono del gotero sigue al raton (su punta en el cursor).
        if self.ui.eyedropper {
            let cur = Pos2::new(self.cursor.x / ppp, self.cursor.y / ppp);
            let s = 13.0;
            let c_icon = cur + egui::vec2(0.7071, -0.7071) * s;
            ui::icon_dropper(&p, c_icon, s, Color32::from_rgb(40, 120, 220));
        }

        // Onda tipo "agua" al copiar un color con el cuentagotas (del color tomado).
        if let Some((pos, col, t0)) = self.eyedropper_ripple {
            let age = t0.elapsed().as_secs_f32();
            if age < 0.7 {
                let center = Pos2::new(pos.x / ppp, pos.y / ppp);
                let base = Color32::from_rgb((col[0] * 255.0) as u8, (col[1] * 255.0) as u8, (col[2] * 255.0) as u8);
                for k in 0..3 {
                    let ph = ((age / 0.7) + k as f32 * 0.16).min(1.0);
                    let radius = (14.0 + ph * 92.0) / ppp;
                    let a = ((1.0 - ph).powf(1.2).clamp(0.0, 1.0) * 200.0) as u8;
                    p.circle_stroke(
                        center,
                        radius,
                        Stroke::new(5.0 / ppp, Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)),
                    );
                }
            }
        }
    }

    fn set_color(&mut self, idx: usize) {
        if let Some(c) = ui::PALETTE.get(idx) {
            self.brush.color = *c;
        }
    }

    fn cycle_present_mode(&mut self) {
        if let Some(g) = self.gpu.as_mut() {
            let order = [
                wgpu::PresentMode::Mailbox,
                wgpu::PresentMode::Immediate,
                wgpu::PresentMode::Fifo,
            ];
            let avail: Vec<_> = order
                .iter()
                .copied()
                .filter(|m| g.supported_present_modes.contains(m))
                .collect();
            if avail.is_empty() {
                return;
            }
            let cur = g.present_mode();
            let idx = avail.iter().position(|m| *m == cur).unwrap_or(0);
            let next = avail[(idx + 1) % avail.len()];
            g.set_present_mode(next);
            log::info!("present mode -> {}", present_mode_name(next));
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Ink v0.2 — motor de tinta + panel")
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_maximized(true);
        let window = Arc::new(event_loop.create_window(attrs).expect("crear ventana"));
        let size = window.inner_size();
        self.camera = Camera::new(vec2(size.width.max(1) as f32, size.height.max(1) as f32));
        let gpu = pollster::block_on(GpuState::new(window.clone()));

        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.egui_state = Some(egui_state);

        // Descubrir todos los packs .abr del usuario y cargar uno por defecto.
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let base = format!(r"{home}\Downloads\Photoshop Brushes");
        self.ps_packs = scan_packs(&base);
        let default_path = self
            .ps_packs
            .iter()
            .find(|(n, _, _)| n.contains("Size Flow"))
            .or_else(|| self.ps_packs.first())
            .map(|(_, p, _)| p.clone());
        if let Some(p) = default_path {
            self.load_ps_pack(&p);
        }

        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Pasar el evento a egui primero. Si egui lo consume (interaccion con el panel),
        // no lo usamos para dibujar/navegar el lienzo.
        let egui_consumed = if let (Some(state), Some(window)) =
            (self.egui_state.as_mut(), self.window.clone())
        {
            state.on_window_event(window.as_ref(), &event).consumed
        } else {
            false
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                self.camera.viewport = vec2(size.width as f32, size.height as f32);
                if let Some(g) = self.gpu.as_mut() {
                    g.resize(size.width, size.height);
                }
            }

            WindowEvent::CursorLeft { .. } => {
                // El cursor/lapiz salio del area: cerrar el trazo en curso para no
                // trazar una recta al re-entrar lejos (evita las "rayas que cruzan").
                if self.drawing {
                    self.finish_stroke();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // Ignorar el movimiento de raton SINTETICO que Windows genera tras un
                // toque del lapiz (el Touch ya lo maneja); evita el trazo duplicado.
                if self.touch_recent() {
                    return;
                }
                let now = Instant::now();
                let cur = vec2(position.x as f32, position.y as f32);
                let dt_move = (now - self.last_move_time).as_secs_f32().max(1e-4);
                let speed = (cur - self.last_cursor).length() / dt_move;
                self.last_move_time = now;

                if self.panning {
                    let delta = cur - self.last_cursor;
                    self.camera.pan_pixels(delta);
                } else if self.gesture.is_some() {
                    self.cursor = cur;
                    self.tool_drag();
                } else if self.drawing || self.ps_drawing {
                    let world = self.camera.screen_to_world(cur);
                    let pressure = (1.0 - (speed / 2600.0).clamp(0.0, 0.7)).clamp(0.05, 1.0);
                    self.add_point(world, pressure);
                }

                self.cursor = cur;
                self.last_cursor = cur;
            }

            WindowEvent::MouseInput { state, button, .. } if self.touch_recent() => {
                // Clic de raton SINTETICO (eco del lapiz/tacto): ignorar para no iniciar
                // un segundo trazo encima del tactil.
                let _ = (state, button);
            }

            WindowEvent::MouseInput { state, button, .. } => match button {
                MouseButton::Left => match state {
                    ElementState::Pressed => {
                        if egui_consumed {
                            // Interaccion con la UI: no dibujar. Si el cuentagotas estaba
                            // activo y se toca otra opcion, se cancela (vuelve la flecha).
                            self.ui.eyedropper = false;
                        } else if self.try_eyedropper() {
                            // Cuentagotas: tomo el color y no dibujo.
                        } else {
                            // Tocar el lienzo cierra el selector de color (con animacion).
                            self.ui.show_colors = false;
                            if self.space_down {
                                self.panning = true;
                            } else if self.ps_settings.is_some() {
                                // Pincel PS activo: tiene prioridad sobre herramientas.
                                self.start_stroke(0.5);
                            } else if let Some(tool) = self.ui.active_tool() {
                                self.tool_press(tool);
                            } else {
                                self.start_stroke(0.5);
                            }
                        }
                    }
                    ElementState::Released => {
                        // Siempre cerramos el trazo/pan/gesto para no quedar "pegados".
                        if self.panning {
                            self.panning = false;
                        } else if self.gesture.is_some() {
                            self.tool_release();
                        } else {
                            self.finish_stroke();
                        }
                    }
                },
                MouseButton::Middle => {
                    if state == ElementState::Pressed {
                        if !egui_consumed {
                            self.panning = true;
                        }
                    } else {
                        self.panning = false;
                    }
                }
                _ => {}
            },

            WindowEvent::MouseWheel { delta, .. } => {
                if !egui_consumed {
                    let amount = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 120.0,
                    };
                    if amount != 0.0 {
                        let factor = 1.12_f32.powf(amount);
                        self.camera.zoom_at(self.cursor, factor);
                    }
                }
            }

            WindowEvent::Touch(t) => {
                // Marca el instante del tacto/lapiz para suprimir el eco de raton sintetico.
                self.last_touch = Some(Instant::now());
                let loc = vec2(t.location.x as f32, t.location.y as f32);
                let pressure = match t.force {
                    Some(Force::Normalized(n)) => n as f32,
                    Some(Force::Calibrated { force, max_possible_force, .. }) => {
                        if max_possible_force > 0.0 {
                            (force / max_possible_force) as f32
                        } else {
                            0.5
                        }
                    }
                    None => 0.5,
                };
                match t.phase {
                    TouchPhase::Started => {
                        if !egui_consumed {
                            // Si quedo un trazo sin cerrar (se perdio el Ended), ciERRalo
                            // antes de empezar otro para no encadenar una recta entre ambos.
                            if self.drawing {
                                self.finish_stroke();
                            }
                            self.cursor = loc;
                            self.last_cursor = loc;
                            if self.try_eyedropper() {
                                // Cuentagotas: solo toma color.
                            } else {
                                self.ui.show_colors = false; // tocar el lienzo cierra el selector
                                if self.ps_settings.is_some() {
                                    self.start_stroke(pressure);
                                } else if let Some(tool) = self.ui.active_tool() {
                                    self.tool_press(tool);
                                } else {
                                    self.start_stroke(pressure);
                                }
                            }
                        }
                    }
                    TouchPhase::Moved => {
                        self.cursor = loc;
                        if self.gesture.is_some() {
                            self.tool_drag();
                        } else {
                            let world = self.camera.screen_to_world(loc);
                            self.add_point(world, pressure);
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        if self.gesture.is_some() {
                            self.tool_release();
                        } else {
                            self.finish_stroke();
                        }
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } if !egui_consumed => {
                let pressed = event.state == ElementState::Pressed;

                // Edicion de texto: mientras hay un texto activo, el teclado escribe en el.
                if let Some(ti) = self.active_text {
                    if pressed {
                        match event.physical_key {
                            PhysicalKey::Code(KeyCode::Escape)
                            | PhysicalKey::Code(KeyCode::Enter)
                            | PhysicalKey::Code(KeyCode::NumpadEnter) => self.commit_text(),
                            PhysicalKey::Code(KeyCode::Backspace) => {
                                if let Some(t) = self.texts.get_mut(ti) {
                                    t.content.pop();
                                }
                            }
                            _ => {
                                if let Some(txt) = &event.text {
                                    if let Some(t) = self.texts.get_mut(ti) {
                                        for ch in txt.chars() {
                                            if !ch.is_control() {
                                                t.content.push(ch);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    return;
                }

                if let PhysicalKey::Code(code) = event.physical_key {
                    match code {
                        KeyCode::Space => self.space_down = pressed,
                        _ if !pressed => {}
                        KeyCode::Escape => event_loop.exit(),
                        // "Activar los atajos del teclado": si esta desactivado, el
                        // resto de atajos (C/Z/Y/V/[ ]/digitos) no hacen nada.
                        _ if !self.settings.shortcuts_enabled => {}
                        KeyCode::KeyC => {
                            self.doc.clear();
                            self.texts.clear();
                            self.selected.clear();
                            self.active_text = None;
                            self.sync_committed();
                        }
                        KeyCode::KeyZ => {
                            self.doc.undo();
                            self.sync_committed();
                        }
                        KeyCode::KeyY => {
                            self.doc.redo();
                            self.sync_committed();
                        }
                        KeyCode::KeyV => self.cycle_present_mode(),
                        KeyCode::BracketLeft => {
                            self.brush.width = (self.brush.width * 0.8).max(0.5);
                        }
                        KeyCode::BracketRight => {
                            self.brush.width = (self.brush.width * 1.25).min(200.0);
                        }
                        KeyCode::Digit1 => self.set_color(0),
                        KeyCode::Digit2 => self.set_color(1),
                        KeyCode::Digit3 => self.set_color(2),
                        KeyCode::Digit4 => self.set_color(3),
                        KeyCode::Digit5 => self.set_color(4),
                        KeyCode::Digit6 => self.set_color(5),
                        KeyCode::Digit7 => self.set_color(6),
                        KeyCode::Digit8 => self.set_color(7),
                        _ => {}
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32();
                self.last_frame = now;
                self.fps_timer += dt;
                self.fps_frames += 1;
                if self.fps_timer >= 0.5 {
                    self.last_fps = self.fps_frames as f32 / self.fps_timer;
                    self.fps_timer = 0.0;
                    self.fps_frames = 0;
                }

                let window = match self.window.clone() {
                    Some(w) => w,
                    None => return,
                };

                // --- Frame de egui ---
                let raw_input = match self.egui_state.as_mut() {
                    Some(s) => s.take_egui_input(window.as_ref()),
                    None => return,
                };
                let stats = Stats {
                    fps: self.last_fps,
                    strokes: self.doc.stroke_count(),
                    verts: self.doc.vertex_count(),
                    zoom: self.camera.zoom,
                    can_undo: self.doc.can_undo(),
                    can_redo: self.doc.can_redo(),
                };
                // Subir a egui las miniaturas de pincel que falten (carga diferida).
                for i in 0..self.ps_thumbs.len() {
                    if self.ps_thumbs[i].is_none() {
                        let img = self.ps_thumb_imgs[i].clone();
                        let tex = self.egui_ctx.load_texture(format!("psthumb{i}"), img, egui::TextureOptions::LINEAR);
                        self.ps_thumbs[i] = Some(tex);
                    }
                }

                let mut actions = UiActions::default();
                let mut ps_select: Option<u32> = None;
                let mut ps_clear = false;
                let mut ps_load_pack: Option<String> = None;
                let mut ps_load_all = false;
                let mut ps_new_round: Option<f32> = None;
                let ctx = self.egui_ctx.clone();
                #[allow(deprecated)]
                let full_output = ctx.run(raw_input, |ctx| {
                    actions = ui::build_panel(ctx, &mut self.ui, &mut self.brush, &mut self.settings, &mut self.doc, stats);
                    self.draw_overlays(ctx);

                    // --- Panel provisional de pinceles de Photoshop (selector) ---
                    {
                        let brushes = &self.ps_brushes;
                        let thumbs = &self.ps_thumbs;
                        let packs = &self.ps_packs;
                        let active_tip = self.ps_settings.as_ref().and_then(|s| match s.tip {
                            ink_core::TipKind::Sampled(id) => Some(id),
                            _ => None,
                        });
                        egui::Area::new(egui::Id::new("ps_brushes_panel"))
                            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 56.0))
                            .show(ctx, |ui| {
                                egui::Frame::popup(ui.style()).show(ui, |ui| {
                                    ui.set_max_width(250.0);
                                    ui.label(egui::RichText::new(format!("Pinceles Photoshop ({})", brushes.len())).strong());
                                    // Gestor de packs.
                                    ui.horizontal(|ui| {
                                        egui::ComboBox::from_id_salt("pack_combo")
                                            .selected_text(format!("+ Cargar pack ({})", packs.len()))
                                            .width(160.0)
                                            .show_ui(ui, |ui| {
                                                for (name, path, loaded) in packs {
                                                    let lbl = if *loaded { format!("✓ {name}") } else { name.clone() };
                                                    if ui.selectable_label(false, lbl).clicked() {
                                                        ps_load_pack = Some(path.clone());
                                                    }
                                                }
                                            });
                                    });
                                    ui.horizontal(|ui| {
                                        if ui.button("Cargar todos").clicked() {
                                            ps_load_all = true;
                                        }
                                        if ui.button("+ Redondo").clicked() {
                                            ps_new_round = Some(1.0);
                                        }
                                        if ui.button("+ Suave").clicked() {
                                            ps_new_round = Some(0.0);
                                        }
                                    });
                                    if active_tip.is_some() {
                                        if ui.button("Volver al pincel normal").clicked() {
                                            ps_clear = true;
                                        }
                                    }
                                    ui.separator();
                                    egui::ScrollArea::vertical().max_height(470.0).auto_shrink([false, false]).show(ui, |ui| {
                                        for (i, b) in brushes.iter().enumerate() {
                                            let selected = active_tip == Some(i as u32);
                                            let clicked = ui
                                                .horizontal(|ui| {
                                                    let mut c = false;
                                                    if let Some(Some(tex)) = thumbs.get(i) {
                                                        let img = egui::Image::new(egui::load::SizedTexture::new(tex.id(), egui::vec2(44.0, 44.0)));
                                                        if ui.add(egui::ImageButton::new(img).selected(selected)).clicked() {
                                                            c = true;
                                                        }
                                                    }
                                                    let label = egui::RichText::new(format!("Pincel {}\n{}×{}", i + 1, b.width, b.height)).size(12.0);
                                                    if ui.selectable_label(selected, label).clicked() {
                                                        c = true;
                                                    }
                                                    c
                                                })
                                                .inner;
                                            if clicked {
                                                ps_select = Some(i as u32);
                                            }
                                        }
                                    });
                                });
                            });
                    }

                    // Panel "Ajustes del pincel" del pincel PS activo (edita en vivo).
                    if let Some(s) = self.ps_settings.as_mut() {
                        brush_settings_panel(ctx, s);
                    }
                });
                if let Some(s) = self.egui_state.as_mut() {
                    s.handle_platform_output(window.as_ref(), full_output.platform_output);
                }
                if let Some(p) = ps_load_pack {
                    self.load_ps_pack(&p);
                }
                if ps_load_all {
                    let paths: Vec<String> = self
                        .ps_packs
                        .iter()
                        .filter(|(_, _, loaded)| !*loaded)
                        .map(|(_, p, _)| p.clone())
                        .collect();
                    for p in paths {
                        self.load_ps_pack(&p);
                    }
                }
                if let Some(h) = ps_new_round {
                    self.create_round_brush(h);
                }
                if let Some(i) = ps_select {
                    self.select_ps_brush(i);
                }
                if ps_clear {
                    self.ps_settings = None;
                }

                // Aplicar acciones del panel.
                if actions.undo {
                    self.doc.undo();
                    self.sync_committed();
                }
                if actions.redo {
                    self.doc.redo();
                    self.sync_committed();
                }
                if actions.clear {
                    self.doc.clear();
                    self.sync_committed();
                }
                if actions.layers_dirty {
                    // El panel de capas modifico el documento: re-subir la malla.
                    self.sync_committed();
                }

                let ppp = ctx.pixels_per_point();
                let primitives = ctx.tessellate(full_output.shapes, ppp);

                // Rejilla del lienzo (geometria que se dibuja detras de la tinta).
                self.grid_mesh.clear();
                let grid_limit = if self.settings.grid_limit_artboard {
                    self.settings
                        .artboard_size()
                        .map(|(w, h)| (vec2(-w * 0.5, -h * 0.5), vec2(w * 0.5, h * 0.5)))
                } else {
                    None
                };
                ink_core::build_grid(
                    &mut self.grid_mesh,
                    self.settings.grid,
                    &self.camera,
                    self.settings.grid_size,
                    self.settings.grid_divisions,
                    self.settings.grid_line_width,
                    grid_limit,
                    self.settings.grid_color(),
                );
                let bg = self.settings.bg_color();

                // --- Render (lienzo + UI encima) ---
                if let Some(g) = self.gpu.as_mut() {
                    let screen = egui_wgpu::ScreenDescriptor {
                        size_in_pixels: [g.width(), g.height()],
                        pixels_per_point: ppp,
                    };
                    g.set_bg(bg);
                    g.set_grid(&self.grid_mesh);
                    g.update_camera(self.camera.view_proj());
                    g.render(&primitives, &full_output.textures_delta, &screen);
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

/// Escanea (recursivo) una carpeta en busca de archivos .abr; devuelve (nombre, ruta).
fn scan_packs(base: &str) -> Vec<(String, String, bool)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String, bool)>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().map_or(false, |x| x.eq_ignore_ascii_case("abr")) {
                    let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    out.push((name, p.to_string_lossy().into_owned(), false));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(std::path::Path::new(base), &mut out);
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out
}

/// Combo de "Control:" (que dinamica modula un parametro), estilo Photoshop.
fn dyn_combo(ui: &mut egui::Ui, id: &str, ctrl: &mut ink_core::DynControl) {
    use ink_core::DynControl as D;
    let label = match ctrl {
        D::Off => "Desactivado",
        D::Fade(_) => "Desvanecer",
        D::PenPressure => "Presión de la pluma",
        D::PenTilt => "Inclinación",
        D::StylusWheel => "Rueda del stylus",
        D::Direction => "Dirección",
        D::Rotation => "Rotación",
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .width(150.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(ctrl, D::Off, "Desactivado");
            ui.selectable_value(ctrl, D::PenPressure, "Presión de la pluma");
            ui.selectable_value(ctrl, D::Fade(50), "Desvanecer");
            ui.selectable_value(ctrl, D::Direction, "Dirección");
        });
}

/// Una fila "etiqueta + slider 0..100%" para un factor 0..1.
fn pct_row(ui: &mut egui::Ui, label: &str, v: &mut f32) {
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(v, 0.0..=1.0).custom_formatter(|x, _| format!("{:.0}%", x * 100.0)).custom_parser(|s| s.trim_end_matches('%').parse::<f64>().ok().map(|x| x / 100.0)));
        ui.label(label);
    });
}

/// Panel "Ajustes del pincel" estilo Photoshop: edita `s` en vivo (el motor de
/// estampado lo aplica al siguiente trazo). Devuelve nada; muta `s`.
fn brush_settings_panel(ctx: &egui::Context, s: &mut ink_core::BrushSettings) {
    use egui::{CollapsingHeader, RichText, Slider};
    egui::Window::new(RichText::new("Ajustes del pincel").strong())
        .id(egui::Id::new("ps_settings_window"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 64.0))
        .default_width(280.0)
        .resizable(false)
        .collapsible(true)
        .show(ctx, |ui| {
            ui.label(RichText::new(&s.name).italics().color(egui::Color32::from_rgb(40, 120, 220)));
            egui::ScrollArea::vertical().max_height(560.0).auto_shrink([false, false]).show(ui, |ui| {
                // ---- Forma de la punta del pincel ----
                CollapsingHeader::new("Forma de la punta").default_open(true).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(Slider::new(&mut s.size, 1.0..=400.0).suffix(" px"));
                        ui.label("Tamaño");
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut s.flip_x, "Voltear X");
                        ui.checkbox(&mut s.flip_y, "Voltear Y");
                    });
                    ui.horizontal(|ui| {
                        ui.add(Slider::new(&mut s.angle, -180.0..=180.0).suffix("°"));
                        ui.label("Ángulo");
                    });
                    pct_row(ui, "Redondez", &mut s.roundness);
                    pct_row(ui, "Dureza", &mut s.hardness);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut s.spacing_on, "");
                        ui.add(Slider::new(&mut s.spacing, 0.01..=2.0).custom_formatter(|x, _| format!("{:.0}%", x * 100.0)));
                        ui.label("Espaciado");
                    });
                });

                // ---- Dinamica de forma ----
                CollapsingHeader::new("Dinámica de forma").show(ui, |ui| {
                    ui.checkbox(&mut s.shape_dyn, "Activar");
                    ui.add_enabled_ui(s.shape_dyn, |ui| {
                        pct_row(ui, "Variación del tamaño", &mut s.size_jitter);
                        ui.horizontal(|ui| {
                            ui.label("Control:");
                            dyn_combo(ui, "size_ctrl", &mut s.size_control);
                        });
                        pct_row(ui, "Diámetro mínimo", &mut s.min_diameter);
                        pct_row(ui, "Variación del ángulo", &mut s.angle_jitter);
                        pct_row(ui, "Variación de la redondez", &mut s.roundness_jitter);
                        pct_row(ui, "Redondez mínima", &mut s.min_roundness);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut s.flip_x_jitter, "Vibración X");
                            ui.checkbox(&mut s.flip_y_jitter, "Vibración Y");
                        });
                    });
                });

                // ---- Dispersion ----
                CollapsingHeader::new("Dispersión").show(ui, |ui| {
                    ui.checkbox(&mut s.scatter_on, "Activar");
                    ui.add_enabled_ui(s.scatter_on, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(Slider::new(&mut s.scatter, 0.0..=10.0).custom_formatter(|x, _| format!("{:.0}%", x * 100.0)));
                            ui.checkbox(&mut s.scatter_both_axes, "Ambos ejes");
                        });
                        ui.horizontal(|ui| {
                            ui.add(Slider::new(&mut s.count, 1..=16));
                            ui.label("Cantidad");
                        });
                        pct_row(ui, "Variación de la cantidad", &mut s.count_jitter);
                    });
                });

                // ---- Transferencia ----
                CollapsingHeader::new("Transferencia").show(ui, |ui| {
                    ui.checkbox(&mut s.transfer_on, "Activar");
                    ui.add_enabled_ui(s.transfer_on, |ui| {
                        pct_row(ui, "Variación de opacidad", &mut s.opacity_jitter);
                        ui.horizontal(|ui| {
                            ui.label("Control:");
                            dyn_combo(ui, "op_ctrl", &mut s.opacity_control);
                        });
                        pct_row(ui, "Variación de flujo", &mut s.flow_jitter);
                        ui.horizontal(|ui| {
                            ui.label("Control:");
                            dyn_combo(ui, "flow_ctrl", &mut s.flow_control);
                        });
                    });
                });

                // ---- Dinamica de color ----
                CollapsingHeader::new("Dinámica de color").show(ui, |ui| {
                    ui.checkbox(&mut s.color_dyn, "Activar");
                    ui.add_enabled_ui(s.color_dyn, |ui| {
                        pct_row(ui, "Variación de tono", &mut s.hue_jitter);
                        pct_row(ui, "Variación de saturación", &mut s.sat_jitter);
                        pct_row(ui, "Variación de brillo", &mut s.bright_jitter);
                    });
                });

                // ---- Casillas simples (estado real; algunas se conectan despues) ----
                CollapsingHeader::new("Más opciones").show(ui, |ui| {
                    ui.checkbox(&mut s.noise, "Ruido");
                    ui.checkbox(&mut s.wet_edges, "Bordes húmedos");
                    ui.checkbox(&mut s.buildup, "Concentración");
                    ui.checkbox(&mut s.smoothing, "Suavizar");
                    ui.checkbox(&mut s.protect_texture, "Proteger textura");
                });

                ui.separator();
                pct_row(ui, "Opacidad", &mut s.opacity);
                pct_row(ui, "Flujo", &mut s.flow);
            });
        });
}

/// Crea una miniatura cuadrada (estilo Photoshop) de una punta: la forma del pincel en
/// oscuro sobre fondo transparente, centrada, de `size`x`size` px.
fn make_thumb(b: &ink_brush::SampledBrush, size: usize) -> egui::ColorImage {
    let (tw, th, data) = downscale_alpha(b.width, b.height, &b.alpha, size as u32);
    let (tw, th) = (tw as usize, th as usize);
    let mut rgba = vec![0u8; size * size * 4];
    let ox = size.saturating_sub(tw) / 2;
    let oy = size.saturating_sub(th) / 2;
    for y in 0..th.min(size.saturating_sub(oy)) {
        for x in 0..tw.min(size.saturating_sub(ox)) {
            let a = data[y * tw + x];
            let idx = ((oy + y) * size + (ox + x)) * 4;
            rgba[idx] = 30;
            rgba[idx + 1] = 30;
            rgba[idx + 2] = 40;
            rgba[idx + 3] = a;
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([size, size], &rgba)
}

/// Reduce una mascara alfa a un lado maximo `max_dim` (promedio por bloque). Las puntas
/// .abr pueden ser enormes (hasta 4000px); a tamanos de pincel normales no se nota.
fn downscale_alpha(w: u32, h: u32, alpha: &[u8], max_dim: u32) -> (u32, u32, Vec<u8>) {
    let m = w.max(h);
    if m <= max_dim || w == 0 || h == 0 {
        return (w, h, alpha.to_vec());
    }
    let scale = max_dim as f32 / m as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    let mut out = vec![0u8; (nw * nh) as usize];
    for y in 0..nh {
        let y0 = (y * h / nh).min(h - 1);
        let y1 = (((y + 1) * h / nh).max(y0 + 1)).min(h);
        for x in 0..nw {
            let x0 = (x * w / nw).min(w - 1);
            let x1 = (((x + 1) * w / nw).max(x0 + 1)).min(w);
            let mut sum = 0u32;
            let mut cnt = 0u32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    sum += alpha[(yy * w + xx) as usize] as u32;
                    cnt += 1;
                }
            }
            out[(y * nw + x) as usize] = (sum / cnt.max(1)) as u8;
        }
    }
    (nw, nh, out)
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,ink_app=info,ink_core=info"),
    )
    .init();

    let event_loop = EventLoop::new().expect("crear event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("ejecutar app");
}
