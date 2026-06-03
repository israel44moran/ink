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
mod notebook;
#[cfg(windows)]
mod pen_win;
mod renderer;
mod settings;
mod ui;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use ink_core::{
    point_in_polygon, push_stamp_quad, stamp_path, vec2, Aabb, Brush, BrushSettings, Camera,
    Document, InputSample, StampVertex, Stroke, TextItem, TipKind, Tool, Vec2,
    Vertex,
};
use renderer::{present_mode_name, GpuState};
use settings::Settings;
use ui::{Stats, UiActions, UiState};

// Pinceles de Photoshop EMPOTRADOS en el binario: el programa "viene con" estos
// (de fabrica de PS). Default = 18 basicos modernos; Legacy = 214 heredados (incluye
// secos, humedos y efectos especiales heredados).
const DEFAULT_BRUSHES_ABR: &[u8] = include_bytes!("../assets/brushes/Default Brushes.abr");
const LEGACY_BRUSHES_ABR: &[u8] = include_bytes!("../assets/brushes/Legacy Brushes.abr");

/// Gesto de herramienta en curso (coordenadas de mundo).
enum Gesture {
    /// Seleccion por rectangulo (marquee).
    Marquee { start: Vec2, end: Vec2 },
    /// Moviendo los trazos seleccionados.
    Move { last: Vec2 },
    /// Lazo a mano alzada (Sector): selecciona trazos completos.
    Lasso { pts: Vec<Vec2> },
    /// Lazo que RECORTA (Lazo): parte los trazos por el contorno y selecciona solo lo de
    /// dentro.
    LassoCut { pts: Vec<Vec2> },
    /// Borrado duro a lo largo del arrastre.
    EraseHard { last: Vec2 },
    /// Borrado suave (atenuar) a lo largo del arrastre.
    EraseSoft { last: Vec2 },
    /// Empujar/smudge a lo largo del arrastre.
    Smudge { last: Vec2 },
}

/// Operacion de dibujo en el historial unificado de deshacer.
enum DrawOp {
    /// Un trazo procedural (vive en el Document, se deshace con doc.undo).
    Procedural,
    /// Un trazo de pincel de Photoshop: `count` vertices al final de ps_committed[tip].
    Ps { tip: u32, count: usize },
    /// Un trazo de GOMA (raster): su entrada esta en `mask_log` (mismo orden).
    EraseMask,
}

/// Operacion para rehacer (guarda lo necesario para reconstruir el trazo).
enum RedoOp {
    Procedural,
    Ps { tip: u32, verts: Vec<StampVertex> },
    EraseMask,
}

/// Un trazo de GOMA guardado (para deshacer y para reconstruir la mascara). Puede ser una
/// goma REDONDA (discos) o una goma con la FORMA de un pincel (estampados con textura suave).
#[derive(Clone)]
enum EraseStroke {
    /// Discos `[cx, cy, radio, tiempo]` (goma redonda; borrado duro).
    Discs(Vec<[f32; 4]>),
    /// Estampados de la forma de un pincel: punta `tip` + vertices (cada uno con su `time`).
    Stamps { tip: u32, verts: Vec<StampVertex> },
}

/// En que pantalla esta la app: la BIBLIOTECA de cuadernos o el LIENZO (editor).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Library,
    Canvas,
}

/// Configuracion persistente de un item de la rueda (lo que el usuario ajusta con los
/// popups: tamano, opacidad y suavidad). Se guarda por item para restaurarla al volver.
#[derive(Clone, Copy)]
struct ItemCfg {
    width: f32,
    opacity: f32,
    smoothing: f32,
}

/// Clave de `item_cfg` para un slot. `None` = no se persiste aqui (Vacio usa nada;
/// los pinceles de Photoshop tienen su propio `ps_settings_map`).
fn slot_key(slot: ui::SlotItem) -> Option<(u8, u32)> {
    match slot {
        ui::SlotItem::Brush(bi) => Some((0, bi as u32)),
        ui::SlotItem::Tool(ti) => Some((1, ti as u32)),
        ui::SlotItem::Eraser => Some((2, 0)),
        _ => None,
    }
}
use std::path::PathBuf;
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
    /// Suavizado "cuerda elastica" (como el Suavizado de Photoshop): posicion (en mundo) del
    /// punto que se dibuja, que persigue al cursor manteniendose a `string_radius` de el.
    string_pos: Vec2,
    /// Radio de la "cuerda" en MUNDO (px de pantalla / zoom), fijado al empezar el trazo
    /// segun la suavidad. 0 = sin suavizado (crudo).
    string_radius: f32,
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
    /// Estampados de pincel de Photoshop seleccionados por el lazo: tip -> indices de
    /// estampado (cada estampado son 6 vertices consecutivos en `ps_committed[tip]`).
    selected_stamps: HashMap<u32, Vec<usize>>,
    /// Lazo POLIGONAL en curso: vertices ya colocados (en mundo). Vacio = inactivo. A
    /// diferencia de los demas gestos, se construye con CLICS y se cierra con doble clic,
    /// clic en el primer vertice o Enter (Escape cancela).
    poly_lasso: Vec<Vec2>,
    /// Instante del ultimo clic del lazo poligonal (para detectar el doble clic de cierre).
    last_poly_click: Option<Instant>,
    texts: Vec<TextItem>,
    active_text: Option<usize>,

    // --- Pinceles texturizados estilo Photoshop (estampados) ---
    /// Catalogo de puntas cargadas de los .abr (mascara alfa de cada una).
    ps_brushes: Vec<ink_brush::SampledBrush>,
    /// Nombres de las categorias del catalogo (basicos de PS, heredados, packs...).
    ps_cat_names: Vec<String>,
    /// Indices de los pinceles que pertenecen a cada categoria (paralelo a ps_cat_names).
    ps_cat_members: Vec<Vec<u32>>,
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
    /// Ajustes por pincel PS (para que cada uno recuerde sus dinamicas al cambiar).
    ps_settings_map: HashMap<u32, BrushSettings>,
    /// Historial unificado de deshacer/rehacer (trazos procedurales + de pincel PS).
    undo_stack: Vec<DrawOp>,
    redo_stack: Vec<RedoOp>,
    /// Trazos de GOMA (discos de la goma redonda, o estampados con la forma de un pincel).
    /// El tiempo marca en la mascara que pixeles se borraron y cuando; un trazo se ve solo
    /// si su tiempo de creacion es mayor. Permite reconstruir la mascara y deshacer borrados.
    erase_strokes: Vec<EraseStroke>,
    /// Trazos de goma deshechos (para rehacer).
    erase_redo: Vec<EraseStroke>,
    /// Discos del trazo de goma REDONDA en curso (se mueve a `erase_strokes` al terminar).
    cur_erase: Vec<[f32; 4]>,
    /// Estampados del trazo de goma CON FORMA en curso (si `eraser_tip` es Some).
    cur_erase_stamps: Vec<StampVertex>,
    /// Forma de la goma: `None` = redonda (discos); `Some(tip)` = forma de ese pincel
    /// (estampados con textura suave). Lo elige el usuario en el panel "Mis pinceles".
    eraser_tip: Option<u32>,
    /// Reloj logico: cada trazo (de dibujo o de goma) toma un tiempo creciente. Los trazos
    /// guardan su tiempo; los borrados marcan ese tiempo en la mascara.
    tick: f32,
    /// Esquina (en mundo) de la VENTANA de mascara de borrado. Se mueve para seguir al
    /// contenido/camara (lienzo infinito); al moverla se reconstruye la mascara.
    mask_origin: Vec2,
    /// Modo GOMA global: al dibujar se BORRA (con el tamano del pincel activo), sin
    /// importar que pincel/forma este seleccionado. Cambiar de pincel no lo desactiva.
    eraser_mode: bool,
    /// Trazo de borrado en curso.
    erasing: bool,
    /// Ultima configuracion (tamano/opacidad/suavidad) de cada item de la rueda
    /// (pincel procedural, herramienta o goma). Se restaura al reseleccionarlo.
    item_cfg: HashMap<(u8, u32), ItemCfg>,
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

    // --- Cuadernos ---
    /// Pantalla actual: biblioteca de cuadernos o lienzo (editor).
    app_mode: AppMode,
    /// Lista de cuadernos para la biblioteca.
    notebooks: Vec<notebook::NotebookEntry>,
    /// Ruta del cuaderno abierto (donde se guarda).
    current_path: Option<std::path::PathBuf>,
    /// Paginas del cuaderno abierto (la pagina activa esta volcada en doc/texts/...).
    pages: Vec<notebook::PageData>,
    /// Indice de la pagina activa.
    current_page: usize,
    /// Hoja FIJADA: bloquea pan/zoom para que la pagina quede encajada (cuadernos de hojas).
    lock_page: bool,
    /// Acumulador del scroll para pasar de pagina con la rueda.
    wheel_accum: f32,
    /// Nombre que se escribe al crear un cuaderno nuevo.
    new_nb_name: String,
    /// Tipo del cuaderno nuevo: infinito (true) o con hojas (false).
    new_nb_infinite: bool,
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
            string_pos: Vec2::ZERO,
            string_radius: 0.0,
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
            selected_stamps: HashMap::new(),
            poly_lasso: Vec::new(),
            last_poly_click: None,
            texts: Vec::new(),
            active_text: None,
            ps_brushes: Vec::new(),
            ps_cat_names: Vec::new(),
            ps_cat_members: Vec::new(),
            ps_settings: None,
            ps_drawing: false,
            ps_samples: Vec::new(),
            ps_index: 0,
            ps_residual: 0.0,
            ps_active_verts: Vec::new(),
            ps_committed: HashMap::new(),
            ps_uploaded: HashSet::new(),
            ps_settings_map: HashMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            erase_strokes: Vec::new(),
            erase_redo: Vec::new(),
            cur_erase: Vec::new(),
            cur_erase_stamps: Vec::new(),
            eraser_tip: None,
            tick: 1.0,
            mask_origin: Vec2::ZERO,
            eraser_mode: false,
            erasing: false,
            item_cfg: HashMap::new(),
            ps_thumb_imgs: Vec::new(),
            ps_thumbs: Vec::new(),
            ps_packs: Vec::new(),
            settings: Settings::default(),
            grid_mesh: Vec::new(),
            eyedropper_ripple: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            ui: UiState::default(),
            app_mode: AppMode::Library,
            notebooks: Vec::new(),
            current_path: None,
            pages: vec![notebook::PageData::empty()],
            current_page: 0,
            lock_page: false,
            wheel_accum: 0.0,
            new_nb_name: String::new(),
            new_nb_infinite: true,
        }
    }

    /// ¿Hubo un evento tactil/lapiz hace muy poco? Los eventos de raton que llegan en
    /// esa ventana son SINTETICOS de Windows (eco del tacto) y deben ignorarse.
    fn touch_recent(&self) -> bool {
        self.last_touch.map_or(false, |t| t.elapsed().as_millis() < 300)
    }

    fn start_stroke(&mut self, initial_pressure: f32) {
        self.commit_text();
        // Modo GOMA: borrar en vez de dibujar (con el tamano del pincel activo).
        if self.eraser_mode {
            self.start_erase();
            return;
        }
        // Pincel de Photoshop activo: dibujar con estampados texturizados.
        if self.ps_settings.is_some() {
            self.start_stroke_ps(initial_pressure);
            return;
        }
        // Las herramientas (seleccion, etc.) y los slots vacios no dibujan tinta.
        if !self.ui.drawing_enabled() {
            return;
        }
        // Suavizado "cuerda elastica" (como el Suavizado de Photoshop), comun a todos los
        // pinceles. El radio (en mundo) se fija al empezar el trazo segun la suavidad.
        self.last_sample_time = Instant::now();
        let world = self.camera.screen_to_world(self.cursor);
        self.string_pos = world;
        self.string_radius = smoothing_string_radius_px(self.brush.smoothing) / self.camera.zoom.max(1e-4);
        let filtered = world;
        // Opacidad -> alfa del color del trazo.
        let mut b = self.brush;
        b.color[3] = self.brush.opacity.clamp(0.0, 1.0);
        let mut s = Stroke::new(b);
        s.push(InputSample {
            pos: filtered,
            pressure: initial_pressure.clamp(0.05, 1.0),
            erosion: 0.0,
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
        if self.erasing {
            self.do_erase(raw_world);
            return;
        }
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
        // Suavizado "cuerda" (Photoshop): el punto dibujado persigue al cursor manteniendose
        // a `string_radius`. Lag fijo (no crece con la velocidad).
        let d = raw_world - self.string_pos;
        let dist = d.length();
        if dist > self.string_radius {
            self.string_pos = raw_world - d / dist * self.string_radius;
        }
        let filtered = self.string_pos;

        let mut changed = false;
        if let Some(stroke) = self.active.as_mut() {
            // Espaciado minimo ~0.6 px en pantalla: muestras mas densas -> curvas mas fieles
            // (trazo de alta precision). El teselado incremental absorbe el coste extra.
            let min_d = (0.6 / self.camera.zoom).max(1e-4);
            if stroke.samples.is_empty() || (filtered - self.last_sample_pos).length() >= min_d {
                stroke.push(InputSample { pos: filtered, pressure, erosion: 0.0 });
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
        if self.erasing {
            self.finish_erase();
            return;
        }
        if self.ps_drawing {
            self.finish_stroke_ps();
            return;
        }
        self.drawing = false;
        if let Some(mut stroke) = self.active.take() {
            if !stroke.samples.is_empty() {
                // Marcar el tiempo de creacion: por la goma por timestamps, este trazo se
                // vera aunque pase por una zona borrada antes (su tiempo es mayor).
                stroke.time = self.tick;
                let mut mesh: Vec<Vertex> = Vec::new();
                stroke.tessellate(&mut mesh); // mesh con el time en cada vertice
                self.doc.add_stroke(stroke);
                let incremental = self.doc.last_add_was_incremental();
                if let Some(g) = self.gpu.as_mut() {
                    // Anexar solo el trazo nuevo; si no cupo o el doc reconstruyo, re-subir todo.
                    if !incremental || !g.append_committed(&mesh) {
                        g.set_committed(self.doc.committed_vertices());
                    }
                    g.set_active(&[]);
                }
                self.undo_stack.push(DrawOp::Procedural);
                self.redo_stack.clear();
                self.tick += 1.0;
            }
        }
        self.active_mesh.clear();
    }

    fn sync_committed(&mut self) {
        if let Some(g) = self.gpu.as_mut() {
            g.set_committed(self.doc.committed_vertices());
        }
    }

    // ===================== Seleccion de estampados (lazo en pinceles Ps) =====================

    /// Centro (en mundo) del estampado `i` dentro de `verts`. Cada estampado son 6
    /// vertices (un quad); el centro es el punto medio de dos esquinas opuestas (0 y 4).
    fn stamp_center(verts: &[StampVertex], i: usize) -> Vec2 {
        let b = i * 6;
        let tl = Vec2::new(verts[b].pos[0], verts[b].pos[1]);
        let br = Vec2::new(verts[b + 4].pos[0], verts[b + 4].pos[1]);
        (tl + br) * 0.5
    }

    /// Selecciona los estampados de pincel de Photoshop cuyo CENTRO cae dentro de `poly`.
    /// Como los estampados se colocan muy densos a lo largo del trazo, seleccionar por
    /// centro sigue el contorno con precision (a la resolucion del espaciado del pincel).
    /// Esto hace que el lazo funcione con CUALQUIER pincel Ps y cualquier tamano.
    fn select_stamps_in_polygon(&self, poly: &[Vec2]) -> HashMap<u32, Vec<usize>> {
        let mut out: HashMap<u32, Vec<usize>> = HashMap::new();
        if poly.len() < 3 {
            return out;
        }
        for (&tip, verts) in &self.ps_committed {
            let count = verts.len() / 6;
            let mut idxs = Vec::new();
            for i in 0..count {
                if point_in_polygon(Self::stamp_center(verts, i), poly) {
                    idxs.push(i);
                }
            }
            if !idxs.is_empty() {
                out.insert(tip, idxs);
            }
        }
        out
    }

    /// Lazo que RECORTA los estampados Ps por el contorno (corte EXACTO, no estampados
    /// enteros): reconstruye el buffer de cada tip subdividiendo los estampados que el
    /// contorno cruza, y marca como seleccionados los sub-estampados que quedan DENTRO.
    /// Los buffers afectados se re-suben a la GPU. Define `self.selected_stamps`.
    fn lasso_cut_stamps(&mut self, poly: &[Vec2]) {
        let mut sel: HashMap<u32, Vec<usize>> = HashMap::new();
        let tips: Vec<u32> = self.ps_committed.keys().copied().collect();
        let mut frags: Vec<(bool, [StampVertex; 6])> = Vec::new();
        for tip in tips {
            let src = match self.ps_committed.get(&tip) {
                Some(v) if v.len() >= 6 => v.clone(),
                _ => continue,
            };
            let count = src.len() / 6;
            let mut newbuf: Vec<StampVertex> = Vec::with_capacity(src.len());
            let mut idxs: Vec<usize> = Vec::new();
            for i in 0..count {
                frags.clear();
                ink_core::clip_stamp_by_polygon(&src[i * 6..i * 6 + 6], poly, &mut frags);
                for (inside, q) in frags.drain(..) {
                    if inside {
                        idxs.push(newbuf.len() / 6);
                    }
                    newbuf.extend_from_slice(&q);
                }
            }
            if idxs.is_empty() {
                continue; // nada dentro de este tip: dejar el buffer original intacto
            }
            self.ps_committed.insert(tip, newbuf);
            let snapshot = self.ps_committed.get(&tip).cloned();
            if let (Some(snapshot), Some(g)) = (snapshot, self.gpu.as_mut()) {
                g.set_committed_stamps(tip, &snapshot);
            }
            sel.insert(tip, idxs);
        }
        self.selected_stamps = sel;
    }

    /// Caja (AABB) que engloba TODA la seleccion: trazos procedurales + estampados Ps.
    fn selection_bounds(&self) -> Option<Aabb> {
        let mut acc = self.doc.bounds_of(&self.selected);
        for (&tip, idxs) in &self.selected_stamps {
            let Some(verts) = self.ps_committed.get(&tip) else { continue };
            for &i in idxs {
                let b = i * 6;
                for v in verts.iter().skip(b).take(6) {
                    let p = Vec2::new(v.pos[0], v.pos[1]);
                    acc = Some(match acc {
                        None => Aabb::from_points(p, p),
                        Some(mut a) => {
                            a.expand(p);
                            a
                        }
                    });
                }
            }
        }
        acc
    }

    /// Mueve los estampados seleccionados sumando `d` a sus vertices y re-sube a la GPU
    /// los buffers de los tips afectados.
    fn move_selected_stamps(&mut self, d: Vec2) {
        if d == Vec2::ZERO || self.selected_stamps.is_empty() {
            return;
        }
        let entries: Vec<(u32, Vec<usize>)> =
            self.selected_stamps.iter().map(|(&t, v)| (t, v.clone())).collect();
        for (tip, idxs) in entries {
            if let Some(verts) = self.ps_committed.get_mut(&tip) {
                for i in idxs {
                    let b = i * 6;
                    for v in verts.iter_mut().skip(b).take(6) {
                        v.pos[0] += d.x;
                        v.pos[1] += d.y;
                    }
                }
            }
            let snapshot = self.ps_committed.get(&tip).cloned();
            if let (Some(snapshot), Some(g)) = (snapshot, self.gpu.as_mut()) {
                g.set_committed_stamps(tip, &snapshot);
            }
        }
    }

    /// Hay algo seleccionado (trazos procedurales o estampados Ps).
    fn has_selection(&self) -> bool {
        !self.selected.is_empty() || self.selected_stamps.values().any(|v| !v.is_empty())
    }

    /// Limpia toda la seleccion (procedural + estampados).
    fn clear_selection(&mut self) {
        self.selected.clear();
        self.selected_stamps.clear();
    }

    // ===================== Cuadernos (biblioteca + guardado) =====================

    /// Aplica una PAGINA (su dibujo, texto y borrados) al estado vivo y la sube a la GPU.
    fn load_page(&mut self, i: usize) {
        let Some((doc, texts, erase, tick)) = self
            .pages
            .get(i)
            .map(|pg| (pg.doc.clone(), pg.texts.clone(), pg.erase_strokes.clone(), pg.tick.max(1.0)))
        else {
            return;
        };
        // Limpiar los ESTAMPADOS de Photoshop dibujados (no se guardan por pagina en esta
        // fase). OJO: NO tocar `ps_settings` (la config del pincel activo): debe persistir
        // entre paginas para poder seguir dibujando con el mismo pincel en la hoja nueva.
        let tips: Vec<u32> = self.ps_committed.keys().copied().collect();
        self.ps_committed.clear();
        self.ps_active_verts.clear();

        self.doc = doc;
        self.doc.refresh(); // reconstruye la malla horneada
        self.texts = texts;
        // En disco solo se guardan los discos de la goma redonda (los estampados de la goma
        // con forma, como los pinceles PS, no persisten por pagina en esta fase).
        self.erase_strokes = erase.into_iter().map(EraseStroke::Discs).collect();
        self.tick = tick;
        self.active_text = None;
        self.erase_redo.clear();
        self.cur_erase.clear();
        self.cur_erase_stamps.clear();
        self.clear_selection();
        self.cancel_poly_lasso();
        self.gesture = None;
        self.undo_stack.clear();
        self.redo_stack.clear();

        if let Some(g) = self.gpu.as_mut() {
            for t in tips {
                g.set_committed_stamps(t, &[]);
            }
            g.clear_active_stamps();
            g.set_active(&[]);
            g.set_committed(self.doc.committed_vertices());
        }
        self.rebuild_mask();
    }

    /// Vuelca el estado vivo a la pagina actual (antes de cambiar de pagina o guardar).
    fn stash_current_page(&mut self) {
        let doc = self.doc.clone();
        let texts = self.texts.clone();
        // Solo se guardan los discos (la goma con forma no persiste en disco aun).
        let erase: Vec<Vec<[f32; 4]>> = self
            .erase_strokes
            .iter()
            .filter_map(|e| if let EraseStroke::Discs(d) = e { Some(d.clone()) } else { None })
            .collect();
        let tick = self.tick;
        if let Some(pg) = self.pages.get_mut(self.current_page) {
            pg.doc = doc;
            pg.texts = texts;
            pg.erase_strokes = erase;
            pg.tick = tick;
        }
    }

    /// ¿La hoja esta fijada? (bloquea pan/zoom; solo en cuadernos de hojas).
    fn page_locked(&self) -> bool {
        self.lock_page && !matches!(self.settings.artboard, settings::Artboard::Infinite)
    }

    /// Centra la camara en la hoja (encajandola en el viewport) para cuadernos de hojas.
    fn center_on_page(&mut self) {
        if let Some((w, h)) = self.settings.artboard_size() {
            let vp = self.camera.viewport;
            let zoom = ((vp.x / w.max(1.0)).min(vp.y / h.max(1.0)) * 0.9).clamp(0.05, 50.0);
            self.camera.zoom = zoom;
            self.camera.center = Vec2::ZERO;
        }
    }

    /// Cambia a la pagina `i` (guardando la actual) y centra la vista en la hoja.
    fn switch_page(&mut self, i: usize) {
        if i >= self.pages.len() || i == self.current_page {
            return;
        }
        self.commit_text();
        self.stash_current_page();
        self.current_page = i;
        self.load_page(i);
        self.center_on_page();
    }

    /// Anade una hoja nueva al final del cuaderno y va a ella.
    fn add_page(&mut self) {
        self.commit_text();
        self.stash_current_page();
        self.pages.push(notebook::PageData::empty());
        let i = self.pages.len() - 1;
        self.current_page = i;
        self.load_page(i);
        self.center_on_page();
    }

    /// Carga un cuaderno (sus paginas) en el estado y reconstruye la GPU.
    fn apply_notebook(&mut self, nb: notebook::NotebookData) {
        self.commit_text();
        self.settings.artboard = if nb.infinite { settings::Artboard::Infinite } else { settings::Artboard::A4 };
        self.pages = nb.pages;
        if self.pages.is_empty() {
            self.pages.push(notebook::PageData::empty());
        }
        self.current_page = 0;
        self.load_page(0);
        if nb.infinite {
            self.camera.center = Vec2::ZERO;
        } else {
            self.center_on_page();
        }
    }

    /// Guarda el cuaderno abierto (si lo hay) en su archivo.
    fn save_current(&mut self) {
        let Some(path) = self.current_path.clone() else { return };
        self.commit_text();
        self.stash_current_page();
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("cuaderno").to_string();
        let infinite = matches!(self.settings.artboard, settings::Artboard::Infinite);
        let mut nb = notebook::NotebookData::new(&name, infinite);
        nb.pages = self.pages.clone();
        let _ = notebook::save(&nb, &path);
    }

    /// Abre un cuaderno desde disco y entra al lienzo.
    fn open_notebook(&mut self, path: PathBuf) {
        if let Some(nb) = notebook::load(&path) {
            self.apply_notebook(nb);
            self.current_path = Some(path);
            self.app_mode = AppMode::Canvas;
        }
    }

    /// Crea un cuaderno nuevo, lo guarda y lo abre.
    fn new_notebook(&mut self, name: &str, infinite: bool) {
        let name = if name.trim().is_empty() { "Cuaderno" } else { name.trim() };
        let nb = notebook::NotebookData::new(name, infinite);
        let path = notebook::path_for(name);
        let _ = notebook::save(&nb, &path);
        self.apply_notebook(nb);
        self.current_path = Some(path);
        self.app_mode = AppMode::Canvas;
    }

    /// Guarda el cuaderno actual y vuelve a la biblioteca (refrescando la lista).
    fn go_to_library(&mut self) {
        self.save_current();
        self.current_path = None;
        self.app_mode = AppMode::Library;
        self.notebooks = notebook::list();
    }

    /// Deshace la ULTIMA operacion de dibujo (procedural, de pincel PS o de goma), en orden.
    fn undo_op(&mut self) {
        match self.undo_stack.pop() {
            Some(DrawOp::Procedural) => {
                self.doc.undo();
                self.sync_committed();
                self.redo_stack.push(RedoOp::Procedural);
            }
            Some(DrawOp::Ps { tip, count }) => {
                if let Some(v) = self.ps_committed.get_mut(&tip) {
                    let at = v.len().saturating_sub(count);
                    let removed = v.split_off(at);
                    let verts = v.clone();
                    if let Some(g) = self.gpu.as_mut() {
                        g.set_committed_stamps(tip, &verts);
                    }
                    self.redo_stack.push(RedoOp::Ps { tip, verts: removed });
                }
            }
            Some(DrawOp::EraseMask) => {
                // Deshacer un trazo de goma: quitarlo y reconstruir la mascara.
                if let Some(discs) = self.erase_strokes.pop() {
                    self.erase_redo.push(discs);
                    self.rebuild_mask();
                }
                self.redo_stack.push(RedoOp::EraseMask);
            }
            None => {}
        }
    }

    /// Rehace la ultima operacion deshecha.
    fn redo_op(&mut self) {
        match self.redo_stack.pop() {
            Some(RedoOp::Procedural) => {
                self.doc.redo();
                self.sync_committed();
                self.undo_stack.push(DrawOp::Procedural);
            }
            Some(RedoOp::Ps { tip, verts }) => {
                let count = verts.len();
                let v = self.ps_committed.entry(tip).or_default();
                v.extend(verts);
                let vc = v.clone();
                if let Some(g) = self.gpu.as_mut() {
                    g.set_committed_stamps(tip, &vc);
                }
                self.undo_stack.push(DrawOp::Ps { tip, count });
            }
            Some(RedoOp::EraseMask) => {
                if let Some(discs) = self.erase_redo.pop() {
                    self.erase_strokes.push(discs);
                    self.rebuild_mask();
                }
                self.undo_stack.push(DrawOp::EraseMask);
            }
            None => {}
        }
    }

    /// ¿Hay algo que deshacer / rehacer? (historial unificado)
    fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }
    fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    // =====================================================================
    // Pinceles texturizados estilo Photoshop (estampados)
    // =====================================================================

    /// Anade un conjunto de puntas al catalogo como una CATEGORIA (reduce el alfa para
    /// controlar la RAM y genera las miniaturas; las texturas se suben bajo demanda).
    fn load_ps_brushes(&mut self, brushes: Vec<ink_brush::SampledBrush>, cat_name: &str) -> usize {
        if brushes.is_empty() {
            return 0;
        }
        // Obtener (o crear) la categoria.
        let cat = self.ps_cat_names.iter().position(|n| n == cat_name).unwrap_or_else(|| {
            self.ps_cat_names.push(cat_name.to_string());
            self.ps_cat_members.push(Vec::new());
            self.ps_cat_names.len() - 1
        });
        let mut count = 0;
        for mut b in brushes {
            let (w, h, data) = downscale_alpha(b.width, b.height, &b.alpha, 512);
            b.width = w;
            b.height = h;
            b.alpha = data;
            let idx = self.ps_brushes.len() as u32;
            self.ps_thumb_imgs.push(make_thumb(&b, 46));
            self.ps_thumbs.push(None);
            self.ps_brushes.push(b);
            self.ps_cat_members[cat].push(idx);
            count += 1;
        }
        count
    }

    /// Carga un pack .abr del disco al catalogo como una categoria (con nombres reales).
    fn load_ps_pack(&mut self, path: &str) -> usize {
        if self.ps_packs.iter().any(|(_, pp, loaded)| pp == path && *loaded) {
            return 0;
        }
        let cat_name = self
            .ps_packs
            .iter()
            .find(|(_, pp, _)| pp == path)
            .map(|(n, _, _)| n.clone())
            .unwrap_or_else(|| "Pack".to_string());
        match std::fs::read(path) {
            Ok(bytes) => {
                let before = self.ps_brushes.len();
                self.load_ps_named(&bytes, &cat_name);
                let n = self.ps_brushes.len() - before;
                if let Some(p) = self.ps_packs.iter_mut().find(|(_, pp, _)| pp == path) {
                    p.2 = true;
                }
                log::info!("Pincel PS: cargadas {n} puntas de {path}");
                n
            }
            Err(e) => {
                log::warn!("No se pudo leer {path}: {e}");
                0
            }
        }
    }

    /// Crea un pincel REDONDO procedural (punta generada por dureza) y lo activa.
    fn create_round_brush(&mut self, hardness: f32) {
        let idx = self.ps_brushes.len();
        let name = if hardness > 0.5 { "Redondo duro" } else { "Redondo suave" };
        let b = generate_round_brush(hardness, name, idx);
        self.load_ps_brushes(vec![b], "Mis pinceles");
        self.select_ps_brush(idx as u32);
    }

    /// Precarga los pinceles de fabrica de PS CLASIFICADOS en las 4 categorias
    /// (generales/secos/humedos/efectos), con sus nombres reales del bloque 'desc'.
    fn preload_ps_default(&mut self, bytes: &[u8]) {
        // Crear las 4 categorias en orden (aunque alguna quede vacia).
        for c in ["Pinceles generales", "Pinceles secos", "Pinceles húmedos", "Pinceles de efectos especiales"] {
            if !self.ps_cat_names.iter().any(|n| n == c) {
                self.ps_cat_names.push(c.to_string());
                self.ps_cat_members.push(Vec::new());
            }
        }
        // Generales: dos puntas redondas (difusa y definida).
        let i0 = self.ps_brushes.len();
        self.load_ps_brushes(vec![generate_round_brush(1.0, "Circular definido", i0)], "Pinceles generales");
        let i1 = self.ps_brushes.len();
        self.load_ps_brushes(vec![generate_round_brush(0.0, "Circular difuso", i1)], "Pinceles generales");
        // Resto: clasificar cada punta de fabrica por su nombre (del 'desc').
        let puntas = ink_brush::parse_abr(bytes).unwrap_or_default();
        let presets = ink_brush::parse_presets(bytes);
        let mut name_by_uuid: HashMap<String, String> = HashMap::new();
        for p in presets {
            name_by_uuid.entry(p.uuid).or_insert(p.name);
        }
        for mut b in puntas {
            let name = name_by_uuid.get(&b.id).cloned();
            let cat = classify_brush(name.as_deref().unwrap_or(""));
            b.name = name;
            self.load_ps_brushes(vec![b], cat);
        }
    }

    /// Carga puntas desde bytes aplicando los NOMBRES reales del bloque 'desc', en una
    /// sola categoria `cat_name`.
    fn load_ps_named(&mut self, bytes: &[u8], cat_name: &str) {
        let presets = ink_brush::parse_presets(bytes);
        let mut name_by_uuid: HashMap<String, String> = HashMap::new();
        for p in presets {
            name_by_uuid.entry(p.uuid).or_insert(p.name);
        }
        let puntas: Vec<_> = ink_brush::parse_abr(bytes)
            .unwrap_or_default()
            .into_iter()
            .map(|mut b| {
                b.name = name_by_uuid.get(&b.id).cloned();
                b
            })
            .collect();
        self.load_ps_brushes(puntas, cat_name);
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

    /// Guarda los ajustes del pincel PS activo (si hay) en el mapa por-pincel, para
    /// que no se pierdan al cambiar de pincel.
    fn save_ps_settings(&mut self) {
        if let Some(s) = self.ps_settings.clone() {
            if let TipKind::Sampled(cur) = s.tip {
                self.ps_settings_map.insert(cur, s);
            }
        }
    }

    /// Activa el pincel PS `i`: recupera sus ajustes guardados o crea unos por defecto
    /// (presion -> tamano y flujo, como Photoshop). No reasigna ningun slot.
    fn activate_ps_brush(&mut self, i: u32) {
        if !self.ensure_tip(i) {
            return;
        }
        self.save_ps_settings(); // preservar los del pincel anterior
        let s = self.ps_settings_map.get(&i).cloned().unwrap_or_else(|| {
            let mut s = BrushSettings::default();
            s.tip = TipKind::Sampled(i);
            s.name = self
                .ps_brushes
                .get(i as usize)
                .and_then(|b| b.name.clone())
                .unwrap_or_else(|| format!("Pincel {}", i + 1));
            s.size = 40.0;
            s.spacing = 0.10;
            s.shape_dyn = true;
            s.size_control = ink_core::DynControl::PenPressure;
            s.min_diameter = 0.0;
            s.transfer_on = true;
            s.flow_control = ink_core::DynControl::PenPressure;
            s
        });
        self.ps_settings = Some(s);
    }

    /// Asigna el pincel PS `i` al slot ACTIVO de la rueda y lo activa.
    fn select_ps_brush(&mut self, i: u32) {
        let seg = self.ui.selected_seg;
        if let Some(slot) = self.ui.slots.get_mut(seg) {
            *slot = ui::SlotItem::PsBrush(i);
        }
        self.activate_ps_brush(i);
    }

    fn start_stroke_ps(&mut self, pressure: f32) {
        self.ps_drawing = true;
        self.ps_samples.clear();
        self.ps_index = 0;
        self.ps_residual = 0.0;
        self.ps_active_verts.clear();
        let world = self.camera.screen_to_world(self.cursor);
        // Suavizado "cuerda" (Photoshop), igual que los pinceles basicos.
        self.last_sample_time = Instant::now();
        self.string_pos = world;
        self.string_radius = smoothing_string_radius_px(self.brush.smoothing) / self.camera.zoom.max(1e-4);
        let f = world;
        self.last_sample_pos = f;
        self.ps_samples.push(InputSample { pos: f, pressure: pressure.clamp(0.05, 1.0), erosion: 0.0 });
        if let Some(g) = self.gpu.as_mut() {
            g.clear_active_stamps();
        }
    }

    /// Color de los estampados del pincel PS activo (el color del pincel).
    fn ps_brush_rgb(&self) -> [f32; 3] {
        [self.brush.color[0], self.brush.color[1], self.brush.color[2]]
    }

    /// Radio de borrado de la goma (medio diametro del pincel/tamano activo).
    fn eraser_radius(&self) -> f32 {
        (self.brush.width * 0.5).max(2.0)
    }

    /// Asegura que la ventana de mascara cubra el punto `p` (mundo). Si `p` se acerca al
    /// borde de la ventana, la RE-CENTRA en `p` y reconstruye la mascara desde los trazos
    /// de goma guardados (no se pierden). Permite borrar en el lienzo infinito.
    fn ensure_mask_covers(&mut self, p: Vec2) {
        let Some(w) = self.gpu.as_ref().map(|g| g.mask_world()) else { return };
        let half = w * 0.5;
        let center = self.mask_origin + Vec2::splat(half);
        let safe = half * 0.5; // re-centrar si el punto sale del 50% central
        if (p - center).abs().max_element() > safe {
            self.mask_origin = p - Vec2::splat(half);
            if let Some(g) = self.gpu.as_mut() {
                g.set_mask_origin([self.mask_origin.x, self.mask_origin.y]);
            }
            // Sin borrados, la mascara es 0 en todas partes: basta mover el origen.
            if !self.erase_strokes.is_empty() {
                self.rebuild_mask();
            }
        }
    }

    /// BrushSettings para la goma CON FORMA: la forma/dinamicas del pincel `tip`, pero con el
    /// tamano de la goma (el de la rueda).
    fn eraser_brush_settings(&self, tip: u32) -> BrushSettings {
        let mut s = self.ps_settings_map.get(&tip).cloned().unwrap_or_default();
        s.tip = TipKind::Sampled(tip);
        s.size = self.brush.width.max(2.0);
        s
    }

    /// Estampa un tramo del trazo de goma con forma (entre las dos ultimas muestras de
    /// `ps_samples`, reutilizado durante el borrado) en la mascara y en `cur_erase_stamps`.
    fn do_erase_stamp(&mut self, tip: u32) {
        let n = self.ps_samples.len();
        if n == 0 {
            return;
        }
        let settings = self.eraser_brush_settings(tip);
        let slice = if n == 1 { &self.ps_samples[0..1] } else { &self.ps_samples[n - 2..n] };
        let out = stamp_path(slice, &settings, self.ps_index, self.ps_residual);
        self.ps_index = out.next_index;
        self.ps_residual = out.residual;
        if out.stamps.is_empty() {
            return;
        }
        let aspect = self.gpu.as_ref().map_or(1.0, |g| g.tip_aspect(tip));
        let mut verts: Vec<StampVertex> = Vec::new();
        for st in &out.stamps {
            push_stamp_quad(&mut verts, st, aspect, [1.0, 1.0, 1.0]); // color irrelevante: solo cuenta la cobertura
        }
        for v in &mut verts {
            v.time = self.tick;
        }
        self.cur_erase_stamps.extend_from_slice(&verts);
        if let Some(g) = self.gpu.as_mut() {
            g.erase_mask_stamps(tip, &verts);
        }
    }

    /// Inicia un trazo de GOMA (borrado raster por timestamps): discos (redonda) o estampados
    /// con la forma de un pincel (`eraser_tip`).
    fn start_erase(&mut self) {
        self.erasing = true;
        self.cur_erase.clear();
        self.cur_erase_stamps.clear();
        let w = self.camera.screen_to_world(self.cursor);
        self.last_sample_pos = w;
        self.ensure_mask_covers(w);
        if let Some(tip) = self.eraser_tip {
            self.ensure_tip(tip);
            self.ps_index = 0;
            self.ps_residual = 0.0;
            self.ps_samples.clear();
            self.ps_samples.push(InputSample { pos: w, pressure: 1.0, erosion: 0.0 });
            self.do_erase_stamp(tip); // estampa el punto inicial (un toque)
        } else {
            let disc = [w.x, w.y, self.eraser_radius(), self.tick];
            self.cur_erase.push(disc);
            if let Some(g) = self.gpu.as_mut() {
                g.erase_mask(&[disc]);
            }
        }
    }

    /// Continua el borrado en `world` (interpola para no dejar huecos al mover rapido).
    /// Todos los discos del movimiento se borran en UN solo draw (instanciado), con el
    /// MISMO tiempo (el del trazo de goma en curso).
    fn do_erase(&mut self, world: Vec2) {
        if let Some(tip) = self.eraser_tip {
            // Goma con forma: anadir muestra y estampar el tramo nuevo.
            self.ps_samples.push(InputSample { pos: world, pressure: 1.0, erosion: 0.0 });
            self.do_erase_stamp(tip);
            self.last_sample_pos = world;
            return;
        }
        let r = self.eraser_radius();
        let t = self.tick;
        let from = self.last_sample_pos;
        let seg = world - from;
        let len = seg.length();
        let step = (r * 0.4).max(0.75); // pasos finos para que los discos solapen
        let n = (len / step).ceil().max(1.0) as usize;
        let mut new_discs: Vec<[f32; 4]> = Vec::with_capacity(n);
        for i in 1..=n {
            let p = from + seg * (i as f32 / n as f32);
            new_discs.push([p.x, p.y, r, t]);
        }
        if !new_discs.is_empty() {
            self.cur_erase.extend_from_slice(&new_discs);
            if let Some(g) = self.gpu.as_mut() {
                g.erase_mask(&new_discs);
            }
        }
        self.last_sample_pos = world;
    }

    /// Finaliza el trazo de goma: lo guarda (para deshacer/reconstruir) y avanza el reloj.
    fn finish_erase(&mut self) {
        self.erasing = false;
        let stroke = if !self.cur_erase_stamps.is_empty() {
            let verts = std::mem::take(&mut self.cur_erase_stamps);
            self.eraser_tip.map(|tip| EraseStroke::Stamps { tip, verts })
        } else if !self.cur_erase.is_empty() {
            Some(EraseStroke::Discs(std::mem::take(&mut self.cur_erase)))
        } else {
            None
        };
        self.cur_erase.clear();
        self.cur_erase_stamps.clear();
        self.ps_samples.clear(); // se reutilizo como buffer del trazo de goma con forma
        if let Some(es) = stroke {
            self.erase_strokes.push(es);
            self.erase_redo.clear();
            self.undo_stack.push(DrawOp::EraseMask);
            self.redo_stack.clear();
            self.tick += 1.0;
        }
    }

    /// Reconstruye la mascara desde cero: limpia y re-aplica todos los trazos de goma (discos
    /// y estampados) en orden de creacion; donde se solapan, el de mayor tiempo gana.
    fn rebuild_mask(&mut self) {
        let strokes = self.erase_strokes.clone();
        if let Some(g) = self.gpu.as_mut() {
            g.clear_mask();
        }
        for es in &strokes {
            match es {
                EraseStroke::Discs(discs) => {
                    if let Some(g) = self.gpu.as_mut() {
                        g.erase_mask(discs);
                    }
                }
                EraseStroke::Stamps { tip, verts } => {
                    self.ensure_tip(*tip);
                    if let Some(g) = self.gpu.as_mut() {
                        g.erase_mask_stamps(*tip, verts);
                    }
                }
            }
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
        // Suavizado "cuerda" (Photoshop): el punto dibujado persigue al cursor a `string_radius`.
        let d = raw_world - self.string_pos;
        let dist = d.length();
        if dist > self.string_radius {
            self.string_pos = raw_world - d / dist * self.string_radius;
        }
        let f = self.string_pos;
        let min_d = (1.0 / self.camera.zoom).max(1e-4);
        if self.ps_samples.len() > 1 && (f - self.last_sample_pos).length() < min_d {
            return;
        }
        self.last_sample_pos = f;
        self.last_sample_time = now;
        self.ps_samples.push(InputSample { pos: f, pressure, erosion: 0.0 });

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
        let rgb = self.ps_brush_rgb();
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
            let rgb = self.ps_brush_rgb();
            for st in &out.stamps {
                push_stamp_quad(&mut self.ps_active_verts, st, aspect, rgb);
            }
        }
        if !self.ps_active_verts.is_empty() {
            let count = self.ps_active_verts.len();
            // Marcar el tiempo de creacion en los estampados (goma por timestamps).
            for v in &mut self.ps_active_verts {
                v.time = self.tick;
            }
            let v = self.ps_committed.entry(tip).or_default();
            v.extend_from_slice(&self.ps_active_verts);
            let verts = v.clone();
            if let Some(g) = self.gpu.as_mut() {
                g.set_committed_stamps(tip, &verts);
            }
            self.undo_stack.push(DrawOp::Ps { tip, count });
            self.redo_stack.clear();
            self.tick += 1.0;
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
            Tool::Select | Tool::Sector | Tool::Lasso => {
                // Si presiono dentro de la seleccion existente, la muevo.
                if self.has_selection() {
                    if let Some(bb) = self.selection_bounds() {
                        if bb.contains(w) {
                            self.gesture = Some(Gesture::Move { last: w });
                            return;
                        }
                    }
                }
                self.clear_selection();
                self.gesture = Some(match tool {
                    Tool::Select => Gesture::Marquee { start: w, end: w },
                    Tool::Sector => Gesture::Lasso { pts: vec![w] },
                    _ => Gesture::LassoCut { pts: vec![w] }, // Lazo (recorta)
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
            // El lazo poligonal se construye con clics (poly_lasso_click), no con press/drag.
            Tool::PolyLasso => {}
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
            Some(Gesture::Lasso { pts }) | Some(Gesture::LassoCut { pts }) => {
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
                self.move_selected_stamps(d);
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
                // Estampados Ps: dentro del rectangulo (como poligono de 4 esquinas).
                let rect = [
                    start,
                    Vec2::new(end.x, start.y),
                    end,
                    Vec2::new(start.x, end.y),
                ];
                self.selected_stamps = self.select_stamps_in_polygon(&rect);
            }
            Some(Gesture::Lasso { pts }) => {
                self.selected = self.doc.strokes_in_polygon(&pts);
                self.selected_stamps = self.select_stamps_in_polygon(&pts);
            }
            Some(Gesture::LassoCut { pts }) => {
                // Recortar los trazos por el contorno y seleccionar solo lo de dentro.
                self.selected = self.doc.lasso_split(&pts);
                // Estampados Ps: recorte EXACTO por la curva (subdividiendo los del borde),
                // no estampados enteros. Define self.selected_stamps.
                self.lasso_cut_stamps(&pts);
                self.sync_committed();
            }
            _ => {}
        }
    }

    // ===================== Lazo poligonal (clic a clic, como Photoshop) =====================

    /// Un clic del lazo poligonal: anade un vertice o CIERRA el contorno (si el clic cae
    /// cerca del primer vertice, o es un doble clic, con al menos 3 vertices).
    fn poly_lasso_click(&mut self) {
        let w = self.camera.screen_to_world(self.cursor);
        let now = Instant::now();
        let zoom = self.camera.zoom.max(1e-4);
        let close = 12.0 / zoom; // "cerca" = ~12 px de pantalla, en unidades de mundo
        if self.poly_lasso.len() >= 3 {
            // Clic sobre el primer vertice -> cerrar.
            if (w - self.poly_lasso[0]).length() <= close {
                self.finish_poly_lasso();
                return;
            }
            // Doble clic (rapido y cerca del ultimo) -> cerrar.
            if let Some(t0) = self.last_poly_click {
                let last = *self.poly_lasso.last().unwrap();
                if now.duration_since(t0).as_millis() < 350 && (w - last).length() <= close {
                    self.finish_poly_lasso();
                    return;
                }
            }
        }
        self.poly_lasso.push(w);
        self.last_poly_click = Some(now);
    }

    /// Cierra el lazo poligonal y aplica el recorte (igual que el lazo a mano alzada).
    fn finish_poly_lasso(&mut self) {
        let pts = std::mem::take(&mut self.poly_lasso);
        self.last_poly_click = None;
        if pts.len() >= 3 {
            self.selected = self.doc.lasso_split(&pts);
            self.lasso_cut_stamps(&pts);
            self.sync_committed();
        }
    }

    /// Cancela el lazo poligonal en curso (sin recortar nada).
    fn cancel_poly_lasso(&mut self) {
        self.poly_lasso.clear();
        self.last_poly_click = None;
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
        // Se dibuja en una capa de FONDO para que quede DEBAJO de la rueda y los paneles, asi
        // el margen de la hoja (en los cuadernos) no cruza por encima de la rueda.
        if let Some((aw, ah)) = self.settings.artboard_size() {
            let r = Rect::from_two_pos(to_pt(Vec2::new(-aw * 0.5, -ah * 0.5)), to_pt(Vec2::new(aw * 0.5, ah * 0.5)));
            let bp = ctx.layer_painter(LayerId::new(Order::Background, egui::Id::new("artboard_frame")));
            bp.rect_stroke(r, egui::CornerRadius::ZERO, Stroke::new(1.5, Color32::from_gray(160)), StrokeKind::Outside);
        }

        // Cursor de la GOMA: anillo del tamano real de borrado (estilo Photoshop).
        if self.eraser_mode {
            let c = Pos2::new(self.cursor.x / ppp, self.cursor.y / ppp);
            let rr = (self.eraser_radius() * cam.zoom / ppp).max(3.0);
            // Doble contorno (oscuro + claro) para que se vea sobre cualquier fondo.
            p.circle_stroke(c, rr, Stroke::new(1.5, Color32::from_rgba_unmultiplied(20, 20, 20, 210)));
            p.circle_stroke(c, rr + 1.5, Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 170)));
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
        if self.settings.highlight_selection && self.has_selection() {
            if let Some(bb) = self.selection_bounds() {
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
            Some(Gesture::Lasso { pts }) | Some(Gesture::LassoCut { pts }) => {
                if pts.len() >= 2 {
                    let mut sp: Vec<Pos2> = pts.iter().map(|w| to_pt(*w)).collect();
                    p.add(Shape::line(sp.clone(), Stroke::new(1.5, accent)));
                    // Cerrar el contorno con una linea punteada hacia el inicio.
                    if let (Some(&first), Some(&last)) = (sp.first(), sp.last()) {
                        sp.clear();
                        p.add(Shape::dashed_line(&[last, first], Stroke::new(1.0, accent), 5.0, 4.0));
                    }
                }
            }
            _ => {}
        }

        // Lazo POLIGONAL en curso: segmentos rectos ya colocados + recta "elastica" hasta el
        // cursor, con los vertices marcados y el primero resaltado (donde se cierra).
        if self.ui.active_tool() == Some(Tool::PolyLasso) && !self.poly_lasso.is_empty() {
            let sp: Vec<Pos2> = self.poly_lasso.iter().map(|w| to_pt(*w)).collect();
            if sp.len() >= 2 {
                p.add(Shape::line(sp.clone(), Stroke::new(1.5, accent)));
            }
            let cur = Pos2::new(self.cursor.x / ppp, self.cursor.y / ppp);
            if let Some(&last) = sp.last() {
                p.add(Shape::dashed_line(&[last, cur], Stroke::new(1.0, accent), 5.0, 4.0));
            }
            for (i, pt) in sp.iter().enumerate() {
                if i == 0 {
                    p.circle_filled(*pt, 4.0, accent);
                    p.circle_stroke(*pt, 6.5, Stroke::new(1.0, accent));
                } else {
                    p.circle_filled(*pt, 2.5, accent);
                }
            }
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

        // Windows: interceptar los mensajes de puntero para leer el boton del lapiz
        // (barrel) directamente, como Photoshop.
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = window.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    pen_win::install(w.hwnd.get());
                }
            }
        }

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

        // Pinceles de Photoshop PRECARGADOS (empotrados en el programa), clasificados
        // en las 4 categorias de PS con nombres reales + los heredados.
        self.preload_ps_default(DEFAULT_BRUSHES_ABR);
        self.load_ps_named(LEGACY_BRUSHES_ABR, "Pinceles heredados (Photoshop)");
        // Packs .abr del usuario en disco: disponibles para cargar bajo demanda.
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let base = format!(r"{home}\Downloads\Photoshop Brushes");
        self.ps_packs = scan_packs(&base);

        // Cargar la lista de cuadernos guardados y empezar en la biblioteca.
        self.notebooks = notebook::list();
        self.app_mode = AppMode::Library;

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
                    // Si la hoja esta fijada, no se mueve la camara.
                    if !self.page_locked() {
                        let delta = cur - self.last_cursor;
                        self.camera.pan_pixels(delta);
                    }
                } else if self.gesture.is_some() {
                    self.cursor = cur;
                    self.tool_drag();
                } else if self.drawing || self.ps_drawing || self.erasing {
                    let world = self.camera.screen_to_world(cur);
                    let pressure = (1.0 - (speed / 2600.0).clamp(0.0, 0.7)).clamp(0.05, 1.0);
                    self.add_point(world, pressure);
                }

                self.cursor = cur;
                self.last_cursor = cur;
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } if self.touch_recent() => {
                // Clic IZQUIERDO de raton SINTETICO (eco del lapiz/tacto): ignorar para no
                // iniciar un segundo trazo encima del tactil. Los botones derecho/medio del
                // lapiz (atajos) SI deben pasar.
                let _ = state;
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
                            // Tocar el lienzo cierra los paneles flotantes (color, "Mis
                            // pinceles", selector PS y Ajustes) para no estorbar al dibujar.
                            self.ui.show_colors = false;
                            self.ui.brush_panel = false;
                            self.ui.show_ps_panel = false;
                            self.ui.show_brush_settings = false;
                            // Dibujar/usar herramienta cierra los deslizadores de la rueda
                            // (tamano / opacidad / suavidad).
                            self.ui.popup = ui::Popup::None;
                            if self.space_down {
                                self.panning = true;
                            } else if self.eraser_mode {
                                // Goma activa: borra la zona tocada (prioridad maxima).
                                self.start_stroke(0.5);
                            } else if self.ps_settings.is_some() {
                                // Pincel PS activo: tiene prioridad sobre herramientas.
                                self.start_stroke(0.5);
                            } else if let Some(tool) = self.ui.active_tool() {
                                if tool == Tool::PolyLasso {
                                    self.poly_lasso_click();
                                } else {
                                    self.tool_press(tool);
                                }
                            } else {
                                self.start_stroke(0.5);
                            }
                        }
                    }
                    ElementState::Released => {
                        // Siempre cerramos el trazo/pan/gesto para no quedar "pegados".
                        if self.panning {
                            self.panning = false;
                        } else if self.ui.active_tool() == Some(Tool::PolyLasso) {
                            // El lazo poligonal se cierra por clic/doble clic/Enter, no al soltar.
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
                // Boton inferior del lapiz (los drivers de tableta lo mapean a clic
                // derecho): abre/cierra "Ajustes del pincel" JUSTO en el puntero.
                MouseButton::Right => {
                    if state == ElementState::Pressed {
                        self.ui.show_brush_settings = !self.ui.show_brush_settings;
                        if self.ui.show_brush_settings {
                            let ppp = self.egui_ctx.pixels_per_point().max(0.01);
                            self.ui.brush_settings_pos = egui::pos2(self.cursor.x / ppp, self.cursor.y / ppp);
                        }
                    }
                }
                _ => {}
            },

            WindowEvent::MouseWheel { delta, .. } => {
                if !egui_consumed && self.app_mode == AppMode::Canvas {
                    let amount = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 120.0,
                    };
                    if amount != 0.0 {
                        if matches!(self.settings.artboard, settings::Artboard::Infinite) {
                            // Lienzo infinito: la rueda hace zoom.
                            let factor = 1.12_f32.powf(amount);
                            self.camera.zoom_at(self.cursor, factor);
                        } else {
                            // Cuaderno de hojas: la rueda pasa de pagina (abajo = siguiente).
                            self.wheel_accum += amount;
                            while self.wheel_accum <= -1.0 {
                                if self.current_page + 1 < self.pages.len() {
                                    self.switch_page(self.current_page + 1);
                                }
                                self.wheel_accum += 1.0;
                            }
                            while self.wheel_accum >= 1.0 {
                                if self.current_page > 0 {
                                    self.switch_page(self.current_page - 1);
                                }
                                self.wheel_accum -= 1.0;
                            }
                        }
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
                                self.ui.show_colors = false; // tocar el lienzo cierra los selectores
                                self.ui.brush_panel = false;
                                self.ui.show_ps_panel = false;
                                self.ui.show_brush_settings = false;
                                self.ui.popup = ui::Popup::None; // y los deslizadores de la rueda
                                if self.eraser_mode {
                                    self.start_stroke(pressure);
                                } else if self.ps_settings.is_some() {
                                    self.start_stroke(pressure);
                                } else if let Some(tool) = self.ui.active_tool() {
                                    if tool == Tool::PolyLasso {
                                        self.poly_lasso_click();
                                    } else {
                                        self.tool_press(tool);
                                    }
                                } else {
                                    self.start_stroke(pressure);
                                }
                            }
                        }
                    }
                    TouchPhase::Moved => {
                        self.cursor = loc;
                        if self.ui.active_tool() == Some(Tool::PolyLasso) {
                            // Lazo poligonal: el toque-arrastre solo mueve el preview; no dibuja.
                        } else if self.gesture.is_some() {
                            self.tool_drag();
                        } else {
                            let world = self.camera.screen_to_world(loc);
                            self.add_point(world, pressure);
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        if self.ui.active_tool() == Some(Tool::PolyLasso) {
                            // El lazo poligonal se cierra por toque en el inicio/doble toque/Enter.
                        } else if self.gesture.is_some() {
                            self.tool_release();
                        } else {
                            self.finish_stroke();
                        }
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } if !egui_consumed && self.app_mode == AppMode::Canvas => {
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

                // Lazo poligonal en curso: Enter cierra el contorno, Escape lo cancela. Se
                // maneja aqui para que Escape NO cierre la app mientras se traza.
                if !self.poly_lasso.is_empty() && pressed {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::Enter) | PhysicalKey::Code(KeyCode::NumpadEnter) => {
                            self.finish_poly_lasso();
                            return;
                        }
                        PhysicalKey::Code(KeyCode::Escape) => {
                            self.cancel_poly_lasso();
                            return;
                        }
                        _ => {}
                    }
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
                            self.clear_selection();
                            self.active_text = None;
                            let tips: Vec<u32> = self.ps_committed.keys().copied().collect();
                            self.ps_committed.clear();
                            if let Some(g) = self.gpu.as_mut() {
                                for t in tips {
                                    g.set_committed_stamps(t, &[]);
                                }
                            }
                            self.undo_stack.clear();
                            self.redo_stack.clear();
                            self.sync_committed();
                        }
                        KeyCode::KeyZ => self.undo_op(),
                        KeyCode::KeyY => self.redo_op(),
                        // Abrir/cerrar "Ajustes del pincel" en el puntero. Pensado para
                        // mapear un BOTON DEL LAPIZ a esta tecla en el driver de la tableta
                        // (los botones del stylus llegan como clic, no distinguibles; una
                        // tecla si es inequivoca).
                        KeyCode::KeyB => {
                            self.ui.show_brush_settings = !self.ui.show_brush_settings;
                            if self.ui.show_brush_settings {
                                let ppp = self.egui_ctx.pixels_per_point().max(0.01);
                                self.ui.brush_settings_pos = egui::pos2(self.cursor.x / ppp, self.cursor.y / ppp);
                            }
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

                // Mantener la ventana de mascara centrada en lo que se ve: al hacer pan a
                // una zona donde se borro, se reconstruye y se ven los borrados (infinito).
                let view_center = self.camera.screen_to_world(self.camera.viewport * 0.5);
                self.ensure_mask_covers(view_center);

                // Windows: boton del lapiz (barrel) -> abrir/cerrar "Ajustes del pincel".
                #[cfg(windows)]
                {
                    let clicks = pen_win::take_barrel_clicks();
                    for _ in 0..clicks {
                        self.ui.show_brush_settings = !self.ui.show_brush_settings;
                        if self.ui.show_brush_settings {
                            let ppp = self.egui_ctx.pixels_per_point().max(0.01);
                            self.ui.brush_settings_pos = egui::pos2(self.cursor.x / ppp, self.cursor.y / ppp);
                        }
                    }
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
                    can_undo: self.can_undo(),
                    can_redo: self.can_redo(),
                };
                // Subir a egui las miniaturas de pincel que falten (carga diferida).
                for i in 0..self.ps_thumbs.len() {
                    if self.ps_thumbs[i].is_none() {
                        let img = self.ps_thumb_imgs[i].clone();
                        let tex = self.egui_ctx.load_texture(format!("psthumb{i}"), img, egui::TextureOptions::LINEAR);
                        self.ps_thumbs[i] = Some(tex);
                    }
                }
                // Pasar a la rueda las miniaturas de los pinceles PS (para los slots PsBrush).
                self.ui.ps_thumb_ids = self.ps_thumbs.iter().map(|t| t.as_ref().map(|h| h.id())).collect();
                // Espejos del catalogo PS para el panel "Mis pinceles" (solo si cambio el nº de
                // pinceles o categorias, p.ej. al cargar un pack; evita clonar cada frame).
                if self.ui.ps_brush_names.len() != self.ps_brushes.len()
                    || self.ui.ps_cat_names.len() != self.ps_cat_names.len()
                {
                    self.ui.ps_brush_names = self
                        .ps_brushes
                        .iter()
                        .enumerate()
                        .map(|(i, b)| b.name.clone().unwrap_or_else(|| format!("Pincel {}", i + 1)))
                        .collect();
                    self.ui.ps_cat_names = self.ps_cat_names.clone();
                    self.ui.ps_cat_members = self.ps_cat_members.clone();
                }
                self.ui.ps_pack_labels = self.ps_packs.iter().map(|(n, _p, l)| (n.clone(), *l)).collect();
                // Nombre de la forma actual de la goma (None = redonda), para el panel.
                self.ui.eraser_shape = self.eraser_tip.map(|t| {
                    self.ui.ps_brush_names.get(t as usize).cloned().unwrap_or_else(|| format!("Pincel {}", t + 1))
                });

                // Sincronizacion rueda <-> "Ajustes del pincel": el pincel PS comparte el
                // tamano y la opacidad con la rueda. Al inicio del frame la rueda parte del
                // valor del pincel PS; al final se propaga el que haya cambiado.
                let ps_sync_prev = self.ps_settings.as_ref().map(|s| (s.size, s.opacity));
                if let Some((sz, op)) = ps_sync_prev {
                    self.brush.width = sz;
                    self.brush.opacity = op;
                }

                let mut actions = UiActions::default();
                let mut ps_select: Option<u32> = None;
                let mut ps_load_pack: Option<String> = None;
                let mut ps_load_all = false;
                let mut ps_new_round: Option<f32> = None;
                // Acciones de la BIBLIOTECA de cuadernos (se procesan tras construir la UI).
                let mut lib_open: Option<PathBuf> = None;
                let mut lib_delete: Option<PathBuf> = None;
                let mut lib_create = false;
                let mut lib_go = false;
                let mut page_prev = false;
                let mut page_next = false;
                let mut page_add = false;
                let mut page_lock_toggle = false;
                let in_library = self.app_mode == AppMode::Library;
                let nb_list: Vec<(String, bool, PathBuf)> = if in_library {
                    self.notebooks.iter().map(|n| (n.name.clone(), n.infinite, n.path.clone())).collect()
                } else {
                    Vec::new()
                };
                let ctx = self.egui_ctx.clone();
                #[allow(deprecated)]
                let full_output = ctx.run(raw_input, |ctx| {
                  if self.app_mode == AppMode::Canvas {
                    actions = ui::build_panel(ctx, &mut self.ui, &mut self.brush, &mut self.settings, &mut self.doc, stats);
                    // Conectar las acciones del panel "Mis pinceles" (pincel PS elegido y
                    // gestor de packs) a las variables que se procesan tras construir la UI.
                    if let Some(gi) = actions.pick_ps {
                        ps_select = Some(gi);
                    }
                    if actions.ps_load_all {
                        ps_load_all = true;
                    }
                    if let Some(h) = actions.ps_new_round {
                        ps_new_round = Some(h);
                    }
                    if let Some(idx) = actions.ps_load_pack {
                        ps_load_pack = self.ps_packs.get(idx).map(|(_, p, _)| p.clone());
                    }
                    self.draw_overlays(ctx);
                    // Boton para volver a la biblioteca de cuadernos.
                    egui::Area::new(egui::Id::new("lib_button"))
                        .anchor(egui::Align2::LEFT_TOP, egui::vec2(10.0, 10.0))
                        .show(ctx, |ui| {
                            if ui.button(egui::RichText::new("🏠 Inicio").size(14.0)).clicked() {
                                lib_go = true;
                            }
                        });
                    // Navegacion de paginas (solo en cuadernos de hojas).
                    if !matches!(self.settings.artboard, settings::Artboard::Infinite) {
                        let cur = self.current_page + 1;
                        let total = self.pages.len();
                        egui::Area::new(egui::Id::new("page_nav"))
                            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -14.0))
                            .show(ctx, |ui| {
                                egui::Frame::popup(ui.style()).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui.add_enabled(self.current_page > 0, egui::Button::new("◀")).clicked() {
                                            page_prev = true;
                                        }
                                        ui.label(egui::RichText::new(format!("Hoja {cur} / {total}")).size(14.0));
                                        if ui.add_enabled(self.current_page + 1 < total, egui::Button::new("▶")).clicked() {
                                            page_next = true;
                                        }
                                        ui.separator();
                                        if ui.button("➕ Hoja").clicked() {
                                            page_add = true;
                                        }
                                        let lock_lbl = if self.lock_page { "🔒 Fijada" } else { "🔓 Fijar" };
                                        if ui.selectable_label(self.lock_page, lock_lbl).clicked() {
                                            page_lock_toggle = true;
                                        }
                                    });
                                });
                            });
                    }

                    // (El selector de pinceles de Photoshop se fusiono con el panel "Mis
                    // pinceles": las categorias se eligen ahi al editar un slot de la rueda.)

                    // Panel "Ajustes del pincel": se abre con el boton del lapiz JUSTO en el
                    // puntero, y funciona para CUALQUIER pincel (Photoshop o de la rueda).
                    if self.ui.show_brush_settings {
                        let pos = self.ui.brush_settings_pos;
                        if let Some(s) = self.ps_settings.as_mut() {
                            brush_settings_panel(ctx, s, &self.settings, pos);
                        } else {
                            brush_settings_panel_proc(ctx, &mut self.brush, &self.settings, pos);
                        }
                    }
                  } else {
                    // ---------------- BIBLIOTECA de cuadernos ----------------
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.add_space(18.0);
                        ui.heading("Mis cuadernos");
                        ui.label(
                            egui::RichText::new("Crea cuadernos infinitos o con hojas. Se guardan solos al volver aquí.")
                                .color(egui::Color32::from_gray(130)),
                        );
                        ui.add_space(12.0);
                        // --- Crear nuevo ---
                        ui.horizontal(|ui| {
                            ui.label("Nombre:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.new_nb_name)
                                    .hint_text("Mi cuaderno")
                                    .desired_width(220.0),
                            );
                            ui.selectable_value(&mut self.new_nb_infinite, true, "Lienzo infinito");
                            ui.selectable_value(&mut self.new_nb_infinite, false, "Cuaderno de hojas");
                            if ui.button(egui::RichText::new("Crear").strong()).clicked() {
                                lib_create = true;
                            }
                        });
                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(8.0);
                        if nb_list.is_empty() {
                            ui.label(
                                egui::RichText::new("Aún no tienes cuadernos. Escribe un nombre y pulsa Crear.")
                                    .italics()
                                    .color(egui::Color32::from_gray(140)),
                            );
                        }
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            for (name, infinite, path) in &nb_list {
                                ui.horizontal(|ui| {
                                    // Icono del TIPO de cuaderno, DIBUJADO (no glifos: la fuente
                                    // no trae varios y saldrian como cuadros).
                                    let (trect, _) = ui.allocate_exact_size(egui::vec2(34.0, 36.0), egui::Sense::hover());
                                    let tp = ui.painter().clone();
                                    let tc = trect.center();
                                    let tcol = egui::Color32::from_gray(150);
                                    if *infinite {
                                        // Lienzo infinito: simbolo de infinito (dos aros).
                                        tp.circle_stroke(tc - egui::vec2(5.5, 0.0), 5.0, egui::Stroke::new(2.0, tcol));
                                        tp.circle_stroke(tc + egui::vec2(5.5, 0.0), 5.0, egui::Stroke::new(2.0, tcol));
                                    } else {
                                        // Cuaderno de hojas: dos paginas apiladas.
                                        let r1 = egui::Rect::from_min_size(tc + egui::vec2(-8.0, -7.0), egui::vec2(13.0, 16.0));
                                        let r2 = egui::Rect::from_min_size(tc + egui::vec2(-3.0, -10.0), egui::vec2(13.0, 16.0));
                                        tp.rect_stroke(r1, egui::CornerRadius::same(2), egui::Stroke::new(1.6, tcol), egui::StrokeKind::Inside);
                                        tp.rect_filled(r2, egui::CornerRadius::same(2), egui::Color32::from_gray(248));
                                        tp.rect_stroke(r2, egui::CornerRadius::same(2), egui::Stroke::new(1.6, tcol), egui::StrokeKind::Inside);
                                    }
                                    // Boton con el nombre del cuaderno.
                                    if ui
                                        .add(
                                            egui::Button::new(egui::RichText::new(name).size(16.0))
                                                .min_size(egui::vec2(320.0, 36.0)),
                                        )
                                        .clicked()
                                    {
                                        lib_open = Some(path.clone());
                                    }
                                    // Boton BORRAR con icono de papelera dibujado.
                                    let (drect, dresp) =
                                        ui.allocate_exact_size(egui::vec2(40.0, 36.0), egui::Sense::click());
                                    let hovered = dresp.hovered();
                                    let dp = ui.painter().clone();
                                    let bg = if hovered { egui::Color32::from_gray(224) } else { egui::Color32::from_gray(238) };
                                    dp.rect_filled(drect, egui::CornerRadius::same(6), bg);
                                    let dc = drect.center();
                                    let dcol = if hovered { egui::Color32::from_rgb(196, 64, 64) } else { egui::Color32::from_gray(110) };
                                    let st = egui::Stroke::new(1.7, dcol);
                                    let body = egui::Rect::from_min_max(dc + egui::vec2(-6.0, -2.0), dc + egui::vec2(6.0, 9.0));
                                    dp.rect_stroke(body, egui::CornerRadius::same(1), st, egui::StrokeKind::Inside);
                                    dp.line_segment([dc + egui::vec2(-8.0, -2.0), dc + egui::vec2(8.0, -2.0)], st); // borde de la tapa
                                    dp.line_segment([dc + egui::vec2(-3.0, -2.0), dc + egui::vec2(-3.0, -5.0)], st); // asa izquierda
                                    dp.line_segment([dc + egui::vec2(-3.0, -5.0), dc + egui::vec2(3.0, -5.0)], st); // asa arriba
                                    dp.line_segment([dc + egui::vec2(3.0, -5.0), dc + egui::vec2(3.0, -2.0)], st); // asa derecha
                                    dp.line_segment([dc + egui::vec2(-2.0, 1.0), dc + egui::vec2(-2.0, 6.0)], st); // ranura izq
                                    dp.line_segment([dc + egui::vec2(2.0, 1.0), dc + egui::vec2(2.0, 6.0)], st); // ranura der
                                    if dresp.clicked() {
                                        lib_delete = Some(path.clone());
                                    }
                                });
                                ui.add_space(5.0);
                            }
                        });
                    });
                  }
                });
                if let Some(s) = self.egui_state.as_mut() {
                    s.handle_platform_output(window.as_ref(), full_output.platform_output);
                }
                // --- Acciones de la biblioteca de cuadernos ---
                if lib_go {
                    self.go_to_library();
                }
                if let Some(p) = lib_open {
                    self.open_notebook(p);
                }
                if lib_create {
                    let name = self.new_nb_name.clone();
                    self.new_notebook(&name, self.new_nb_infinite);
                    self.new_nb_name.clear();
                }
                if let Some(p) = lib_delete {
                    notebook::delete(&p);
                    self.notebooks = notebook::list();
                }
                // Navegacion de paginas.
                if page_prev && self.current_page > 0 {
                    self.switch_page(self.current_page - 1);
                }
                if page_next && self.current_page + 1 < self.pages.len() {
                    self.switch_page(self.current_page + 1);
                }
                if page_add {
                    self.add_page();
                }
                if page_lock_toggle {
                    self.lock_page = !self.lock_page;
                    if self.lock_page {
                        self.center_on_page();
                    }
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
                    // Pincel de Photoshop elegido en "Mis pinceles": se asigna al slot en
                    // edicion (ya hecho en ui.rs) y se activa para PINTAR.
                    self.select_ps_brush(i);
                }
                // Forma de la goma (elegida en "Mis pinceles").
                if let Some(tip) = actions.pick_eraser_tip {
                    self.eraser_tip = Some(tip);
                    self.ensure_tip(tip);
                }
                if actions.eraser_round {
                    self.eraser_tip = None;
                }
                // Tocar un slot de la rueda: activar su pincel PS, o salir del modo PS.
                if let Some(i) = actions.activate_ps {
                    self.activate_ps_brush(i);
                } else if actions.exit_ps {
                    self.save_ps_settings();
                    self.ps_settings = None;
                }

                // Propagar el tamano/opacidad entre la rueda y el pincel PS.
                let ps_activated = ps_select.is_some() || actions.activate_ps.is_some() || ps_new_round.is_some();
                if ps_activated {
                    // Pincel recien activado: su tamano/opacidad mandan en la rueda.
                    if let Some(s) = self.ps_settings.as_ref() {
                        self.brush.width = s.size;
                        self.brush.opacity = s.opacity;
                    }
                } else if let (Some((prev_sz, prev_op)), Some(s)) = (ps_sync_prev, self.ps_settings.as_mut()) {
                    // El que cambio (rueda o ajustes) gana y se copia al otro.
                    if (self.brush.width - prev_sz).abs() > 1e-4 {
                        s.size = self.brush.width;
                    } else {
                        self.brush.width = s.size;
                    }
                    if (self.brush.opacity - prev_op).abs() > 1e-4 {
                        s.opacity = self.brush.opacity;
                    } else {
                        self.brush.opacity = s.opacity;
                    }
                }

                // La GOMA es un slot de la rueda: el modo borrador esta activo si el slot
                // seleccionado es la goma. Cambiar a cualquier otro slot la desactiva sola.
                self.eraser_mode = matches!(self.ui.slots.get(self.ui.selected_seg), Some(ui::SlotItem::Eraser));

                // Persistencia por item: al RESELECCIONAR un slot (pincel/herramienta/goma),
                // restaurar su ultima configuracion guardada (tamano/opacidad/suavidad).
                if let Some(seg) = actions.slot_selected {
                    // Cambiar de herramienta/pincel cancela un lazo poligonal a medias.
                    if self.ui.active_tool() != Some(Tool::PolyLasso) {
                        self.cancel_poly_lasso();
                    }
                    if let Some(slot) = self.ui.slots.get(seg).copied() {
                        if let Some(key) = slot_key(slot) {
                            if let Some(c) = self.item_cfg.get(&key).copied() {
                                self.brush.width = c.width;
                                self.brush.opacity = c.opacity;
                                self.brush.smoothing = c.smoothing;
                            }
                        }
                    }
                }
                // Guardar la configuracion del slot activo (captura los cambios de los popups).
                if let Some(slot) = self.ui.slots.get(self.ui.selected_seg).copied() {
                    if let Some(key) = slot_key(slot) {
                        self.item_cfg.insert(
                            key,
                            ItemCfg { width: self.brush.width, opacity: self.brush.opacity, smoothing: self.brush.smoothing },
                        );
                    }
                }

                // Aplicar acciones del panel.
                if actions.undo {
                    self.undo_op();
                }
                if actions.redo {
                    self.redo_op();
                }
                if actions.clear {
                    self.doc.clear();
                    let tips: Vec<u32> = self.ps_committed.keys().copied().collect();
                    self.ps_committed.clear();
                    if let Some(g) = self.gpu.as_mut() {
                        for t in tips {
                            g.set_committed_stamps(t, &[]);
                        }
                        g.clear_mask(); // quitar todos los borrados de la goma
                    }
                    self.erase_strokes.clear();
                    self.erase_redo.clear();
                    self.cur_erase.clear();
                    self.tick = 1.0;
                    self.undo_stack.clear();
                    self.redo_stack.clear();
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
                let infinite_canvas = matches!(self.settings.artboard, settings::Artboard::Infinite);
                // Cuaderno de HOJAS: dibujar la PAGINA (con el color/papel elegido en Ajustes)
                // detras de todo. Antes estaba fija a blanco; ahora respeta el fondo del lienzo.
                if let Some((w, h)) = self.settings.artboard_size() {
                    let (hw, hh) = (w * 0.5, h * 0.5);
                    let paper = self.settings.bg_color();
                    let v = |x: f32, y: f32| Vertex { pos: [x, y], color: paper, time: 0.0 };
                    self.grid_mesh.extend_from_slice(&[
                        v(-hw, -hh), v(hw, -hh), v(hw, hh),
                        v(-hw, -hh), v(hw, hh), v(-hw, hh),
                    ]);
                }
                let grid_limit = if self.settings.grid_limit_artboard || !infinite_canvas {
                    // En hojas, la rejilla se limita a la pagina.
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
                // En hojas, el fondo es una "mesa" gris para que la pagina resalte.
                let bg = if infinite_canvas {
                    self.settings.bg_color()
                } else {
                    [0.20, 0.21, 0.24, 1.0]
                };

                // Recorte del contenido a la HOJA (cuadernos de hojas): el dibujo y la
                // rejilla no se salen del rectangulo de la pagina.
                let content_clip = if infinite_canvas {
                    None
                } else if let Some((w, h)) = self.settings.artboard_size() {
                    let (hw, hh) = (w * 0.5, h * 0.5);
                    let a = self.camera.world_to_screen(vec2(-hw, -hh));
                    let b = self.camera.world_to_screen(vec2(hw, hh));
                    let vp = self.camera.viewport;
                    let x0 = a.x.min(b.x).max(0.0);
                    let y0 = a.y.min(b.y).max(0.0);
                    let x1 = a.x.max(b.x).min(vp.x);
                    let y1 = a.y.max(b.y).min(vp.y);
                    Some((x0 as u32, y0 as u32, (x1 - x0).max(0.0) as u32, (y1 - y0).max(0.0) as u32))
                } else {
                    None
                };

                // --- Render (lienzo + UI encima) ---
                if let Some(g) = self.gpu.as_mut() {
                    let screen = egui_wgpu::ScreenDescriptor {
                        size_in_pixels: [g.width(), g.height()],
                        pixels_per_point: ppp,
                    };
                    g.set_bg(bg);
                    g.set_content_clip(content_clip);
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
/// Radio de la "cuerda" (en PIXELES de pantalla) del Suavizado estilo Photoshop, segun la
/// suavidad (0..1). 0% = 0 px (trazo crudo, pegado a la punta); 100% = cuerda larga (linea
/// muy suave). El punto dibujado persigue al cursor a esta distancia -> lag FIJO y pequeno
/// (no crece con la velocidad como un filtro paso-bajo), igual que el Suavizado de Photoshop.
fn smoothing_string_radius_px(smoothing: f32) -> f32 {
    smoothing.clamp(0.0, 1.0) * 48.0
}

fn pct_row(ui: &mut egui::Ui, label: &str, v: &mut f32) {
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(v, 0.0..=1.0).custom_formatter(|x, _| format!("{:.0}%", x * 100.0)).custom_parser(|s| s.trim_end_matches('%').parse::<f64>().ok().map(|x| x / 100.0)));
        ui.label(label);
    });
}

/// Panel "Ajustes del pincel" estilo Photoshop: edita `s` en vivo (el motor de
/// estampado lo aplica al siguiente trazo). `cfg` da la unidad de medida (misma que
/// la rueda). Devuelve nada; muta `s`.
/// Panel completo de ajustes para un pincel de Photoshop (estampado). Se abre en `pos`
/// (el puntero) y se mantiene dentro de la pantalla.
fn brush_settings_panel(ctx: &egui::Context, s: &mut ink_core::BrushSettings, cfg: &Settings, pos: egui::Pos2) {
    use egui::{CollapsingHeader, RichText, Slider};
    egui::Window::new(RichText::new("Ajustes del pincel").strong())
        .id(egui::Id::new("ps_settings_window"))
        .fixed_pos(pos)
        .constrain(true)
        .default_width(280.0)
        .resizable(false)
        .collapsible(true)
        .default_open(true)
        .show(ctx, |ui| {
            ui.label(RichText::new(&s.name).italics().color(egui::Color32::from_rgb(40, 120, 220)));
            egui::ScrollArea::vertical().max_height(560.0).auto_shrink([false, false]).show(ui, |ui| {
                // ---- Forma de la punta del pincel ----
                CollapsingHeader::new("Forma de la punta").default_open(true).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Misma unidad de medida que la rueda (pts/px/mm... segun ajustes).
                        ui.add(Slider::new(&mut s.size, 1.0..=400.0).custom_formatter(|v, _| cfg.format_measure(v as f32)));
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

/// Panel de ajustes para un pincel PROCEDURAL de la rueda (Pluma, Rotulador, Lapiz...).
/// Muestra los controles que ESE motor usa: tamano, opacidad y suavidad (las mismas
/// magnitudes que la rueda). Se abre en `pos` (el puntero).
fn brush_settings_panel_proc(ctx: &egui::Context, brush: &mut ink_core::Brush, cfg: &Settings, pos: egui::Pos2) {
    use egui::{RichText, Slider};
    egui::Window::new(RichText::new("Ajustes del pincel").strong())
        .id(egui::Id::new("proc_settings_window"))
        .fixed_pos(pos)
        .constrain(true)
        .default_width(260.0)
        .resizable(false)
        .collapsible(true)
        .default_open(true)
        .show(ctx, |ui| {
            ui.label(RichText::new("Pincel de la rueda").italics().color(egui::Color32::from_rgb(40, 120, 220)));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // Misma unidad de medida que la rueda (px/pts/mm... segun ajustes).
                ui.add(Slider::new(&mut brush.width, 1.0..=400.0).custom_formatter(|v, _| cfg.format_measure(v as f32)));
                ui.label("Tamaño");
            });
            pct_row(ui, "Opacidad", &mut brush.opacity);
            pct_row(ui, "Suavidad", &mut brush.smoothing);
        });
}

/// Clasifica un pincel en una de las 4 categorias de Photoshop por su nombre.
fn classify_brush(name: &str) -> &'static str {
    let n = name.to_lowercase();
    let has = |kws: &[&str]| kws.iter().any(|k| n.contains(k));
    if has(&["pencil", "charcoal", "pastel", "drawing", "eraser", "crayon", "lápiz", "lapiz", "carboncillo", "pastel", "borrador"]) {
        "Pinceles secos"
    } else if has(&["ink", "oil", "paint", "blender", "wet", "watercolor", "gouache", "tinta", "óleo", "oleo", "acuarela", "húmedo", "humedo", "entintado", "mezclador"]) {
        "Pinceles húmedos"
    } else if has(&["spatter", "concept", "foliage", "texture", "scatter", "grunge", "splat", "salpicadura", "concepto", "trama", "efecto"]) {
        "Pinceles de efectos especiales"
    } else {
        "Pinceles generales"
    }
}

/// Genera una punta REDONDA procedural (mascara alfa con falloff por dureza).
fn generate_round_brush(hardness: f32, name: &str, idx: usize) -> ink_brush::SampledBrush {
    let size = 128usize;
    let r = size as f32 / 2.0;
    let mut alpha = vec![0u8; size * size];
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - r + 0.5;
            let dy = y as f32 - r + 0.5;
            let d = (dx * dx + dy * dy).sqrt() / r;
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
    ink_brush::SampledBrush { id: format!("round-{idx}"), name: Some(name.to_string()), width: size as u32, height: size as u32, alpha }
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
            // Blanca: se tinta segun el fondo (oscuro en el selector, claro en la rueda).
            rgba[idx] = 255;
            rgba[idx + 1] = 255;
            rgba[idx + 2] = 255;
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
