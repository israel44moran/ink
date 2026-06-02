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

use std::sync::Arc;
use std::time::Instant;

use ink_core::{
    vec2, Brush, Camera, Document, InputSample, OneEuroFilter, Stroke, TextItem, Tool, Vec2, Vertex,
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

    // Ajustes (area de trabajo + interaccion) y geometria de rejilla.
    settings: Settings,
    grid_mesh: Vec<Vertex>,

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
            settings: Settings::default(),
            grid_mesh: Vec::new(),
            egui_ctx: egui::Context::default(),
            egui_state: None,
            ui: UiState::default(),
        }
    }

    fn start_stroke(&mut self, initial_pressure: f32) {
        self.commit_text();
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
        }
        self.ui.eyedropper = false;
        true
    }

    fn add_point(&mut self, raw_world: Vec2, pressure: f32) {
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
            self.active_mesh.clear();
            if let Some(st) = &self.active {
                st.tessellate(&mut self.active_mesh);
            }
            if let Some(g) = self.gpu.as_mut() {
                g.set_active(&self.active_mesh);
            }
        }
    }

    fn finish_stroke(&mut self) {
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

            WindowEvent::CursorMoved { position, .. } => {
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
                } else if self.drawing {
                    let world = self.camera.screen_to_world(cur);
                    let pressure = (1.0 - (speed / 2600.0).clamp(0.0, 0.7)).clamp(0.05, 1.0);
                    self.add_point(world, pressure);
                }

                self.cursor = cur;
                self.last_cursor = cur;
            }

            WindowEvent::MouseInput { state, button, .. } => match button {
                MouseButton::Left => match state {
                    ElementState::Pressed => {
                        if egui_consumed {
                            // Interaccion con la UI: no dibujar.
                        } else if self.try_eyedropper() {
                            // Cuentagotas: tomo el color y no dibujo.
                        } else if self.space_down {
                            self.panning = true;
                        } else if let Some(tool) = self.ui.active_tool() {
                            self.tool_press(tool);
                        } else {
                            self.start_stroke(0.5);
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
                            self.cursor = loc;
                            self.last_cursor = loc;
                            if self.try_eyedropper() {
                                // Cuentagotas: solo toma color.
                            } else if let Some(tool) = self.ui.active_tool() {
                                self.tool_press(tool);
                            } else {
                                self.start_stroke(pressure);
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
                let mut actions = UiActions::default();
                let ctx = self.egui_ctx.clone();
                #[allow(deprecated)]
                let full_output = ctx.run(raw_input, |ctx| {
                    actions = ui::build_panel(ctx, &mut self.ui, &mut self.brush, &mut self.settings, stats);
                    self.draw_overlays(ctx);
                });
                if let Some(s) = self.egui_state.as_mut() {
                    s.handle_platform_output(window.as_ref(), full_output.platform_output);
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

                let ppp = ctx.pixels_per_point();
                let primitives = ctx.tessellate(full_output.shapes, ppp);

                // Rejilla del lienzo (geometria que se dibuja detras de la tinta).
                self.grid_mesh.clear();
                ink_core::build_grid(
                    &mut self.grid_mesh,
                    self.settings.grid,
                    &self.camera,
                    self.settings.grid_size,
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
