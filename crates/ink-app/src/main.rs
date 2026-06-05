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
    Document, InkConfig, InkSmoother, InputSample, StampVertex, Stroke, TextItem, TipKind, Tool,
    Vec2, Vertex,
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

/// Un trazo precalculado para la vista previa: (puntos en mundo, color RGBA, ancho de pincel).
type PreviewStroke = (Vec<Vec2>, [f32; 4], f32);
/// Una pagina precalculada para la vista previa: (sus trazos, limites min/max en mundo).
type PreviewPage = (Vec<PreviewStroke>, Option<(Vec2, Vec2)>);

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
use std::path::{Path, PathBuf};
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
    /// Alt presionado (para Alt + rueda = zoom en los cuadernos de hojas).
    alt_down: bool,
    /// Zoom por ARRASTRE (Alt + deslizar el lapiz/raton): activo mientras se arrastra.
    zooming: bool,
    /// Punto (pantalla) sobre el que se centra el zoom por arrastre.
    zoom_anchor: Vec2,
    last_move_time: Instant,
    /// Instante del ultimo evento tactil/lapiz. Windows genera ademas eventos de
    /// raton SINTETICOS a partir del tacto; si llegan justo despues de un Touch los
    /// ignoramos para no dibujar el trazo DOS veces (los "garabatos extra").
    last_touch: Option<Instant>,

    // Trazo activo
    /// Suavizador de entrada (One-Euro + presion fluida + densificado por spline + taper):
    /// convierte los eventos crudos en muestras densas y suaves. Da el trazo "de calidad
    /// extrema" sin sacrificar latencia. Compartido por el camino normal y el de Photoshop.
    smoother: InkSmoother,
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

    // --- Modo DOCUMENTO (escritura con teclado en la hoja) ---
    /// Texto (Markdown) de la hoja actual. Se guarda en `PageData.body`.
    page_body: String,
    /// Alineacion del texto de la hoja actual (0 izq, 1 centro, 2 der, 3 justif). En `PageData`.
    page_align: u32,
    /// Diseño de pagina del cuaderno (margenes, interlineado, fuente...). En `NotebookData`.
    doc_layout: notebook::DocLayout,
    /// `true` = modo escritura activo: el teclado escribe en la hoja (no atajos de dibujo).
    write_mode: bool,
    /// Panel de "Diseño de pagina" abierto.
    show_page_setup: bool,
    /// Historial del documento (deshacer/rehacer del texto), con snapshots del `page_body`.
    doc_undo: Vec<String>,
    doc_redo: Vec<String>,
    doc_snap: String,
    doc_snap_at: f32,
    /// Modo pantalla completa (enfoque de escritura).
    fullscreen: bool,
    /// Texturas de las imagenes del documento, cacheadas por ruta.
    doc_img_cache: std::collections::HashMap<String, egui::TextureHandle>,
    /// Celda de tabla en edicion: (char inicio del bloque, fila, columna) + su texto.
    editing_cell: Option<(usize, usize, usize)>,
    cell_buf: String,
    /// Pedir foco a la celda en edicion SOLO el primer frame (si no, queda atrapada).
    cell_focus: bool,
    /// Desplazamiento horizontal de la tabla ancha (cuando excede el ancho de la hoja).
    table_hscroll: f32,

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
    /// Acabado/diseño del cuaderno abierto (se conserva al guardar).
    current_finish: u32,
    /// Capas (fx), intensidad y acento del cuaderno abierto (se conservan al guardar).
    current_fx: u32,
    current_intensity: f32,
    current_accent: u32,
    /// Forma/grosor/ceja del cuaderno abierto (se conservan al guardar).
    current_shape: u32,
    current_thickness: f32,
    current_overhang: f32,
    /// Textura del material del cuaderno abierto.
    current_texture: u32,
    /// Color de todo el cuaderno abierto (para finish 800 solido / 801 degradado).
    current_cover_a: [f32; 3],
    current_cover_b: [f32; 3],
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
    /// Diseño base elegido para el cuaderno nuevo (foil 0..11 o cargador >= 100).
    new_nb_finish: u32,
    /// Capas (fx), intensidad y acento elegidos para el cuaderno nuevo.
    new_nb_fx: u32,
    new_nb_intensity: f32,
    new_nb_accent: u32,
    /// Forma/grosor/ceja elegidos para el cuaderno nuevo.
    new_nb_shape: u32,
    new_nb_thickness: f32,
    new_nb_overhang: f32,
    /// Textura del material elegida para el cuaderno nuevo.
    new_nb_texture: u32,
    /// Color (COPIC) elegido para el cuaderno nuevo (finish 800 solido / 801 degradado).
    new_nb_cover_a: [f32; 3],
    new_nb_cover_b: [f32; 3],
    /// Ajustes GLOBALES (panel Tweaks): pose en el estante e interaccion.
    lib_tweaks: notebook::LibTweaks,
    /// Panel de Tweaks (ajustes globales) abierto.
    show_tweaks: bool,
    /// Panel de creación abierto (vista previa + opciones de carátula).
    creating_nb: bool,
    /// Si se está EDITANDO la carátula de un cuaderno ya creado, su ruta (None = crear nuevo).
    editing_nb: Option<PathBuf>,
    /// Reloj de animación (segundos acumulados) para las cartas.
    clock: f32,
    // --- Cartas hologr aficas de la biblioteca (Home) ---
    /// Animacion por carta: [hover 0..1, rotX, rotY] (suavizado hacia el objetivo).
    card_anim: Vec<[f32; 3]>,
    /// Rectangulos (cx, cy, hx, hy en px) de las cartas del ultimo layout (para hit-test).
    card_rects: Vec<(f32, f32, f32, f32)>,
    /// Desplazamiento vertical de la cuadricula de cartas (rueda).
    card_scroll: f32,
    /// Angulo de volteo (rad, 0=portada, PI=reverso) de cada cuaderno. Se mueve con INERCIA
    /// (gravedad cero): la rueda le da impulso y luego flota hasta frenarse por rozamiento.
    card_flip: Vec<f32>,
    /// Velocidad angular del volteo de cada cuaderno (para la inercia).
    card_flip_vel: Vec<f32>,
    /// Arrastre de cartas en la biblioteca: indice agarrado, punto inicial y si ya se arrastra.
    drag_idx: Option<usize>,
    drag_start: Vec2,
    dragging: bool,
    /// Renombrado en linea: indice del cuaderno cuyo nombre se edita (clic en el nombre).
    renaming: Option<usize>,
    rename_buf: String,
    rename_grace: i32,
    /// Al soltar una HOJA (nota rapida) sobre un cuaderno: menu (origen, destino) para elegir
    /// entre cambiar de posicion o guardar la nota dentro de ese cuaderno.
    merge_prompt: Option<(usize, usize)>,
    /// VISTA PREVIA (Alt+clic): cuaderno en vista previa, progreso de apertura (0..1), nombre, el
    /// contenido de TODAS sus paginas (para pasarlas), la pagina mostrada y el rect de origen
    /// (la carta en la rejilla) para la animacion de "acercarse".
    preview_idx: Option<usize>,
    preview_t: f32,
    preview_name: String,
    preview_pages: Vec<PreviewPage>,
    preview_page: usize,
    preview_from: (Vec2, Vec2),
    /// Biblioteca v2: archiveros (carpetas), filtro activo, busqueda y orden.
    archiveros: Vec<String>,
    active_archivero: String, // "" = Todos
    current_archivero: String, // archivero del cuaderno abierto (para guardar)
    search_query: String,
    sort_mode: u32, // 0 Recientes, 1 A-Z, 2 Cuadernos, 3 Notas
    new_archivero_buf: String,
    creating_archivero: bool,
    /// Zonas (px fisicos) de los archiveros en la barra lateral, para soltar un cuaderno encima y
    /// asignarlo: (min_x, min_y, max_x, max_y, nombre_archivero). "" = Todos (quitar de archivero).
    archivero_drop: Vec<(f32, f32, f32, f32, String)>,
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
            alt_down: false,
            zooming: false,
            zoom_anchor: Vec2::ZERO,
            last_move_time: now,
            last_touch: None,
            smoother: InkSmoother::new(InkConfig::default()),
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
            page_body: String::new(),
            page_align: 0,
            doc_layout: notebook::DocLayout::default(),
            write_mode: false,
            show_page_setup: false,
            doc_undo: Vec::new(),
            doc_redo: Vec::new(),
            doc_snap: String::new(),
            doc_snap_at: 0.0,
            fullscreen: false,
            doc_img_cache: std::collections::HashMap::new(),
            editing_cell: None,
            cell_buf: String::new(),
            cell_focus: false,
            table_hscroll: 0.0,
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
            current_finish: 1,
            current_fx: 0,
            current_intensity: 1.0,
            current_accent: 0,
            current_shape: 0,
            current_thickness: 1.0,
            current_overhang: 1.0,
            current_texture: 0,
            current_cover_a: [1.0, 1.0, 1.0],
            current_cover_b: [1.0, 1.0, 1.0],
            pages: vec![notebook::PageData::empty()],
            current_page: 0,
            lock_page: false,
            wheel_accum: 0.0,
            new_nb_name: String::new(),
            new_nb_infinite: true,
            new_nb_finish: 1,
            new_nb_fx: 0,
            new_nb_intensity: 1.0,
            new_nb_accent: 0,
            new_nb_shape: 0,
            new_nb_thickness: 1.0,
            new_nb_overhang: 1.0,
            new_nb_texture: 0,
            new_nb_cover_a: [1.0, 1.0, 1.0],
            new_nb_cover_b: [1.0, 1.0, 1.0],
            lib_tweaks: notebook::load_tweaks(),
            show_tweaks: false,
            creating_nb: false,
            editing_nb: None,
            clock: 0.0,
            card_anim: Vec::new(),
            card_rects: Vec::new(),
            card_scroll: 0.0,
            card_flip: Vec::new(),
            card_flip_vel: Vec::new(),
            drag_idx: None,
            drag_start: Vec2::ZERO,
            dragging: false,
            renaming: None,
            rename_buf: String::new(),
            rename_grace: 0,
            merge_prompt: None,
            preview_idx: None,
            preview_t: 0.0,
            preview_name: String::new(),
            preview_pages: Vec::new(),
            preview_page: 0,
            preview_from: (Vec2::ZERO, Vec2::ZERO),
            archiveros: notebook::load_archiveros(),
            active_archivero: String::new(),
            current_archivero: String::new(),
            search_query: String::new(),
            sort_mode: 0,
            new_archivero_buf: String::new(),
            creating_archivero: false,
            archivero_drop: Vec::new(),
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
        // Suavizador de entrada (One-Euro + presion fluida + spline + taper). Sustituye al
        // viejo suavizado "cuerda": curvas de calidad extrema sin sacrificar latencia.
        self.last_sample_time = Instant::now();
        let world = self.camera.screen_to_world(self.cursor);
        let cfg = self.ink_config();
        self.smoother.reset(cfg);
        // Opacidad -> alfa del color del trazo.
        let mut b = self.brush;
        b.color[3] = self.brush.opacity.clamp(0.0, 1.0);
        let mut s = Stroke::new(b);
        for sm in self.smoother.push(world, initial_pressure.clamp(0.05, 1.0), 1.0 / 120.0) {
            self.last_sample_pos = sm.pos;
            s.push(sm);
        }
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

    /// Ajustes del suavizador de entrada para el pincel y zoom actuales. Las longitudes van en
    /// MUNDO (escaladas por el zoom para sentirse constantes en pantalla). El taper solo se
    /// aplica a la pluma (ancho por presion).
    fn ink_config(&self) -> InkConfig {
        let zoom = self.camera.zoom.max(1e-4);
        let is_pen = matches!(self.brush.kind, ink_core::BrushKind::Pen);
        InkConfig {
            // Densificado fino: ~1.3 px de pantalla entre muestras -> curvas sin facetas.
            max_step: (1.3 / zoom).max(0.05),
            // Afilado de entrada ~1.6 anchos de pincel (solo pluma).
            taper_in: if is_pen { (self.brush.width * 1.6).max(1.5 / zoom) } else { 0.0 },
            pressure_smooth: 0.6,
            smoothing: self.brush.smoothing,
            use_pressure: is_pen,
        }
    }

    /// Afilado de SALIDA: rampa (smoothstep) la presion de las ultimas unidades del trazo para
    /// que la pluma termine en punta. Se aplica al cerrar (en vivo no se conoce el fin), asi el
    /// trazo "se asienta" suavemente al levantar el lapiz, como en Concepts.
    fn apply_exit_taper(&self, stroke: &mut Stroke) {
        if !matches!(stroke.brush.kind, ink_core::BrushKind::Pen) {
            return;
        }
        let taper = (stroke.brush.width * 1.6).max(1.5 / self.camera.zoom.max(1e-4));
        if taper <= 1e-4 {
            return;
        }
        let n = stroke.samples.len();
        let mut dist = 0.0_f32;
        for i in (0..n).rev() {
            if i + 1 < n {
                dist += (stroke.samples[i + 1].pos - stroke.samples[i].pos).length();
            }
            if dist >= taper {
                break;
            }
            let t = (dist / taper).clamp(0.0, 1.0);
            let f = t * t * (3.0 - 2.0 * t); // smoothstep: 0 en el extremo final, 1 a `taper`
            stroke.samples[i].pressure *= f;
        }
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
        let dt = (now - self.last_sample_time).as_secs_f32().clamp(1e-4, 0.1);
        self.last_sample_time = now;
        // El suavizador emite muestras DENSAS sobre una curva suave (puede ser 0..N por evento).
        // Cada una se tesela de forma incremental (mantiene el invariante vivo == final).
        let new = self.smoother.push(raw_world, pressure, dt);
        for sm in new {
            match self.active.as_mut() {
                Some(stroke) => stroke.push(sm),
                None => break,
            }
            self.last_sample_pos = sm.pos;
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
        // Cerrar la cola del suavizado: emite el ultimo tramo pendiente (~1 evento).
        for sm in self.smoother.finish() {
            match self.active.as_mut() {
                Some(s) => s.push(sm),
                None => break,
            }
            self.last_sample_pos = sm.pos;
        }
        if let Some(mut stroke) = self.active.take() {
            if !stroke.samples.is_empty() {
                // Afilado de salida (pluma): el trazo termina en punta al levantar.
                self.apply_exit_taper(&mut stroke);
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
        let Some((doc, texts, erase, tick, body, align)) = self
            .pages
            .get(i)
            .map(|pg| (pg.doc.clone(), pg.texts.clone(), pg.erase_strokes.clone(), pg.tick.max(1.0), pg.body.clone(), pg.align))
        else {
            return;
        };
        self.page_body = body;
        self.page_align = align;
        self.doc_undo.clear();
        self.doc_redo.clear();
        self.doc_snap = self.page_body.clone();
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
        let body = self.page_body.clone();
        let align = self.page_align;
        if let Some(pg) = self.pages.get_mut(self.current_page) {
            pg.doc = doc;
            pg.texts = texts;
            pg.erase_strokes = erase;
            pg.tick = tick;
            pg.body = body;
            pg.align = align;
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
        self.current_finish = nb.finish;
        self.current_fx = nb.fx;
        self.current_intensity = nb.fx_intensity;
        self.current_accent = nb.accent;
        self.current_shape = nb.shape;
        self.current_thickness = nb.thickness;
        self.current_overhang = nb.overhang;
        self.current_texture = nb.texture;
        self.current_cover_a = nb.cover_a;
        self.current_cover_b = nb.cover_b;
        self.current_archivero = nb.archivero.clone();
        self.doc_layout = nb.doc_layout;
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
        // Si es una nota rapida (hoja, finish 500..599), su carátula sigue el papel/cuadricula
        // ACTUAL: si cambiaste la cuadricula dentro de la nota, la carátula cambia al volver.
        if self.current_finish >= 500 && self.current_finish < 600 {
            self.current_finish = sheet_finish_for_grid(self.settings.grid);
        }
        let mut nb = notebook::NotebookData::new(&name, infinite, self.current_finish);
        nb.fx = self.current_fx;
        nb.fx_intensity = self.current_intensity;
        nb.accent = self.current_accent;
        nb.shape = self.current_shape;
        nb.thickness = self.current_thickness;
        nb.overhang = self.current_overhang;
        nb.texture = self.current_texture;
        nb.cover_a = self.current_cover_a;
        nb.cover_b = self.current_cover_b;
        nb.archivero = self.current_archivero.clone();
        nb.doc_layout = self.doc_layout;
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

    /// Crea un cuaderno nuevo (con su carátula: diseño base + capas + forma), lo guarda y lo abre.
    fn new_notebook(&mut self, name: &str, infinite: bool, finish: u32, fx: u32, intensity: f32, accent: u32) {
        let name = if name.trim().is_empty() { "Cuaderno" } else { name.trim() };
        let mut nb = notebook::NotebookData::new(name, infinite, finish);
        nb.fx = fx;
        nb.fx_intensity = intensity;
        nb.accent = accent;
        nb.shape = self.new_nb_shape;
        nb.thickness = self.new_nb_thickness;
        nb.overhang = self.new_nb_overhang;
        nb.texture = self.new_nb_texture;
        nb.cover_a = self.new_nb_cover_a;
        nb.cover_b = self.new_nb_cover_b;
        nb.archivero = self.active_archivero.clone(); // entra al archivero activo
        let path = notebook::path_for(name);
        let _ = notebook::save(&nb, &path);
        self.apply_notebook(nb);
        self.current_path = Some(path);
        self.app_mode = AppMode::Canvas;
    }

    /// Crea una NOTA RAPIDA: una sola hoja, con nombre = fecha y hora de creación. Su carátula
    /// no es un cuaderno 3D sino una HOJA (rayas/cuadrícula/puntos, segun el papel actual). Se
    /// abre al momento para escribir. En la biblioteca, su `finish` >= 500 la dibuja como hoja.
    fn new_quick_note(&mut self) {
        let name = quick_note_name();
        // La carátula refleja el papel REAL de la nota (la cuadrícula activa).
        let finish = sheet_finish_for_grid(self.settings.grid);
        let mut nb = notebook::NotebookData::new(&name, false, finish);
        nb.fx = 0;
        nb.fx_intensity = 1.0;
        nb.accent = 0;
        nb.shape = 0;
        nb.thickness = 1.0;
        nb.overhang = 1.0;
        nb.archivero = self.active_archivero.clone(); // entra al archivero activo
        let path = notebook::path_for(&name);
        let _ = notebook::save(&nb, &path);
        self.apply_notebook(nb);
        self.current_path = Some(path);
        self.app_mode = AppMode::Canvas;
    }

    /// Indices (en self.notebooks) de los cuadernos VISIBLES segun archivero + busqueda + orden.
    fn visible_notebooks(&self) -> Vec<usize> {
        let q = self.search_query.trim().to_lowercase();
        let mut v: Vec<usize> = (0..self.notebooks.len())
            .filter(|&i| {
                let nb = &self.notebooks[i];
                let arch_ok = self.active_archivero.is_empty() || nb.archivero == self.active_archivero;
                let is_note = nb.finish >= 500 && nb.finish < 600;
                let kind_ok = match self.sort_mode {
                    2 => !is_note, // Cuadernos
                    3 => is_note,  // Notas
                    _ => true,
                };
                let search_ok = q.is_empty() || nb.name.to_lowercase().contains(&q);
                arch_ok && kind_ok && search_ok
            })
            .collect();
        if self.sort_mode == 1 {
            // A - Z
            v.sort_by(|&a, &b| self.notebooks[a].name.to_lowercase().cmp(&self.notebooks[b].name.to_lowercase()));
        }
        v
    }

    /// Numero de cuadernos en un archivero ("" = todos).
    fn archivero_count(&self, arch: &str) -> usize {
        if arch.is_empty() {
            self.notebooks.len()
        } else {
            self.notebooks.iter().filter(|n| n.archivero == arch).count()
        }
    }

    /// Guarda el cuaderno actual y vuelve a la biblioteca (refrescando la lista).
    fn go_to_library(&mut self) {
        self.save_current();
        self.current_path = None;
        self.app_mode = AppMode::Library;
        self.notebooks = notebook::list();
        self.card_scroll = 0.0;
        self.card_anim.clear();
        self.card_flip.clear();
        self.card_flip_vel.clear();
        self.creating_nb = false;
        self.editing_nb = None;
        self.drag_idx = None;
        self.dragging = false;
        self.renaming = None;
        self.preview_idx = None;
        self.preview_t = 0.0;
        // Limpiar la tinta del cuaderno que se cerro para que NO se vea en el Home.
        if let Some(g) = self.gpu.as_mut() {
            g.clear_ink();
        }
    }

    /// Aplica el color de la barra de titulo del SO segun el tema actual (Windows 11, DWM).
    #[allow(unused_variables)]
    fn apply_titlebar(&self) {
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Some(w) = &self.window {
                if let Ok(h) = w.window_handle() {
                    if let RawWindowHandle::Win32(wh) = h.as_raw() {
                        let cap = if self.lib_tweaks.theme == 1 { 0x0013_181C } else { 0x0014_0F0E };
                        set_dark_titlebar(wh.hwnd.get(), cap);
                    }
                }
            }
        }
    }

    // ===================== Cartas hologr aficas de la biblioteca (Home) =====================

    /// Ancho (px fisicos) reservado por la barra lateral izquierda.
    fn library_sidebar_px(&self) -> f32 {
        self.egui_ctx.pixels_per_point().max(0.5) * 340.0
    }

    /// Linea (px fisicos) BAJO la barra superior del area derecha (titulo de seccion + buscador +
    /// pestañas): donde se recortan las cartas al desplazar (no tapan esa barra).
    fn library_header_px(&self) -> f32 {
        self.egui_ctx.pixels_per_point().max(0.5) * 188.0
    }

    /// Borde superior (px fisicos) donde empieza la primera fila de cartas (con margen por la
    /// inclinacion 3D que sube la esquina de la carta).
    fn library_top(&self) -> f32 {
        self.library_header_px() + 46.0
    }

    /// Columnas que caben en el area de la cuadricula (a la derecha de la barra lateral).
    fn library_cols(&self) -> usize {
        let vp = self.camera.viewport;
        let (cw, gap) = (188.0_f32, 36.0_f32);
        let left = self.library_sidebar_px() + 24.0;
        let reserve_r = if self.show_tweaks { 300.0 * self.egui_ctx.pixels_per_point().max(0.5) } else { 0.0 };
        let usable = (vp.x - left - reserve_r).max(cw + 20.0);
        (((usable + gap) / (cw + gap)).floor() as usize).max(1)
    }

    /// Cuanto se puede desplazar la cuadricula hacia arriba (px). 0 si todo cabe en pantalla.
    fn library_max_scroll(&self) -> f32 {
        let nvis = self.visible_notebooks().len();
        if nvis == 0 {
            return 0.0;
        }
        let vp = self.camera.viewport;
        let (ch, gap) = (263.0_f32, 36.0_f32);
        let cols = self.library_cols();
        let rows = nvis.div_ceil(cols);
        let row_h = ch + gap + 134.0;
        let lowest = self.library_top() + rows.saturating_sub(1) as f32 * row_h + ch + 100.0;
        // +84: reserva inferior para que el conmutador de tema (abajo-centro) no tape la ultima fila.
        (lowest - vp.y + 84.0).max(0.0)
    }

    /// Layout en cuadricula: posiciones (centro, medio-tamano) por cuaderno. SOLO los visibles
    /// (segun archivero/busqueda/orden) reciben hueco; los ocultos van fuera de pantalla. Asi el
    /// resto del sistema (animacion, arrastre, vista previa) sigue usando el indice del cuaderno.
    fn library_card_layout(&self) -> Vec<(Vec2, Vec2)> {
        let n = self.notebooks.len();
        let vp = self.camera.viewport;
        let (cw, ch, gap) = (188.0_f32, 263.0_f32, 36.0_f32);
        let left = self.library_sidebar_px() + 24.0;
        let reserve_r = if self.show_tweaks { 300.0 * self.egui_ctx.pixels_per_point().max(0.5) } else { 0.0 };
        let usable = (vp.x - left - reserve_r).max(cw + 20.0);
        let cols = self.library_cols();
        let total_w = cols as f32 * cw + cols.saturating_sub(1) as f32 * gap;
        let x0 = left + (usable - total_w) * 0.5 + cw * 0.5;
        let top = self.library_top();
        let row_h = ch + gap + 134.0;
        let mut out = vec![(vec2(-9999.0, -9999.0), vec2(cw * 0.5, ch * 0.5)); n];
        for (slot, &i) in self.visible_notebooks().iter().enumerate() {
            let (col, row) = (slot % cols, slot / cols);
            let cx = x0 + col as f32 * (cw + gap);
            let cy = top + ch * 0.5 + row as f32 * row_h - self.card_scroll;
            out[i] = (vec2(cx, cy), vec2(cw * 0.5, ch * 0.5));
        }
        out
    }

    /// Suaviza la animacion (hover/tilt) de cada carta hacia su objetivo (segun el cursor) y
    /// guarda los rects para el hit-test de los clics.
    fn update_card_anim(&mut self, layout: &[(Vec2, Vec2)], dt: f32) {
        self.card_anim.resize(layout.len(), [0.0, 0.0, 0.0]);
        self.card_flip.resize(layout.len(), 0.0);
        self.card_flip_vel.resize(layout.len(), 0.0);
        let cur = self.cursor;
        // Inclinacion BASE (siempre, para que se note el grosor 3D del cuaderno) + un rango
        // mayor al pasar el cursor (la carta "se mueve mas").
        // Pose en el estante (global, panel Tweaks): inclinacion = mirar desde arriba; giro =
        // girar para ver el lomo. En grados -> radianes.
        let base_rx = self.lib_tweaks.inclinacion.to_radians();
        let base_ry = self.lib_tweaks.giro.to_radians();
        let hover_mode = self.lib_tweaks.hover;
        let max_ang = 0.20; // seguimiento del cursor MUY suave (apenas se ladea hacia el cursor)
        for (i, (c, h)) in layout.iter().enumerate() {
            let inside = (cur.x - c.x).abs() <= h.x && (cur.y - c.y).abs() <= h.y;
            let (tx, ty) = if inside {
                (
                    ((cur.x - (c.x - h.x)) / (2.0 * h.x)).clamp(0.0, 1.0),
                    ((cur.y - (c.y - h.y)) / (2.0 * h.y)).clamp(0.0, 1.0),
                )
            } else {
                (0.5, 0.5)
            };
            let (t_hover, t_rotx, t_roty) = if inside {
                let tilt_x = base_rx + (0.5 - ty) * 2.0 * max_ang;
                let tilt_y = base_ry + (tx - 0.5) * 2.0 * max_ang;
                match hover_mode {
                    3 => (0.5, base_rx + (0.5 - ty) * max_ang, base_ry + (tx - 0.5) * max_ang), // sutil
                    2 => (0.45, base_rx, base_ry + 0.6), // girar (muestra mas el lomo)
                    1 => (1.0, tilt_x, base_ry - 0.45),  // abrir (gira hacia el lector)
                    _ => (1.0, tilt_x, tilt_y),          // levantar
                }
            } else {
                (0.0, base_rx, base_ry)
            };
            let a = &mut self.card_anim[i];
            // Suavizado INDEPENDIENTE DE LOS FPS: la app redibuja en Mailbox (muy rapido, 200+
            // fps), asi que un factor por-fotograma iba rapidisimo. Se calcula desde dt con una
            // "tasa" baja (1.6) -> el hover/tilt llega LENTO y flotante a cualquier tasa de fps.
            let kr = 1.0 - (-1.6 * dt).exp();
            a[0] += (t_hover - a[0]) * kr;
            a[1] += (t_rotx - a[1]) * kr;
            a[2] += (t_roty - a[2]) * kr;
            // Volteo: mientras el cursor ESTA encima, gira con INERCIA (gravedad cero) y se queda
            // donde lo dejes; al SALIR el cursor, vuelve suave a la portada (0).
            let f60 = dt * 60.0; // escala "por-fotograma@60fps" -> tiempo real (independiente de fps)
            if inside {
                let mut nf = self.card_flip[i] + self.card_flip_vel[i] * f60;
                self.card_flip_vel[i] *= (-3.7 * dt).exp(); // rozamiento suave (independiente de fps)
                if nf < 0.0 {
                    nf = 0.0;
                    self.card_flip_vel[i] = 0.0;
                }
                if nf > std::f32::consts::PI {
                    nf = std::f32::consts::PI;
                    self.card_flip_vel[i] = 0.0;
                }
                if self.card_flip_vel[i].abs() < 0.0004 {
                    self.card_flip_vel[i] = 0.0;
                }
                self.card_flip[i] = nf;
            } else {
                // El cursor ya no esta encima: regresa LENTO y fluido a su posicion original.
                self.card_flip_vel[i] = 0.0;
                self.card_flip[i] += (0.0 - self.card_flip[i]) * (1.0 - (-1.8 * dt).exp());
                if self.card_flip[i].abs() < 0.002 {
                    self.card_flip[i] = 0.0;
                }
            }
        }
        self.card_rects = layout.iter().map(|(c, h)| (c.x, c.y, h.x, h.y)).collect();
    }

    /// Indice de la carta bajo el cursor (None si ninguna). Sin distinguir la papelera.
    fn library_card_at(&self) -> Option<usize> {
        let cur = self.cursor;
        self.card_rects
            .iter()
            .position(|&(cx, cy, hx, hy)| (cur.x - cx).abs() <= hx && (cur.y - cy).abs() <= hy)
    }

    /// Renombra el cuaderno `idx`: cambia su nombre y MUEVE su archivo para que el nombre de
    /// archivo coincida (asi se conserva al guardar). Tambien actualiza el orden manual.
    fn rename_notebook(&mut self, idx: usize, new_name: &str) {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return;
        }
        let Some(entry) = self.notebooks.get(idx) else { return };
        // Si el nombre no cambio, no hacemos NADA (evita el "lagazo" de releer todo el disco).
        if entry.name == new_name {
            return;
        }
        let old_path = entry.path.clone();
        let new_path = notebook::path_for(new_name);
        if let Some(mut nb) = notebook::load(&old_path) {
            nb.name = new_name.to_string();
            let _ = notebook::save(&nb, &new_path);
            if new_path != old_path {
                notebook::delete(&old_path);
                // Mantener la posicion en el orden manual (reemplazar el nombre de archivo).
                let oldfn = old_path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string());
                let newfn = new_path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string());
                if let (Some(o), Some(n)) = (oldfn.clone(), newfn) {
                    let mut order = notebook::load_order();
                    let mut found = false;
                    for it in order.iter_mut() {
                        if *it == o {
                            *it = n.clone();
                            found = true;
                        }
                    }
                    if !found {
                        order.push(n);
                    }
                    notebook::save_order(&order);
                }
                if self.current_path.as_deref() == Some(old_path.as_path()) {
                    self.current_path = Some(new_path.clone());
                }
            }
        }
        // Actualizar SOLO la entrada en memoria (sin releer todo el disco -> sin lagazo).
        if let Some(e) = self.notebooks.get_mut(idx) {
            e.name = new_name.to_string();
            e.path = new_path;
        }
    }

    /// Zona de la PAPELERA unica (centro y radio, en px): arrastra una carta aqui para borrarla.
    fn trash_zone(&self) -> (f32, f32, f32) {
        let vp = self.camera.viewport;
        (vp.x - 90.0, 86.0, 50.0)
    }

    /// ¿El cursor esta sobre la papelera (para soltar y borrar)?
    fn over_trash_zone(&self) -> bool {
        let (tx, ty, r) = self.trash_zone();
        (self.cursor.x - tx).hypot(self.cursor.y - ty) < r
    }

    /// Si el cursor esta sobre una fila de archivero de la barra lateral, devuelve su nombre
    /// ("" = Todos, para quitar el cuaderno de cualquier archivero). None si no hay ninguna.
    fn archivero_at_cursor(&self) -> Option<String> {
        let (x, y) = (self.cursor.x, self.cursor.y);
        self.archivero_drop
            .iter()
            .find(|(x0, y0, x1, y1, _)| x >= *x0 && x <= *x1 && y >= *y0 && y <= *y1)
            .map(|(_, _, _, _, name)| name.clone())
    }

    /// Asigna el cuaderno `i` al archivero `name` ("" = ninguno): persiste el campo y actualiza la
    /// copia en memoria SIN recargar la lista (asi no hay "salto" ni reinicio de las animaciones).
    fn assign_archivero(&mut self, i: usize, name: String) {
        let path = match self.notebooks.get(i) {
            Some(nb) if nb.archivero != name => nb.path.clone(),
            _ => return,
        };
        if let Some(mut data) = notebook::load(&path) {
            data.archivero = name.clone();
            let _ = notebook::save(&data, &path);
        }
        if let Some(nb) = self.notebooks.get_mut(i) {
            nb.archivero = name;
        }
    }

    /// Toque/lápiz en la BIBLIOTECA: abre, arrastra y suelta cuadernos igual que el clic
    /// izquierdo del ratón. Las cartas son wgpu (no widgets egui), así que se decide por
    /// hit-test sobre `self.cursor`. Esto permite usar la tableta/lápiz en el Home (antes el
    /// manejador de `Touch` solo servía para dibujar en el lienzo).
    fn touch_library(&mut self, phase: TouchPhase, loc: Vec2) {
        self.cursor = loc;
        self.last_cursor = loc;
        match phase {
            TouchPhase::Started => {
                if self.preview_idx.is_some() {
                    // Tocar durante la vista previa la cierra.
                    self.close_preview();
                } else if !self.creating_nb && self.renaming.is_none() {
                    if let Some(i) = self.library_card_at() {
                        if self.alt_down {
                            self.open_preview(i);
                        } else {
                            self.drag_idx = Some(i);
                            self.drag_start = loc;
                            self.dragging = false;
                        }
                    }
                }
            }
            TouchPhase::Moved => {
                if self.drag_idx.is_some()
                    && !self.dragging
                    && (loc - self.drag_start).length() > 8.0
                {
                    self.dragging = true;
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if let Some(i) = self.drag_idx.take() {
                    if self.dragging && self.over_trash_zone() {
                        self.delete_card(i);
                    } else if self.dragging && self.archivero_at_cursor().is_some() {
                        if let Some(name) = self.archivero_at_cursor() {
                            self.assign_archivero(i, name);
                        }
                    } else if self.dragging {
                        let target = self.library_card_at();
                        match (self.card_is_sheet(i), target) {
                            (true, Some(tg)) if tg != i && !self.card_is_sheet(tg) => {
                                self.merge_prompt = Some((i, tg));
                            }
                            _ => self.drop_card(i),
                        }
                    } else if let Some(nb) = self.notebooks.get(i) {
                        self.open_notebook(nb.path.clone());
                    }
                }
                self.dragging = false;
            }
        }
    }

    /// Borra el cuaderno `i` de la biblioteca.
    fn delete_card(&mut self, i: usize) {
        if let Some(nb) = self.notebooks.get(i) {
            let path = nb.path.clone();
            notebook::delete(&path);
            // Quitar SOLO la animacion de la carta borrada (las demas siguen igual, sin salto).
            if i < self.card_anim.len() {
                self.card_anim.remove(i);
                self.card_flip.remove(i);
                self.card_flip_vel.remove(i);
            }
            self.notebooks = notebook::list();
        }
    }

    /// Suelta la carta arrastrada `from` en el hueco mas cercano al cursor y guarda el nuevo
    /// orden (persistente). Los demas cuadernos se desplazan para hacer sitio.
    fn drop_card(&mut self, from: usize) {
        let layout = self.library_card_layout();
        if layout.is_empty() || from >= self.notebooks.len() {
            return;
        }
        // Hueco destino = carta cuyo centro queda mas cerca del cursor.
        let mut target = from;
        let mut best = f32::MAX;
        for (j, (c, _h)) in layout.iter().enumerate() {
            let d = (c.x - self.cursor.x).hypot(c.y - self.cursor.y);
            if d < best {
                best = d;
                target = j;
            }
        }
        if target == from {
            return;
        }
        let mut files: Vec<String> = self
            .notebooks
            .iter()
            .filter_map(|nb| nb.path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()))
            .collect();
        if from >= files.len() {
            return;
        }
        let f = files.remove(from);
        let ins = target.min(files.len());
        files.insert(ins, f);
        notebook::save_order(&files);
        // Reordenar TAMBIEN las animaciones para que SIGAN a su cuaderno: asi no hay "salto" ni
        // reinicio de los demas cuadernos al soltar (no se limpian, solo se mueven con su carta).
        Self::reorder_vec(&mut self.card_anim, from, ins);
        Self::reorder_vec(&mut self.card_flip, from, ins);
        Self::reorder_vec(&mut self.card_flip_vel, from, ins);
        self.notebooks = notebook::list();
    }

    /// Mueve el elemento `from` a la posicion `to` dentro del vector (remove + insert).
    fn reorder_vec<T>(v: &mut Vec<T>, from: usize, to: usize) {
        if from < v.len() {
            let x = v.remove(from);
            v.insert(to.min(v.len()), x);
        }
    }

    /// ¿La carta `i` es una nota rapida (hoja, finish 500..599)? (Las figuras 3D, >=600, NO.)
    fn card_is_sheet(&self, i: usize) -> bool {
        self.notebooks.get(i).map_or(false, |n| n.finish >= 500 && n.finish < 600)
    }

    /// Intercambia la POSICION de dos cartas en el orden manual (persistente).
    fn swap_cards(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        let mut files: Vec<String> = self
            .notebooks
            .iter()
            .filter_map(|nb| nb.path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()))
            .collect();
        if a >= files.len() || b >= files.len() {
            return;
        }
        files.swap(a, b);
        notebook::save_order(&files);
        // Intercambiar tambien sus animaciones (sin reiniciar las demas -> sin salto).
        if a < self.card_anim.len() && b < self.card_anim.len() {
            self.card_anim.swap(a, b);
            self.card_flip.swap(a, b);
            self.card_flip_vel.swap(a, b);
        }
        self.notebooks = notebook::list();
    }

    /// Fusiona la nota `from` DENTRO del cuaderno `target`: anexa sus paginas al final de las del
    /// cuaderno y borra la nota suelta (y su entrada del orden).
    fn merge_note_into(&mut self, from: usize, target: usize) {
        let (Some(fe), Some(te)) = (self.notebooks.get(from), self.notebooks.get(target)) else {
            return;
        };
        let fpath = fe.path.clone();
        let tpath = te.path.clone();
        let (Some(fnb), Some(mut tnb)) = (notebook::load(&fpath), notebook::load(&tpath)) else {
            return;
        };
        // Anexar las hojas de la nota al final del cuaderno destino.
        tnb.pages.extend(fnb.pages);
        let _ = notebook::save(&tnb, &tpath);
        // Borrar la nota suelta y quitarla del orden.
        notebook::delete(&fpath);
        if let Some(fname) = fpath.file_name().and_then(|s| s.to_str()) {
            let mut order = notebook::load_order();
            order.retain(|o| o != fname);
            notebook::save_order(&order);
        }
        // Quitar SOLO la animacion de la nota fusionada (las demas siguen igual, sin salto).
        if from < self.card_anim.len() {
            self.card_anim.remove(from);
            self.card_flip.remove(from);
            self.card_flip_vel.remove(from);
        }
        self.notebooks = notebook::list();
    }

    /// Abre el panel para EDITAR la carátula del cuaderno `i` (clic derecho): carga sus
    /// valores actuales y entra en modo edición.
    fn start_edit_cover(&mut self, i: usize) {
        if let Some(nb) = self.notebooks.get(i) {
            self.new_nb_finish = nb.finish;
            self.new_nb_fx = nb.fx;
            self.new_nb_intensity = nb.fx_intensity;
            self.new_nb_accent = nb.accent;
            self.new_nb_shape = nb.shape;
            self.new_nb_thickness = nb.thickness;
            self.new_nb_overhang = nb.overhang;
            self.new_nb_texture = nb.texture;
            self.new_nb_cover_a = nb.cover_a;
            self.new_nb_cover_b = nb.cover_b;
            self.editing_nb = Some(nb.path.clone());
            self.creating_nb = true;
        }
    }

    /// Guarda SOLO la carátula (diseño + capas) en un cuaderno ya existente, sin tocar sus
    /// paginas, y refresca la biblioteca.
    fn save_cover_edit(&mut self, path: &Path) {
        if let Some(mut nb) = notebook::load(path) {
            nb.finish = self.new_nb_finish;
            nb.fx = self.new_nb_fx;
            nb.fx_intensity = self.new_nb_intensity;
            nb.accent = self.new_nb_accent;
            nb.shape = self.new_nb_shape;
            nb.thickness = self.new_nb_thickness;
            nb.overhang = self.new_nb_overhang;
            nb.texture = self.new_nb_texture;
            nb.cover_a = self.new_nb_cover_a;
            nb.cover_b = self.new_nb_cover_b;
            let _ = notebook::save(&nb, path);
        }
        self.notebooks = notebook::list();
        // Si la carta editada esta abierta, conservar tambien en memoria.
        if self.current_path.as_deref() == Some(path) {
            self.current_finish = self.new_nb_finish;
            self.current_fx = self.new_nb_fx;
            self.current_intensity = self.new_nb_intensity;
            self.current_accent = self.new_nb_accent;
            self.current_shape = self.new_nb_shape;
            self.current_thickness = self.new_nb_thickness;
            self.current_overhang = self.new_nb_overhang;
            self.current_texture = self.new_nb_texture;
            self.current_cover_a = self.new_nb_cover_a;
            self.current_cover_b = self.new_nb_cover_b;
        }
    }

    /// Cuad de FONDO del Home a pantalla completa (finish 900 -> degradado/viñeta en el shader).
    /// Se dibuja el primero (detras de las cartas) para dar profundidad.
    fn bg_card(&self) -> renderer::CardInstance {
        let vp = self.camera.viewport;
        let finish = if self.lib_tweaks.theme == 1 { 901.0 } else { 900.0 };
        [
            vp.x * 0.5, vp.y * 0.5, vp.x * 0.5 + 4.0, vp.y * 0.5 + 4.0, 0.0, 0.0, 0.5, 0.5, 0.0,
            0.0, 0.0, 0.0, finish, 0.0, 1.0, 0.0, 0.0, 0.0, 0.001, 0.0, 0.0, 4.0, 0.0,
        ]
    }

    /// Construye las instancias de carta para la GPU. La carta bajo el cursor se dibuja al
    /// final (encima de las demas, ya que se eleva en 3D).
    fn build_card_instances(&self, layout: &[(Vec2, Vec2)]) -> Vec<renderer::CardInstance> {
        let cur = self.cursor;
        let mut cards: Vec<renderer::CardInstance> = Vec::with_capacity(layout.len());
        let mut hover_idx: Option<usize> = None;
        for (i, (c, h)) in layout.iter().enumerate() {
            let a = self.card_anim.get(i).copied().unwrap_or([0.0; 3]);
            // La carta que se esta arrastrando SIGUE al cursor (elevada), para reordenar.
            let dragged = self.dragging && self.drag_idx == Some(i);
            let center = if dragged { cur } else { *c };
            let flip = self.card_flip.get(i).copied().unwrap_or(0.0); // rueda = voltear (ver reverso)
            let nb = self.notebooks.get(i);
            let finish = nb.map_or(1, |n| n.finish);
            let is_sheet = finish >= 500 && finish < 600; // nota rapida (500..599) = HOJA plana
            // Las HOJAS van planas DE FRENTE (sin giro/inclinacion del estante) para no verse
            // "arrugadas"; los cuadernos si toman su pose 3D del estante.
            let (rotx, roty, hov) = if dragged {
                (0.0, flip, 1.0)
            } else if is_sheet {
                (0.0, flip, a[0])
            } else {
                (a[1], a[2] + flip, a[0])
            };
            // El "puntero" del efecto solo sigue al cursor segun el HOVER: si el cursor no esta
            // sobre la carta (p.ej. en la fila de abajo, misma columna), queda neutro (0.5) y la
            // portada no reacciona. Mezclado por `hov` -> transicion suave.
            let cr_x = ((cur.x - (center.x - h.x)) / (2.0 * h.x)).clamp(0.0, 1.0);
            let cr_y = ((cur.y - (center.y - h.y)) / (2.0 * h.y)).clamp(0.0, 1.0);
            let ptr_x = 0.5 + (cr_x - 0.5) * hov;
            let ptr_y = 0.5 + (cr_y - 0.5) * hov;
            let fx = nb.map_or(0, |n| n.fx);
            let inten = nb.map_or(1.0, |n| n.fx_intensity);
            let accent = nb.map_or(0, |n| n.accent);
            let shape = if is_sheet { 4 } else { nb.map_or(0, |n| n.shape) };
            let (df, ohf, bf) = shape_params(shape);
            // Hoja: muy fina (papel); cuaderno: grosor segun su forma.
            let depth = if is_sheet { 0.012 } else { df * nb.map_or(1.0, |n| n.thickness) };
            let overh = if is_sheet { 0.0 } else { ohf * nb.map_or(1.0, |n| n.overhang) };
            // Color de TODO el cuaderno (COPIC): finish 800 = solido (cover_a); 801 = degradado
            // (cover_a -> cover_b). Reutiliza los slots base/accent de la instancia.
            let mut base = finish_base_color(finish);
            let mut ac = accent_color(accent);
            if finish == 800 || finish == 801 {
                if let Some(n) = nb {
                    base = n.cover_a;
                    if finish == 801 {
                        ac = n.cover_b;
                    }
                }
            }
            // FLOTACION: cuando el cursor esta encima (hov>0), el libro flota lentamente -como
            // gravedad cero-: vaiven sutil en rotacion + leve sube/baja. Fase por carta (segun el
            // indice) para que no floten todos sincronizados. Se desvanece con `hov`.
            let t = self.clock;
            let ph = i as f32 * 1.7;
            // Las hojas NO giran (se mantienen planas); solo un leve sube/baja al pasar el cursor.
            let (rotx, roty) = if is_sheet {
                (rotx, roty)
            } else {
                (
                    rotx + (t * 1.5 + ph).sin() * 0.055 * hov,
                    roty + (t * 1.2 + ph * 1.3).cos() * 0.065 * hov,
                )
            };
            let cy = center.y - (t * 1.3 + ph).sin() * (if is_sheet { 4.0 } else { 11.0 }) * hov;
            let texture = nb.map_or(0, |n| n.texture);
            cards.push([
                center.x, cy, h.x, h.y, rotx, roty, ptr_x, ptr_y, hov,
                base[0], base[1], base[2], finish as f32, fx as f32, inten, ac[0], ac[1], ac[2],
                depth, overh, bf, shape as f32, texture as f32,
            ]);
            if dragged || a[0] > 0.45 {
                hover_idx = Some(i);
            }
        }
        if let Some(hi) = hover_idx {
            let card = cards.remove(hi);
            cards.push(card);
        }
        cards
    }

    /// Rectangulo (centro, medio-tamano en px) de la carta de VISTA PREVIA del panel de
    /// creación: grande, a la izquierda de la pantalla (proporcion 5:7).
    fn preview_rect(&self) -> (Vec2, Vec2) {
        let vp = self.camera.viewport;
        let hh = (vp.y * 0.30).clamp(180.0, 720.0);
        let hw = hh * (188.0 / 263.0);
        (vec2(vp.x * 0.28, vp.y * 0.54), vec2(hw, hh))
    }

    /// Construye la UNICA carta de vista previa (efecto pleno + vaiven 3D automatico) que se
    /// muestra mientras se elige la carátula en el panel de creación.
    fn build_preview_card(&self) -> Vec<renderer::CardInstance> {
        let (c, h) = self.preview_rect();
        let finish = self.new_nb_finish;
        let mut base = finish_base_color(finish);
        let mut ac = accent_color(self.new_nb_accent);
        if finish == 800 || finish == 801 {
            base = self.new_nb_cover_a;
            if finish == 801 {
                ac = self.new_nb_cover_b;
            }
        }
        let (df, ohf, bf) = shape_params(self.new_nb_shape);
        let depth = df * self.new_nb_thickness;
        let overh = ohf * self.new_nb_overhang;
        let t = self.clock;
        let rotx = (t * 0.7).sin() * 0.12; // vaiven suave para lucir el 3D
        let roty = (t * 0.9).cos() * 0.16;
        let ptr_x = 0.5 + 0.42 * (t * 0.6).sin(); // el brillo recorre la carta
        let ptr_y = 0.5 + 0.42 * (t * 0.5).cos();
        vec![[
            c.x, c.y, h.x, h.y, rotx, roty, ptr_x, ptr_y, 1.0,
            base[0], base[1], base[2], finish as f32, self.new_nb_fx as f32,
            self.new_nb_intensity, ac[0], ac[1], ac[2],
            depth, overh, bf, self.new_nb_shape as f32, self.new_nb_texture as f32,
        ]]
    }

    /// Abre la VISTA PREVIA (Alt+clic) del cuaderno `i`: carga su 1ª pagina y precalcula los
    /// trazos (puntos en mundo, color, ancho) para previsualizar el contenido sin abrir el libro.
    fn open_preview(&mut self, i: usize) {
        let Some(entry) = self.notebooks.get(i) else { return };
        let path = entry.path.clone();
        let name = entry.name.clone();
        let Some(nb) = notebook::load(&path) else { return };
        // Precalcular TODAS las paginas (para poder pasarlas en la vista previa).
        let mut pages: Vec<PreviewPage> = Vec::new();
        for pg in &nb.pages {
            let mut strokes: Vec<PreviewStroke> = Vec::new();
            let mut mn = Vec2::splat(f32::MAX);
            let mut mx = Vec2::splat(f32::MIN);
            for layer in &pg.doc.layers {
                if !layer.visible {
                    continue;
                }
                for st in &layer.strokes {
                    if st.samples.is_empty() {
                        continue;
                    }
                    let pts: Vec<Vec2> = st.samples.iter().map(|s| s.pos).collect();
                    for p in &pts {
                        mn = mn.min(*p);
                        mx = mx.max(*p);
                    }
                    let mut c = st.brush.color;
                    c[3] *= layer.opacity * st.brush.opacity;
                    strokes.push((pts, c, st.brush.width));
                }
            }
            let bounds = if mx.x >= mn.x { Some((mn, mx)) } else { None };
            pages.push((strokes, bounds));
        }
        if pages.is_empty() {
            pages.push((Vec::new(), None));
        }
        // Rect de origen (la carta en la rejilla) para que la animacion "venga" desde ahi.
        let layout = self.library_card_layout();
        self.preview_from = layout.get(i).copied().unwrap_or((
            vec2(self.camera.viewport.x * 0.5, self.camera.viewport.y * 0.5),
            vec2(94.0, 131.0),
        ));
        self.preview_idx = Some(i);
        self.preview_t = 0.0;
        self.preview_name = name;
        self.preview_pages = pages;
        self.preview_page = 0;
    }

    /// Cierra la vista previa.
    fn close_preview(&mut self) {
        self.preview_idx = None;
        self.preview_t = 0.0;
        self.preview_pages.clear();
        self.preview_page = 0;
    }

    /// Pasa de pagina en la vista previa (dir = +1 / -1), con limites.
    fn preview_flip(&mut self, dir: i32) {
        if self.preview_pages.is_empty() {
            return;
        }
        let last = self.preview_pages.len() - 1;
        let cur = self.preview_page as i32 + dir;
        self.preview_page = cur.clamp(0, last as i32) as usize;
    }

    /// La cuadricula del cuaderno en vista previa: si es nota rapida (hoja) la da su `finish`;
    /// si no, el papel actual (settings.grid).
    fn preview_grid_kind(&self) -> ink_core::GridKind {
        use ink_core::GridKind::*;
        let finish = self
            .preview_idx
            .and_then(|i| self.notebooks.get(i))
            .map_or(0, |n| n.finish);
        match finish {
            500 => Lines,
            501 => Squares,
            502 => Dots,
            503 => None,
            504 => Iso,
            505 => Triangle,
            _ => self.settings.grid,
        }
    }

    /// Dibuja la cuadricula (papel) dentro de un rectangulo de pagina.
    fn draw_preview_grid(p: &egui::Painter, r: egui::Rect, kind: ink_core::GridKind) {
        use ink_core::GridKind::*;
        let col = egui::Color32::from_rgba_unmultiplied(95, 125, 175, 90);
        let step = (r.height() / 22.0).max(7.0);
        let st = egui::Stroke::new(1.0, col);
        match kind {
            Lines => {
                let mut y = r.top() + step;
                while y < r.bottom() - 1.0 {
                    p.hline(r.x_range(), y, st);
                    y += step;
                }
            }
            Squares => {
                let mut y = r.top() + step;
                while y < r.bottom() - 1.0 {
                    p.hline(r.x_range(), y, st);
                    y += step;
                }
                let mut x = r.left() + step;
                while x < r.right() - 1.0 {
                    p.vline(x, r.y_range(), st);
                    x += step;
                }
            }
            Dots => {
                let mut y = r.top() + step;
                while y < r.bottom() - 1.0 {
                    let mut x = r.left() + step;
                    while x < r.right() - 1.0 {
                        p.circle_filled(egui::pos2(x, y), 1.3, col);
                        x += step;
                    }
                    y += step;
                }
            }
            _ => {}
        }
    }

    /// Dibuja el contenido (trazos) de una pagina ajustado a un rectangulo (recortado a el).
    fn draw_preview_page(p: &egui::Painter, r: egui::Rect, page: &PreviewPage) {
        let (strokes, bounds) = page;
        let Some((mn, mx)) = bounds else { return };
        let avail = r.shrink(r.width().min(r.height()) * 0.07);
        let bw = (mx.x - mn.x).max(1.0);
        let bh = (mx.y - mn.y).max(1.0);
        let s = (avail.width() / bw).min(avail.height() / bh);
        let bcx = (mn.x + mx.x) * 0.5;
        let bcy = (mn.y + mx.y) * 0.5;
        let rc = avail.center();
        let pp = p.with_clip_rect(r);
        for (pts, col, w) in strokes {
            if pts.len() < 2 {
                continue;
            }
            let c = egui::Color32::from_rgba_unmultiplied(
                (col[0] * 255.0) as u8,
                (col[1] * 255.0) as u8,
                (col[2] * 255.0) as u8,
                (col[3] * 255.0) as u8,
            );
            let sw = (w * s).max(0.6);
            let line: Vec<egui::Pos2> = pts
                .iter()
                .map(|q| egui::pos2(rc.x + (q.x - bcx) * s, rc.y + (q.y - bcy) * s))
                .collect();
            pp.add(egui::Shape::line(line, egui::Stroke::new(sw, c)));
        }
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

    /// Config del suavizador para un pincel de Photoshop (estampados): mismo densificado y
    /// presion fluida; el taper rampa la presion (solo afecta si el pincel usa presion).
    fn ink_config_ps(&self) -> InkConfig {
        let zoom = self.camera.zoom.max(1e-4);
        let size = self.ps_settings.as_ref().map_or(self.brush.width, |s| s.size);
        InkConfig {
            max_step: (1.3 / zoom).max(0.05),
            taper_in: (size * 1.2).max(1.5 / zoom),
            pressure_smooth: 0.6,
            smoothing: self.brush.smoothing,
            use_pressure: true,
        }
    }

    fn start_stroke_ps(&mut self, pressure: f32) {
        self.ps_drawing = true;
        self.ps_samples.clear();
        self.ps_index = 0;
        self.ps_residual = 0.0;
        self.ps_active_verts.clear();
        let world = self.camera.screen_to_world(self.cursor);
        // Mismo suavizador de entrada que los pinceles basicos (curvas suaves + presion fluida).
        self.last_sample_time = Instant::now();
        let cfg = self.ink_config_ps();
        self.smoother.reset(cfg);
        for sm in self.smoother.push(world, pressure.clamp(0.05, 1.0), 1.0 / 120.0) {
            self.last_sample_pos = sm.pos;
            self.ps_samples.push(sm);
        }
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
        let dt = (now - self.last_sample_time).as_secs_f32().clamp(1e-4, 0.1);
        self.last_sample_time = now;
        let Some(s) = self.ps_settings.clone() else { return };
        let tip = match s.tip {
            TipKind::Sampled(id) => id,
            _ => 0,
        };
        let aspect = self.gpu.as_ref().map_or(1.0, |g| g.tip_aspect(tip));
        let rgb = self.ps_brush_rgb();
        // El suavizador emite muestras densas y suaves; estampamos el tramo de cada una.
        let new = self.smoother.push(raw_world, pressure, dt);
        for sm in new {
            self.ps_samples.push(sm);
            self.last_sample_pos = sm.pos;
            let n = self.ps_samples.len();
            if n < 2 {
                continue;
            }
            let out = stamp_path(&self.ps_samples[n - 2..n], &s, self.ps_index, self.ps_residual);
            self.ps_index = out.next_index;
            self.ps_residual = out.residual;
            if out.stamps.is_empty() {
                continue;
            }
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
        // Cerrar la cola del suavizado: estampar el ultimo tramo pendiente.
        let tail = self.smoother.finish();
        if !tail.is_empty() {
            let aspect = self.gpu.as_ref().map_or(1.0, |g| g.tip_aspect(tip));
            let rgb = self.ps_brush_rgb();
            for sm in tail {
                self.ps_samples.push(sm);
                self.last_sample_pos = sm.pos;
                let n = self.ps_samples.len();
                if n < 2 {
                    continue;
                }
                let out = stamp_path(&self.ps_samples[n - 2..n], &s, self.ps_index, self.ps_residual);
                self.ps_index = out.next_index;
                self.ps_residual = out.residual;
                for st in &out.stamps {
                    push_stamp_quad(&mut self.ps_active_verts, st, aspect, rgb);
                }
            }
        }
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

    /// MODO DOCUMENTO: dibuja/edita el cuerpo de texto de la hoja DENTRO de los margenes
    /// (solo en cuadernos de hojas). En `write_mode` es un editor de teclado; si no, el texto
    /// se ve fijo (la tinta del lapiz queda por encima en una fase posterior). El tamano de
    /// fuente, el interlineado y la familia salen de `doc_layout`.
    fn draw_document(&mut self, ctx: &egui::Context) {
        let Some((w, h)) = self.settings.artboard_size() else { return };
        let ppp = ctx.pixels_per_point().max(0.01);
        let m = self.doc_layout.margins; // [arriba, derecha, abajo, izquierda]
        let p0 = self.camera.world_to_screen(Vec2::new(-w * 0.5 + m[3], -h * 0.5 + m[0])) / ppp;
        let p1 = self.camera.world_to_screen(Vec2::new(w * 0.5 - m[1], h * 0.5 - m[2])) / ppp;
        let rect = egui::Rect::from_two_pos(egui::pos2(p0.x, p0.y), egui::pos2(p1.x, p1.y));
        if rect.width() < 20.0 || rect.height() < 20.0 {
            return;
        }
        // Si no hay texto y no estamos escribiendo, no dibujar nada (hoja en blanco para tinta).
        if !self.write_mode && self.page_body.is_empty() {
            return;
        }
        let size_pts = (self.doc_layout.font_size * self.camera.zoom / ppp).clamp(5.0, 400.0);
        let fam = match self.doc_layout.font {
            1 => egui::FontFamily::Name("serif".into()),       // Spectral
            2 => egui::FontFamily::Monospace,                  // JetBrains Mono
            3 => egui::FontFamily::Name("doc_lora".into()),
            4 => egui::FontFamily::Name("doc_merri".into()),
            5 => egui::FontFamily::Name("doc_garamond".into()),
            6 => egui::FontFamily::Name("doc_atkinson".into()),
            7 => egui::FontFamily::Name("doc_sourcesans".into()),
            _ => egui::FontFamily::Proportional,               // 0 = Hanken (Sans)
        };
        let ls = self.doc_layout.line_spacing.max(0.5);
        let text_col = egui::Color32::from_rgb(28, 26, 30);
        let writing = self.write_mode;
        let id_te = egui::Id::new("doc_te");
        // Linea donde esta el cursor: en ella se MUESTRAN las marcas (para editarlas); en las
        // demas se ocultan (vista renderizada). Solo en modo escritura.
        let reveal_line = if writing {
            egui::TextEdit::load_state(ctx, id_te)
                .and_then(|st| st.cursor.char_range())
                .map(|r| {
                    let idx = r.primary.index.min(self.page_body.chars().count());
                    self.page_body.chars().take(idx).filter(|&c| c == '\n').count()
                })
        } else {
            None
        };
        let accent = egui::Color32::from_rgb(150, 120, 84);
        let check_col = egui::Color32::from_rgb(60, 160, 90);
        let page_align = self.page_align;
        // Historial de deshacer: registra un punto al pausar el tecleo (~1 s).
        if writing && self.page_body != self.doc_snap && (self.clock - self.doc_snap_at) > 1.0 {
            if self.doc_undo.last().map(|s| s.as_str()) != Some(self.doc_snap.as_str()) {
                self.doc_undo.push(self.doc_snap.clone());
                if self.doc_undo.len() > 300 {
                    self.doc_undo.remove(0);
                }
            }
            self.doc_redo.clear();
            self.doc_snap = self.page_body.clone();
            self.doc_snap_at = self.clock;
        }
        // Job + decoraciones FUERA del closure (markdown_job no necesita `ui`), para poder cargar
        // las imagenes antes (necesitan `&mut self`).
        let (mut job0, decos) = markdown_job(&self.page_body, size_pts, ls, fam.clone(), text_col, reveal_line, page_align);
        job0.wrap.max_width = rect.width();
        let mut imgs: std::collections::HashMap<String, egui::TextureHandle> = std::collections::HashMap::new();
        for (_, d) in &decos {
            if let Deco::Image(pth) = d {
                if !imgs.contains_key(pth) {
                    if let Some(h) = self.load_doc_image(ctx, pth) {
                        imgs.insert(pth.clone(), h);
                    }
                }
            }
        }
        // Tablas: se recogen aqui (con su posicion) y se dibujan DESPUES en una capa por encima,
        // para que sus celdas y los botones +columna/+fila reciban los clics (la caja de texto
        // del documento cubre toda la hoja).
        let mut tables: Vec<(usize, usize, Vec<Vec<String>>, egui::Pos2, f32, f32)> = Vec::new();
        // Paginacion: si una TABLA o IMAGEN no cabe en lo que queda de la hoja, se marca su
        // inicio para moverla (y lo que le siga) a la pagina siguiente. `body_before` permite
        // aplicar el corte solo en un frame SIN tecleo (asi el indice no se desfasa).
        let body_before = self.page_body.clone();
        let mut reflow_cut: Option<usize> = None;
        let body = &mut self.page_body;
        egui::Area::new(egui::Id::new("doc_editor"))
            .order(egui::Order::Middle)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                ui.set_clip_rect(rect);
                let galley = ui.painter().layout_job(job0);
                if writing {
                    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                        let (mut j, _) = markdown_job(buf.as_str(), size_pts, ls, fam.clone(), text_col, reveal_line, page_align);
                        j.wrap.max_width = wrap;
                        ui.painter().layout_job(j)
                    };
                    let te = egui::TextEdit::multiline(body)
                        .id(id_te)
                        .frame(egui::Frame::NONE)
                        .desired_width(rect.width())
                        .hint_text("Escribe aquí…")
                        .layouter(&mut layouter);
                    let r = ui.add_sized(rect.size(), te);
                    if !r.has_focus() && ui.memory(|m| m.focused()).is_none() {
                        r.request_focus();
                    }
                } else {
                    ui.painter().galley(rect.min, galley.clone(), text_col);
                }
                // Decoraciones (casilla, cita, regla, imagen) sobre el texto renderizado.
                let painter = ui.painter();
                for (cidx, deco) in &decos {
                    let cr = galley.pos_from_cursor(egui::text::CCursor::new(*cidx));
                    let top = rect.min + cr.min.to_vec2();
                    let rowh = cr.height().max(size_pts);
                    match deco {
                        Deco::Check(done) => {
                            let s = (size_pts.min(rowh) * 0.82).max(8.0);
                            let bx = egui::Rect::from_min_size(egui::pos2(top.x + 1.0, top.y + (rowh - s) * 0.5), egui::vec2(s, s));
                            painter.rect_stroke(bx, egui::CornerRadius::same(3), egui::Stroke::new(1.6, if *done { check_col } else { accent }), egui::StrokeKind::Inside);
                            if *done {
                                let st = egui::Stroke::new(1.8, check_col);
                                painter.line_segment([egui::pos2(bx.left() + s * 0.22, bx.center().y + s * 0.04), egui::pos2(bx.left() + s * 0.42, bx.bottom() - s * 0.24)], st);
                                painter.line_segment([egui::pos2(bx.left() + s * 0.42, bx.bottom() - s * 0.24), egui::pos2(bx.right() - s * 0.18, bx.top() + s * 0.24)], st);
                            }
                        }
                        Deco::Quote => {
                            let x = top.x + 2.0;
                            painter.line_segment([egui::pos2(x, top.y + 1.0), egui::pos2(x, top.y + rowh - 1.0)], egui::Stroke::new(3.0, accent));
                        }
                        Deco::Hr => {
                            let y = top.y + rowh * 0.5;
                            painter.line_segment([egui::pos2(rect.left() + 2.0, y), egui::pos2(rect.right() - 2.0, y)], egui::Stroke::new(1.4, egui::Color32::from_gray(170)));
                        }
                        Deco::Image(pth) => {
                            if let Some(tex) = imgs.get(pth) {
                                let sz = tex.size_vec2();
                                let avw = (rect.width() - 8.0).max(8.0);
                                let avh = (rowh - 8.0).max(8.0);
                                let scale = (avw / sz.x).min(avh / sz.y).max(0.001);
                                let dsz = sz * scale;
                                let r = egui::Rect::from_min_size(egui::pos2(top.x, top.y + 4.0), dsz);
                                painter.image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                            } else {
                                painter.text(egui::pos2(top.x + 2.0, top.y + rowh * 0.5), egui::Align2::LEFT_CENTER, "[imagen no encontrada]", egui::FontId::proportional((size_pts * 0.8).max(8.0)), egui::Color32::from_gray(150));
                            }
                        }
                        Deco::Table { cells, cstart, cend, col_scale, row_scale } => {
                            // Se dibuja despues, en una capa por encima (clics funcionan).
                            tables.push((*cstart, *cend, cells.clone(), top, *col_scale, *row_scale));
                        }
                    }
                    // Paginacion: marcar la PRIMERA tabla/imagen que no cabe y deja algo arriba.
                    if writing && reflow_cut.is_none() {
                        let bh = match deco {
                            Deco::Table { cells, row_scale, .. } => cells.len() as f32 * size_pts * 1.9 * *row_scale,
                            Deco::Image(_) => size_pts * 9.0,
                            _ => 0.0,
                        };
                        if bh > 1.0 && *cidx > 0 && top.y > rect.top() + size_pts && top.y + bh > rect.bottom() + 1.0 {
                            let cut_line = body.chars().take(*cidx).filter(|&c| c == '\n').count();
                            // Solo si el cursor esta ANTES del bloque (no arrastrarlo de pagina).
                            if reveal_line.map_or(true, |rl| rl < cut_line) {
                                reflow_cut = Some(*cidx);
                            }
                        }
                    }
                }
            });
        // Paginacion: aplicar el corte SOLO si no se tecleo este frame (indice consistente) y no
        // se esta editando una celda (no arrancar la tabla mientras escribes en ella).
        if let Some(cut) = reflow_cut {
            if self.page_body == body_before && self.editing_cell.is_none() {
                self.reflow_block_to_next_page(cut);
            }
        }
        // Dibujar las tablas por ENCIMA. avail_w = ancho de contenido de la hoja; cada columna se
        // auto-ajusta al texto y, si la tabla excede el ancho, aparece un deslizador horizontal.
        let avail_w = (rect.width() - 2.0).max(40.0);
        for (cstart, cend, cells, top, col_scale, row_scale) in tables {
            self.render_table(ctx, cstart, cend, &cells, top, avail_w, size_pts, fam.clone(), text_col, col_scale, row_scale);
        }
    }

    /// Dibuja UNA tabla en una capa por encima del editor. Columnas con ANCHO AUTOMATICO (crecen
    /// con el texto); si la tabla supera el ancho de la hoja aparece un DESLIZADOR horizontal.
    /// Celdas editables (clic), Enter pasa a la fila siguiente (creandola), y botones "+".
    fn render_table(&mut self, ctx: &egui::Context, cstart: usize, cend: usize, cells: &[Vec<String>], top: egui::Pos2, avail_w: f32, size_pts: f32, fam: egui::FontFamily, text_col: egui::Color32, col_scale: f32, row_scale: f32) {
        if cells.is_empty() {
            return;
        }
        let nrows = cells.len();
        let ncols = cells.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
        // Alto de fila y ancho minimo de columna: propios de ESTA tabla (de su directiva).
        let cell_h = size_pts * 1.9 * row_scale.clamp(0.5, 3.0);
        let th = nrows as f32 * cell_h;
        let pad = 8.0;
        let head_fam = egui::FontFamily::Name("head".into());
        // Ancho minimo de columna; puede ser muy pequeño (col_scale=0) y crece con el texto.
        let col_min = 16.0 + col_scale.clamp(0.0, 1.0) * 200.0;
        let editing = self.editing_cell.filter(|&(cs, _, _)| cs == cstart);
        let want_focus = self.cell_focus;
        let mut buf = self.cell_buf.clone();
        let mut start_edit: Option<(usize, usize)> = None;
        let (mut add_col, mut add_row, mut lost, mut enter_row) = (false, false, false, false);
        let mut scroll_x = self.table_hscroll.max(0.0);
        egui::Area::new(egui::Id::new(("doc_table", cstart)))
            .order(egui::Order::Foreground)
            .fixed_pos(top)
            .show(ctx, |ui| {
                // Ancho de cada columna = max(min, texto mas ancho + relleno). Se mide con el
                // painter (layout_no_wrap es &self; ctx.fonts no sirve aqui por ser &mut).
                let col_w: Vec<f32> = (0..ncols)
                    .map(|c| {
                        let mut w = col_min;
                        for (r, row) in cells.iter().enumerate() {
                            if let Some(t) = row.get(c) {
                                if !t.is_empty() {
                                    let fid = if r == 0 { egui::FontId::new(size_pts, head_fam.clone()) } else { egui::FontId::new(size_pts, fam.clone()) };
                                    let tw = ui.painter().layout_no_wrap(t.clone(), fid, text_col).size().x;
                                    w = w.max(tw + pad * 2.0);
                                }
                            }
                        }
                        w
                    })
                    .collect();
                let total_w = col_w.iter().sum::<f32>();
                let has_scroll = total_w > avail_w + 1.0;
                let max_scroll = (total_w - avail_w).max(0.0);
                scroll_x = scroll_x.clamp(0.0, max_scroll);
                let view_w = if has_scroll { avail_w } else { total_w };
                let mut xstart = vec![0.0f32; ncols + 1];
                for c in 0..ncols {
                    xstart[c + 1] = xstart[c] + col_w[c];
                }
                ui.set_clip_rect(egui::Rect::from_min_size(top - egui::vec2(2.0, 2.0), egui::vec2(view_w + 26.0, th + 30.0)));
                let painter = ui.painter().clone();
                let grid = egui::Stroke::new(1.0, egui::Color32::from_gray(120));
                let ox = top.x - scroll_x;
                for r in 0..=nrows {
                    let y = top.y + r as f32 * cell_h;
                    painter.line_segment([egui::pos2(top.x, y), egui::pos2(top.x + view_w, y)], grid);
                }
                for c in 0..=ncols {
                    let x = ox + xstart[c];
                    if x >= top.x - 0.5 && x <= top.x + view_w + 0.5 {
                        painter.line_segment([egui::pos2(x, top.y), egui::pos2(x, top.y + th)], grid);
                    }
                }
                for r in 0..nrows {
                    for c in 0..ncols {
                        let cx = ox + xstart[c];
                        let crect = egui::Rect::from_min_size(egui::pos2(cx, top.y + r as f32 * cell_h), egui::vec2(col_w[c], cell_h));
                        if crect.right() < top.x - 0.5 || crect.left() > top.x + view_w + 0.5 {
                            continue; // fuera del area visible (scroll)
                        }
                        if editing == Some((cstart, r, c)) {
                            let inner = crect.shrink(3.0);
                            let resp = ui.put(inner, egui::TextEdit::singleline(&mut buf).frame(egui::Frame::NONE).desired_width(inner.width().max(20.0)).font(egui::FontId::new(size_pts, fam.clone())).text_color(text_col));
                            if want_focus {
                                resp.request_focus(); // SOLO el primer frame (si no, queda atrapada)
                            }
                            if resp.lost_focus() {
                                // Enter -> fila siguiente; Escape o clic fuera -> salir.
                                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    enter_row = true;
                                } else {
                                    lost = true;
                                }
                            }
                        } else {
                            let txt = cells.get(r).and_then(|row| row.get(c)).map(|s| s.as_str()).unwrap_or("");
                            let fid = if r == 0 { egui::FontId::new(size_pts, head_fam.clone()) } else { egui::FontId::new(size_pts, fam.clone()) };
                            painter.text(egui::pos2(crect.left() + pad, crect.center().y), egui::Align2::LEFT_CENTER, txt, fid, text_col);
                            if ui.interact(crect, egui::Id::new(("tcell", cstart, r, c)), egui::Sense::click()).clicked() {
                                start_edit = Some((r, c));
                            }
                        }
                    }
                }
                // Botones "+": solo al pasar el cursor; transparentes (solo el "+").
                let near = egui::Rect::from_min_size(top, egui::vec2(view_w + 24.0, th + 24.0));
                let hovering = ui.rect_contains_pointer(near);
                let cbtn = egui::Rect::from_min_size(egui::pos2(top.x + view_w + 3.0, top.y), egui::vec2(18.0, th));
                let cr = ui.interact(cbtn, egui::Id::new(("tcol", cstart)), egui::Sense::click());
                if hovering || cr.hovered() {
                    let col = if cr.hovered() { egui::Color32::from_gray(40) } else { egui::Color32::from_gray(120) };
                    painter.text(cbtn.center(), egui::Align2::CENTER_CENTER, "+", egui::FontId::proportional(18.0), col);
                }
                if cr.clicked() {
                    add_col = true;
                }
                let rbtn = egui::Rect::from_min_size(egui::pos2(top.x, top.y + th + 2.0), egui::vec2(view_w, 14.0));
                let rr = ui.interact(rbtn, egui::Id::new(("trow", cstart)), egui::Sense::click());
                if hovering || rr.hovered() {
                    let col = if rr.hovered() { egui::Color32::from_gray(40) } else { egui::Color32::from_gray(120) };
                    painter.text(egui::pos2(top.x + view_w * 0.5, rbtn.center().y), egui::Align2::CENTER_CENTER, "+", egui::FontId::proportional(18.0), col);
                }
                if rr.clicked() {
                    add_row = true;
                }
                // Deslizador horizontal cuando la tabla excede el ancho de la hoja.
                if has_scroll {
                    let track_y = top.y + th + 18.0;
                    let track = egui::Rect::from_min_size(egui::pos2(top.x, track_y), egui::vec2(view_w, 6.0));
                    painter.rect_filled(track, egui::CornerRadius::same(3), egui::Color32::from_gray(70));
                    let thumb_w = (view_w * (view_w / total_w)).clamp(28.0, view_w);
                    let frac = if max_scroll > 0.0 { scroll_x / max_scroll } else { 0.0 };
                    let thumb_x = top.x + frac * (view_w - thumb_w);
                    let thumb = egui::Rect::from_min_size(egui::pos2(thumb_x, track_y - 1.0), egui::vec2(thumb_w, 8.0));
                    let sresp = ui.interact(track, egui::Id::new(("tscroll", cstart)), egui::Sense::click_and_drag());
                    if sresp.dragged() && (view_w - thumb_w) > 0.0 {
                        scroll_x = (scroll_x + sresp.drag_delta().x / (view_w - thumb_w) * max_scroll).clamp(0.0, max_scroll);
                    }
                    let tc = if sresp.hovered() || sresp.dragged() { egui::Color32::from_gray(160) } else { egui::Color32::from_gray(110) };
                    painter.rect_filled(thumb, egui::CornerRadius::same(4), tc);
                }
            });
        self.table_hscroll = scroll_x;
        // Consumir la peticion de foco (ya se pidio este frame).
        if want_focus {
            self.cell_focus = false;
        }
        // Aplicar cambios al Markdown de la tabla.
        let replace = |me: &mut Self, nc: &[Vec<String>]| {
            let new_text = serialize_table(nc);
            let mut bchars: Vec<char> = me.page_body.chars().collect();
            let cs = cstart.min(bchars.len());
            let ce = cend.min(bchars.len()).max(cs);
            bchars.splice(cs..ce, new_text.chars());
            me.page_body = bchars.into_iter().collect();
        };
        if add_col {
            let mut nc = cells.to_vec();
            for row in &mut nc {
                while row.len() < ncols {
                    row.push(String::new());
                }
                row.push(String::new());
            }
            replace(self, &nc);
            self.editing_cell = None;
        } else if add_row {
            let mut nc = cells.to_vec();
            nc.push(vec![String::new(); ncols]);
            replace(self, &nc);
            self.editing_cell = None;
        } else if enter_row {
            // Enter en una celda: guardar y bajar a la misma columna de la fila siguiente,
            // creando una fila nueva si estabamos en la ultima.
            if let Some((_, r, c)) = editing {
                let mut nc = cells.to_vec();
                while nc.len() <= r {
                    nc.push(vec![String::new(); ncols]);
                }
                while nc[r].len() <= c {
                    nc[r].push(String::new());
                }
                nc[r][c] = buf.clone();
                if r + 1 >= nc.len() {
                    nc.push(vec![String::new(); ncols]);
                }
                replace(self, &nc);
                let nr = r + 1;
                self.cell_buf = nc.get(nr).and_then(|row| row.get(c)).cloned().unwrap_or_default();
                self.editing_cell = Some((cstart, nr, c));
                self.cell_focus = true;
            }
        } else if let Some((_, r, c)) = editing {
            self.cell_buf = buf.clone();
            let mut nc = cells.to_vec();
            while nc.len() <= r {
                nc.push(vec![String::new(); ncols]);
            }
            while nc[r].len() <= c {
                nc[r].push(String::new());
            }
            if nc[r][c] != buf {
                nc[r][c] = buf;
                replace(self, &nc);
            }
            if lost {
                // Si el foco se perdio por clic en OTRA celda, saltar directo a editarla.
                if let Some((sr, sc)) = start_edit {
                    self.editing_cell = Some((cstart, sr, sc));
                    self.cell_buf = cells.get(sr).and_then(|row| row.get(sc)).cloned().unwrap_or_default();
                    self.cell_focus = true;
                } else {
                    self.editing_cell = None;
                }
            }
        } else if let Some((r, c)) = start_edit {
            self.editing_cell = Some((cstart, r, c));
            self.cell_buf = cells.get(r).and_then(|row| row.get(c)).cloned().unwrap_or_default();
            self.cell_focus = true;
        }
    }

    /// Mueve el bloque que empieza en el caracter `cut` (una tabla/imagen que no cabe) y todo lo
    /// que le sigue al INICIO de la pagina siguiente (creandola si hace falta). El usuario se
    /// queda en la pagina actual; el contenido "fluye" hacia adelante y se navega con ▶.
    fn reflow_block_to_next_page(&mut self, cut: usize) {
        let bchars: Vec<char> = self.page_body.chars().collect();
        let cut = cut.min(bchars.len());
        if cut == 0 {
            return;
        }
        let kept: String = bchars[..cut].iter().collect::<String>().trim_end().to_string();
        let moved: String = bchars[cut..]
            .iter()
            .collect::<String>()
            .trim_start_matches(|c| c == '\n' || c == ' ')
            .to_string();
        if moved.is_empty() {
            return;
        }
        let next = self.current_page + 1;
        if next >= self.pages.len() {
            self.pages.push(notebook::PageData::empty());
        }
        let existing = self.pages[next].body.clone();
        self.pages[next].body = if existing.trim().is_empty() {
            moved
        } else {
            format!("{moved}\n\n{existing}")
        };
        self.page_body = kept;
        if let Some(pg) = self.pages.get_mut(self.current_page) {
            pg.body = self.page_body.clone();
        }
        self.doc_snap = self.page_body.clone();
    }

    /// Carga (cacheada por ruta) la textura de una imagen del documento.
    fn load_doc_image(&mut self, ctx: &egui::Context, path: &str) -> Option<egui::TextureHandle> {
        if let Some(h) = self.doc_img_cache.get(path) {
            return Some(h.clone());
        }
        let bytes = std::fs::read(path).ok()?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
        let handle = ctx.load_texture(format!("docimg_{path}"), color, egui::TextureOptions::LINEAR);
        self.doc_img_cache.insert(path.to_string(), handle.clone());
        Some(handle)
    }

    /// Panel "Diseño de página": margenes, interlineado, espaciado de parrafo, fuente y tamano.
    fn page_setup_window(&mut self, ctx: &egui::Context) {
        if !self.show_page_setup {
            return;
        }
        let mut layout = self.doc_layout;
        let mut open = true;
        egui::Window::new("Diseño de página")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.spacing_mut().slider_width = 180.0;
                ui.label(egui::RichText::new("MÁRGENES (pt)").size(11.0).color(egui::Color32::from_gray(140)));
                ui.add(egui::Slider::new(&mut layout.margins[0], 0.0..=200.0).text("Arriba"));
                ui.add(egui::Slider::new(&mut layout.margins[2], 0.0..=200.0).text("Abajo"));
                ui.add(egui::Slider::new(&mut layout.margins[3], 0.0..=200.0).text("Izquierda"));
                ui.add(egui::Slider::new(&mut layout.margins[1], 0.0..=200.0).text("Derecha"));
                ui.separator();
                ui.add(egui::Slider::new(&mut layout.font_size, 8.0..=48.0).text("Tamaño"));
                ui.add(egui::Slider::new(&mut layout.line_spacing, 1.0..=3.0).text("Interlineado"));
                ui.add(egui::Slider::new(&mut layout.para_spacing, 0.0..=40.0).text("Espacio párrafo"));
                ui.label("Fuente:");
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut layout.font, 0, "Sans");
                    ui.selectable_value(&mut layout.font, 1, "Serif");
                    ui.selectable_value(&mut layout.font, 2, "Mono");
                    ui.selectable_value(&mut layout.font, 3, "Lora");
                    ui.selectable_value(&mut layout.font, 4, "Merriweather");
                    ui.selectable_value(&mut layout.font, 5, "Garamond");
                    ui.selectable_value(&mut layout.font, 6, "Atkinson");
                    ui.selectable_value(&mut layout.font, 7, "Source Sans");
                });
            });
        self.doc_layout = layout;
        if !open {
            self.show_page_setup = false;
        }
    }

    /// Aplica un comando Markdown de la toolbar al `page_body` segun la SELECCION del editor:
    /// negrita/cursiva/tachado ENVUELVEN la seleccion; encabezados/listas/cita ponen un PREFIJO
    /// al inicio de la linea. Actualiza el cursor del editor para que el foco no se pierda.
    fn apply_md(&mut self, ctx: &egui::Context, md: Md) {
        self.doc_snapshot();
        let id = egui::Id::new("doc_te");
        let mut chars: Vec<char> = self.page_body.chars().collect();
        let n = chars.len();
        let (mut lo, mut hi) = (n, n);
        if let Some(state) = egui::TextEdit::load_state(ctx, id) {
            if let Some(r) = state.cursor.char_range() {
                lo = r.primary.index.min(r.secondary.index).min(n);
                hi = r.primary.index.max(r.secondary.index).min(n);
            }
        }
        let new_cursor: usize;
        match md {
            // --- Envolver la seleccion (formato en linea) ---
            Md::Bold | Md::Italic | Md::Underline | Md::Strike | Md::Highlight | Md::Code => {
                let mark: Vec<char> = match md {
                    Md::Bold => "**",
                    Md::Italic => "*",
                    Md::Underline => "__",
                    Md::Strike => "~~",
                    Md::Highlight => "==",
                    _ => "`",
                }
                .chars()
                .collect();
                let ml = mark.len();
                for (k, &c) in mark.iter().enumerate() {
                    chars.insert(hi + k, c);
                }
                for (k, &c) in mark.iter().enumerate() {
                    chars.insert(lo + k, c);
                }
                new_cursor = if lo == hi { lo + ml } else { hi + 2 * ml };
            }
            // --- Enlace [texto](url): la seleccion es el texto; el cursor cae en "url" ---
            Md::Link => {
                let tail: Vec<char> = "](url)".chars().collect();
                for (k, &c) in tail.iter().enumerate() {
                    chars.insert(hi + k, c);
                }
                chars.insert(lo, '[');
                // cursor sobre "url" (tras "[" + texto + "](")
                new_cursor = hi + 1 + 3;
            }
            // --- Regla horizontal: bloque "---" en su propia linea ---
            Md::Hr => {
                let block: Vec<char> = "\n---\n".chars().collect();
                for (k, &c) in block.iter().enumerate() {
                    chars.insert(hi + k, c);
                }
                new_cursor = hi + block.len();
            }
            // --- Sangria: 2 espacios al inicio de la linea ---
            Md::Indent => {
                let mut start = lo.min(chars.len());
                while start > 0 && chars[start - 1] != '\n' {
                    start -= 1;
                }
                chars.insert(start, ' ');
                chars.insert(start, ' ');
                new_cursor = hi + 2;
            }
            // --- Quitar sangria: hasta 2 espacios del inicio de la linea ---
            Md::Outdent => {
                let mut start = lo.min(chars.len());
                while start > 0 && chars[start - 1] != '\n' {
                    start -= 1;
                }
                let mut removed = 0;
                while removed < 2 && start < chars.len() && chars[start] == ' ' {
                    chars.remove(start);
                    removed += 1;
                }
                new_cursor = hi.saturating_sub(removed);
            }
            // --- Bloque de codigo: envolver en vallas ``` en lineas propias ---
            Md::CodeBlock => {
                let close: Vec<char> = "\n```".chars().collect();
                for (k, &c) in close.iter().enumerate() {
                    chars.insert(hi + k, c);
                }
                let open: Vec<char> = "```\n".chars().collect();
                for (k, &c) in open.iter().enumerate() {
                    chars.insert(lo + k, c);
                }
                new_cursor = hi + open.len();
            }
            // --- Limpiar formato: quita marcas Markdown/etiquetas de la seleccion ---
            Md::Clear => {
                let hi = hi.min(chars.len());
                let sel: String = chars[lo..hi].iter().collect();
                let cleaned = strip_md(&sel);
                let midlen = cleaned.chars().count();
                let mut v: Vec<char> = chars[..lo].to_vec();
                v.extend(cleaned.chars());
                v.extend(chars[hi..].iter().copied());
                chars = v;
                new_cursor = lo + midlen;
            }
            // --- Imagen: plantilla ![imagen](ruta); el cursor cae en "ruta" para pegar la ruta ---
            Md::Image => {
                let ins: Vec<char> = "![imagen](ruta)".chars().collect();
                for (k, &c) in ins.iter().enumerate() {
                    chars.insert(hi + k, c);
                }
                new_cursor = hi + 10; // tras "![imagen]("
            }
            // --- Tabla: esqueleto 2x2 vacio en su propio bloque ---
            Md::Table => {
                // La tabla guarda SU tamaño (ancho de columna / alto de fila) en una directiva
                // oculta; asi el tamaño es propio de cada tabla y no afecta a las demas.
                let dir = format!("<!--tbl c={:.2} r={:.2}-->", self.doc_layout.table_scale.clamp(0.0, 1.0), self.doc_layout.table_row.clamp(0.5, 3.0));
                let block: Vec<char> = format!("\n{dir}\n|  |  |\n| --- | --- |\n|  |  |\n").chars().collect();
                for (k, &c) in block.iter().enumerate() {
                    chars.insert(hi + k, c);
                }
                new_cursor = hi + block.len();
            }
            // --- Prefijo de linea (encabezado / lista / cita) ---
            _ => {
                let mut start = lo.min(chars.len());
                while start > 0 && chars[start - 1] != '\n' {
                    start -= 1;
                }
                let prefix: Vec<char> = match md {
                    Md::H1 => "# ",
                    Md::H2 => "## ",
                    Md::H3 => "### ",
                    Md::H4 => "#### ",
                    Md::H5 => "##### ",
                    Md::H6 => "###### ",
                    Md::Bullet => "- ",
                    Md::Number => "1. ",
                    Md::Task => "- [ ] ",
                    Md::Quote => "> ",
                    Md::Callout => "> [!nota] ",
                    _ => "",
                }
                .chars()
                .collect();
                for (k, &c) in prefix.iter().enumerate() {
                    chars.insert(start + k, c);
                }
                new_cursor = hi + prefix.len();
            }
        }
        self.page_body = chars.into_iter().collect();
        let count = self.page_body.chars().count();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let cc = egui::text::CCursor::new(new_cursor.min(count));
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(cc)));
        egui::TextEdit::store_state(ctx, id, state);
        ctx.memory_mut(|m| m.request_focus(id));
        self.doc_snap = self.page_body.clone();
        self.doc_snap_at = self.clock;
    }

    /// Aplica COLOR de texto (o de resaltado si `highlight`) a la seleccion, envolviendola con
    /// una etiqueta `<span>`/`<mark>` que el render oculta y pinta.
    fn apply_color(&mut self, ctx: &egui::Context, rgb: [u8; 3], highlight: bool) {
        self.doc_snapshot();
        let id = egui::Id::new("doc_te");
        let mut chars: Vec<char> = self.page_body.chars().collect();
        let n = chars.len();
        let (mut lo, mut hi) = (n, n);
        if let Some(state) = egui::TextEdit::load_state(ctx, id) {
            if let Some(r) = state.cursor.char_range() {
                lo = r.primary.index.min(r.secondary.index).min(n);
                hi = r.primary.index.max(r.secondary.index).min(n);
            }
        }
        let hex = format!("{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
        let (open, close) = if highlight {
            (format!("<mark style=\"background:#{hex}\">"), "</mark>".to_string())
        } else {
            (format!("<span style=\"color:#{hex}\">"), "</span>".to_string())
        };
        let openv: Vec<char> = open.chars().collect();
        let closev: Vec<char> = close.chars().collect();
        for (k, &c) in closev.iter().enumerate() {
            chars.insert(hi + k, c);
        }
        for (k, &c) in openv.iter().enumerate() {
            chars.insert(lo + k, c);
        }
        let new_cursor = if lo == hi { lo + openv.len() } else { hi + openv.len() + closev.len() };
        self.page_body = chars.into_iter().collect();
        let count = self.page_body.chars().count();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let cc = egui::text::CCursor::new(new_cursor.min(count));
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(cc)));
        egui::TextEdit::store_state(ctx, id, state);
        ctx.memory_mut(|m| m.request_focus(id));
        self.doc_snap = self.page_body.clone();
        self.doc_snap_at = self.clock;
    }

    /// Guarda un punto de historial (el `page_body` actual) para deshacer; limpia el rehacer.
    fn doc_snapshot(&mut self) {
        if self.doc_undo.last().map(|s| s.as_str()) != Some(self.page_body.as_str()) {
            self.doc_undo.push(self.page_body.clone());
            if self.doc_undo.len() > 300 {
                self.doc_undo.remove(0);
            }
        }
        self.doc_redo.clear();
    }

    /// Deshacer en el documento (texto).
    fn doc_undo_op(&mut self, ctx: &egui::Context) {
        if self.page_body != self.doc_snap {
            self.doc_undo.push(self.doc_snap.clone());
        }
        if let Some(prev) = self.doc_undo.pop() {
            self.doc_redo.push(self.page_body.clone());
            self.page_body = prev.clone();
            self.doc_snap = prev;
            self.doc_snap_at = self.clock;
            self.sync_doc_cursor_end(ctx);
        }
    }

    /// Rehacer en el documento (texto).
    fn doc_redo_op(&mut self, ctx: &egui::Context) {
        if let Some(next) = self.doc_redo.pop() {
            self.doc_undo.push(self.page_body.clone());
            self.page_body = next.clone();
            self.doc_snap = next;
            self.doc_snap_at = self.clock;
            self.sync_doc_cursor_end(ctx);
        }
    }

    /// Coloca el cursor del editor al final del texto (tras deshacer/rehacer) y le da el foco.
    fn sync_doc_cursor_end(&self, ctx: &egui::Context) {
        let id = egui::Id::new("doc_te");
        let count = self.page_body.chars().count();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(count))));
        egui::TextEdit::store_state(ctx, id, state);
        ctx.memory_mut(|m| m.request_focus(id));
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
                    // Barra de titulo segun el tema: Tinta (RGB 14,15,20) / Cuaderno (#1c1813).
                    let cap = if self.lib_tweaks.theme == 1 { 0x0013_181C } else { 0x0014_0F0E };
                    set_dark_titlebar(w.hwnd.get(), cap);
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

        setup_fonts(&self.egui_ctx);

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

                if self.zooming {
                    // Alt + arrastrar = zoom: deslizar hacia ARRIBA acerca, hacia abajo aleja.
                    if !self.page_locked() {
                        let dy = self.last_cursor.y - cur.y;
                        if dy != 0.0 {
                            self.camera.zoom_at(self.zoom_anchor, 1.01_f32.powf(dy));
                        }
                    }
                } else if self.panning {
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

                // Biblioteca: si hay un arrastre en curso y el cursor se movio lo suficiente,
                // pasamos a modo "arrastrando" (la carta seguira al cursor para reordenar).
                if self.drag_idx.is_some() && !self.dragging
                    && (cur - self.drag_start).length() > 8.0
                {
                    self.dragging = true;
                }
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
                        if self.app_mode == AppMode::Library && self.preview_idx.is_some() {
                            // Un clic durante la VISTA PREVIA la cierra (vuelve a la biblioteca).
                            self.close_preview();
                        } else if self.app_mode == AppMode::Library && !self.creating_nb {
                            // Cartas (wgpu, no widgets egui): decidir por hit-test, NO por
                            // `egui_consumed`. Alt+clic = VISTA PREVIA (abrir libro sin entrar);
                            // clic normal = posible arrastre (al soltar: abrir / reordenar / borrar).
                            // El NOMBRE se renombra con una zona clicable de egui (ver abajo).
                            if self.renaming.is_none() {
                                if let Some(i) = self.library_card_at() {
                                    if self.alt_down {
                                        self.open_preview(i);
                                    } else {
                                        self.drag_idx = Some(i);
                                        self.drag_start = self.cursor;
                                        self.dragging = false;
                                    }
                                }
                            }
                        } else if egui_consumed {
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
                            if self.alt_down {
                                // Alt + arrastrar = zoom (deslizar para acercar/alejar).
                                self.zooming = true;
                                self.zoom_anchor = self.cursor;
                            } else if self.space_down {
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
                        if self.app_mode == AppMode::Library {
                            // Soltar en la biblioteca: sobre la papelera = borrar; si se arrastro,
                            // reordenar; si no se movio, abrir.
                            if let Some(i) = self.drag_idx.take() {
                                if self.dragging && self.over_trash_zone() {
                                    self.delete_card(i);
                                } else if self.dragging && self.archivero_at_cursor().is_some() {
                                    // Soltar sobre una fila de la barra lateral = asignar a ese archivero.
                                    if let Some(name) = self.archivero_at_cursor() {
                                        self.assign_archivero(i, name);
                                    }
                                } else if self.dragging {
                                    // Si arrastramos una HOJA (nota rapida) sobre un CUADERNO
                                    // (no hoja), ofrecer el menu reordenar/guardar-dentro; si no,
                                    // reordenar normal.
                                    let target = self.library_card_at();
                                    match (self.card_is_sheet(i), target) {
                                        (true, Some(tg)) if tg != i && !self.card_is_sheet(tg) => {
                                            self.merge_prompt = Some((i, tg));
                                        }
                                        _ => self.drop_card(i),
                                    }
                                } else if let Some(nb) = self.notebooks.get(i) {
                                    self.open_notebook(nb.path.clone());
                                }
                            }
                            self.dragging = false;
                        } else if self.zooming {
                            // Siempre cerramos el trazo/pan/gesto para no quedar "pegados".
                            self.zooming = false;
                        } else if self.panning {
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
                        if self.app_mode == AppMode::Library {
                            // Clic derecho sobre una carta = volver a EDITAR su carátula. Las
                            // cartas las dibuja wgpu (no son widgets egui), asi que se decide por
                            // hit-test, no por `egui_consumed` (egui reclama el puntero del panel).
                            if self.preview_idx.is_some() {
                                self.close_preview();
                            } else if !self.creating_nb {
                                if let Some(i) = self.library_card_at() {
                                    self.start_edit_cover(i);
                                }
                            }
                        } else {
                            self.ui.show_brush_settings = !self.ui.show_brush_settings;
                            if self.ui.show_brush_settings {
                                let ppp = self.egui_ctx.pixels_per_point().max(0.01);
                                self.ui.brush_settings_pos = egui::pos2(self.cursor.x / ppp, self.cursor.y / ppp);
                            }
                        }
                    }
                }
                _ => {}
            },

            WindowEvent::ModifiersChanged(m) => {
                self.alt_down = m.state().alt_key();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 120.0,
                };
                if self.app_mode == AppMode::Library && self.preview_idx.is_some() && amount != 0.0 {
                    // En VISTA PREVIA, la rueda PASA DE PAGINA (arriba = anterior, abajo = siguiente).
                    self.preview_flip(if amount > 0.0 { -1 } else { 1 });
                } else if self.app_mode == AppMode::Library && !self.creating_nb && amount != 0.0 {
                    // Biblioteca: la rueda SOBRE un cuaderno/nota lo VOLTEA (gira con inercia),
                    // este la ventana maximizada o no. Si el cursor NO esta sobre ninguna carta
                    // y hay desbordamiento, la rueda DESPLAZA la cuadricula (con tope).
                    if let Some(i) = self.library_card_at() {
                        self.card_flip_vel.resize(self.notebooks.len(), 0.0);
                        if i < self.card_flip_vel.len() {
                            // La rueda da IMPULSO; luego el cuaderno flota girando (inercia).
                            self.card_flip_vel[i] += amount * 0.05;
                        }
                    } else {
                        let max_scroll = self.library_max_scroll();
                        if max_scroll > 0.0 {
                            self.card_scroll = (self.card_scroll - amount * 90.0).clamp(0.0, max_scroll);
                        }
                    }
                }
                if !egui_consumed && self.app_mode == AppMode::Canvas {
                    if amount != 0.0 {
                        // Hace zoom el lienzo infinito siempre; en los cuadernos de hojas la
                        // rueda pasa de pagina, salvo con Alt (zoom) cuando la hoja no esta fija.
                        let zoom_now = matches!(self.settings.artboard, settings::Artboard::Infinite)
                            || (self.alt_down && !self.page_locked());
                        if zoom_now {
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
                // En la BIBLIOTECA el lapiz/tacto navega las cartas (abrir/arrastrar/soltar),
                // no dibuja. (Con Poll el redibujo es continuo, asi que el `return` es seguro.)
                if self.app_mode == AppMode::Library {
                    self.touch_library(t.phase, loc);
                    return;
                }
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
                                if self.alt_down {
                                    // Alt + arrastrar el lapiz = zoom (deslizar para acercar/alejar).
                                    self.zooming = true;
                                    self.zoom_anchor = loc;
                                } else if self.eraser_mode {
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
                        if self.zooming {
                            // Alt + arrastrar = zoom (arriba acerca, abajo aleja).
                            if !self.page_locked() {
                                let dy = self.cursor.y - loc.y;
                                if dy != 0.0 {
                                    self.camera.zoom_at(self.zoom_anchor, 1.01_f32.powf(dy));
                                }
                            }
                            self.cursor = loc;
                        } else {
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
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        if self.zooming {
                            self.zooming = false;
                        } else if self.ui.active_tool() == Some(Tool::PolyLasso) {
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
                // Reloj de animacion de las cartas (acotado para no dar saltos al reanudar).
                self.clock += dt.min(0.05);
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
                let mut lib_create = false;
                let mut lib_save_cover = false;
                let mut lib_open_new = false;
                let mut lib_quick_note = false;
                let mut lib_merge_save = false;
                let mut lib_merge_swap = false;
                let mut lib_merge_cancel = false;
                let mut lib_close_preview = false;
                let mut lib_preview_prev = false;
                let mut lib_preview_next = false;
                let mut lib_set_theme: Option<u32> = None;
                let mut lib_set_archivero: Option<String> = None;
                let mut lib_new_archivero = false;
                let mut lib_create_archivero = false;
                let mut lib_set_sort: Option<u32> = None;
                let mut lib_cancel_new = false;
                let mut lib_toggle_tweaks = false;
                let mut lib_close_tweaks = false;
                let mut tweaks_save = false;
                let mut lib_rename: Option<usize> = None;
                let mut lib_start_rename: Option<usize> = None;
                let mut lib_cancel_rename = false;
                let mut lib_go = false;
                let mut page_prev = false;
                let mut page_next = false;
                let mut page_add = false;
                let mut page_lock_toggle = false;
                let mut toggle_write = false;
                let mut toggle_setup = false;
                let mut md_action: Option<Md> = None;
                let mut md_color: Option<([u8; 3], bool)> = None;
                let mut doc_undo_flag = false;
                let mut doc_redo_flag = false;
                let mut toggle_fullscreen = false;
                let mut md_align: Option<u32> = None;
                // Tamano de la PROXIMA tabla (ancho de columna y alto de fila): se edita en el
                // popup hover del boton Tabla y se guarda como defaults para tablas nuevas.
                let mut table_scale = self.doc_layout.table_scale;
                let mut table_row = self.doc_layout.table_row;
                let in_library = self.app_mode == AppMode::Library;
                let nb_list: Vec<(String, bool, PathBuf)> = if in_library {
                    self.notebooks.iter().map(|n| (n.name.clone(), n.infinite, n.path.clone())).collect()
                } else {
                    Vec::new()
                };
                // Layout + animacion de las cartas de la biblioteca (Home). Con el panel de
                // creación abierto no hay cuadricula (solo la vista previa).
                let show_grid = in_library && !self.creating_nb && self.preview_idx.is_none();
                let card_layout = if show_grid { self.library_card_layout() } else { Vec::new() };
                if show_grid {
                    self.update_card_anim(&card_layout, dt.min(0.05));
                } else {
                    self.card_rects.clear();
                }
                // Animacion de apertura de la vista previa (Alt+clic): preview_t 0 -> 1, suave.
                if self.preview_idx.is_some() {
                    let k = 1.0 - (-6.0 * dt.min(0.05)).exp();
                    self.preview_t += (1.0 - self.preview_t) * k;
                }
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
                    // MODO DOCUMENTO: editor de texto en la hoja (solo cuadernos de hojas).
                    if !matches!(self.settings.artboard, settings::Artboard::Infinite) {
                        self.draw_document(ctx);
                        self.page_setup_window(ctx);
                        // TOOLBAR de edicion (tipo Word): solo en modo escritura.
                        if self.write_mode {
                            egui::Area::new(egui::Id::new("md_toolbar"))
                                .order(egui::Order::Foreground)
                                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
                                .show(ctx, |ui| {
                                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 1.0;
                                            // Historial.
                                            if md_button(ui, Md::Undo, "Deshacer").clicked() { doc_undo_flag = true; }
                                            if md_button(ui, Md::Redo, "Rehacer").clicked() { doc_redo_flag = true; }
                                            if md_button(ui, Md::Clear, "Limpiar formato").clicked() { md_action = Some(Md::Clear); }
                                            ui.separator();
                                            // Encabezados.
                                            if md_button(ui, Md::H1, "Título 1").clicked() { md_action = Some(Md::H1); }
                                            if md_button(ui, Md::H2, "Título 2").clicked() { md_action = Some(Md::H2); }
                                            if md_button(ui, Md::H3, "Título 3").clicked() { md_action = Some(Md::H3); }
                                            // Mas encabezados (H4-H6) en un desplegable.
                                            let rhn = md_button(ui, Md::Hn, "Más títulos");
                                            let phn = ui.make_persistent_id("md_hn_pop");
                                            if rhn.clicked() { ui.memory_mut(|m| m.toggle_popup(phn)); }
                                            #[allow(deprecated)]
                                            egui::popup_below_widget(ui, phn, &rhn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                                                ui.horizontal(|ui| {
                                                    if md_button(ui, Md::H4, "Título 4").clicked() { md_action = Some(Md::H4); }
                                                    if md_button(ui, Md::H5, "Título 5").clicked() { md_action = Some(Md::H5); }
                                                    if md_button(ui, Md::H6, "Título 6").clicked() { md_action = Some(Md::H6); }
                                                });
                                            });
                                            ui.separator();
                                            // Estilo de texto.
                                            if md_button(ui, Md::Bold, "Negrita").clicked() { md_action = Some(Md::Bold); }
                                            if md_button(ui, Md::Italic, "Cursiva").clicked() { md_action = Some(Md::Italic); }
                                            if md_button(ui, Md::Strike, "Tachado").clicked() { md_action = Some(Md::Strike); }
                                            if md_button(ui, Md::Underline, "Subrayado").clicked() { md_action = Some(Md::Underline); }
                                            if md_button(ui, Md::Highlight, "Resaltar").clicked() { md_action = Some(Md::Highlight); }
                                            if md_button(ui, Md::Code, "Código").clicked() { md_action = Some(Md::Code); }
                                            if md_button(ui, Md::CodeBlock, "Bloque de código").clicked() { md_action = Some(Md::CodeBlock); }
                                            ui.separator();
                                            // Estructura.
                                            if md_button(ui, Md::Link, "Enlace").clicked() { md_action = Some(Md::Link); }
                                            if md_button(ui, Md::Image, "Imagen").clicked() { md_action = Some(Md::Image); }
                                            // Tabla: clic = insertar; al pasar el cursor se despliega
                                            // un control del TAMANO (ancho) de las tablas.
                                            let table_resp = md_button(ui, Md::Table, "Tabla — pasa el cursor para el tamaño");
                                            if table_resp.clicked() { md_action = Some(Md::Table); }
                                            {
                                                let pop_id = egui::Id::new("tbl_size_pop");
                                                let was_open = ui.memory(|m| m.data.get_temp::<bool>(pop_id).unwrap_or(false));
                                                if table_resp.hovered() || was_open {
                                                    // Panel PEGADO al boton (sin hueco) para que el cursor pueda
                                                    // pasar del boton al panel sin que se cierre.
                                                    let area = egui::Area::new(pop_id)
                                                        .order(egui::Order::Foreground)
                                                        .fixed_pos(table_resp.rect.left_bottom())
                                                        .show(ui.ctx(), |ui| {
                                                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                                                ui.set_width(200.0);
                                                                ui.label(egui::RichText::new("Tamaño de la NUEVA tabla").size(12.0).strong());
                                                                ui.add_space(2.0);
                                                                ui.label(egui::RichText::new("Ancho de columna").size(11.0));
                                                                ui.add(egui::Slider::new(&mut table_scale, 0.0..=1.0).show_value(false));
                                                                ui.label(egui::RichText::new("Alto de fila").size(11.0));
                                                                ui.add(egui::Slider::new(&mut table_row, 0.6..=2.5).show_value(false));
                                                                ui.label(egui::RichText::new("Se aplica solo a la próxima tabla que insertes.").size(10.0).weak());
                                                            });
                                                        });
                                                    // Mantener abierto si el cursor esta sobre el boton, el panel o
                                                    // el espacio entre ambos; o si se esta arrastrando el slider.
                                                    let union = table_resp.rect.union(area.response.rect).expand(6.0);
                                                    let pp = ui.input(|i| i.pointer.hover_pos());
                                                    let dragging = ui.input(|i| i.pointer.any_down());
                                                    let keep = pp.map_or(false, |p| union.contains(p)) || (was_open && dragging);
                                                    ui.memory_mut(|m| m.data.insert_temp(pop_id, keep));
                                                } else {
                                                    ui.memory_mut(|m| m.data.insert_temp(pop_id, false));
                                                }
                                            }
                                            if md_button(ui, Md::Task, "Tarea").clicked() { md_action = Some(Md::Task); }
                                            if md_button(ui, Md::Quote, "Cita").clicked() { md_action = Some(Md::Quote); }
                                            if md_button(ui, Md::Callout, "Callout").clicked() { md_action = Some(Md::Callout); }
                                            ui.separator();
                                            // Listas y sangría.
                                            if md_button(ui, Md::Bullet, "Lista").clicked() { md_action = Some(Md::Bullet); }
                                            if md_button(ui, Md::Number, "Lista numerada").clicked() { md_action = Some(Md::Number); }
                                            if md_button(ui, Md::Indent, "Sangrar").clicked() { md_action = Some(Md::Indent); }
                                            if md_button(ui, Md::Outdent, "Quitar sangría").clicked() { md_action = Some(Md::Outdent); }
                                            // Alineación (desplegable).
                                            let ral = md_button(ui, Md::Align, "Alineación");
                                            let pal = ui.make_persistent_id("md_align_pop");
                                            if ral.clicked() { ui.memory_mut(|m| m.toggle_popup(pal)); }
                                            #[allow(deprecated)]
                                            egui::popup_below_widget(ui, pal, &ral, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                                                ui.horizontal(|ui| {
                                                    if md_button(ui, Md::AlignLeft, "Izquierda").clicked() { md_align = Some(0); }
                                                    if md_button(ui, Md::AlignCenter, "Centro").clicked() { md_align = Some(1); }
                                                    if md_button(ui, Md::AlignRight, "Derecha").clicked() { md_align = Some(2); }
                                                    if md_button(ui, Md::AlignJustify, "Justificado").clicked() { md_align = Some(3); }
                                                });
                                            });
                                            ui.separator();
                                            // Color de texto (con paleta emergente).
                                            let rc = md_button(ui, Md::FontColor, "Color de texto");
                                            let pc = ui.make_persistent_id("md_fontcolor_pop");
                                            if rc.clicked() { ui.memory_mut(|m| m.toggle_popup(pc)); }
                                            #[allow(deprecated)]
                                            egui::popup_below_widget(ui, pc, &rc, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                                                if let Some(rgb) = color_palette(ui) { md_color = Some((rgb, false)); }
                                            });
                                            // Color de resaltado (con paleta emergente).
                                            let rh = md_button(ui, Md::HighlightColor, "Color de resaltado");
                                            let ph = ui.make_persistent_id("md_hicolor_pop");
                                            if rh.clicked() { ui.memory_mut(|m| m.toggle_popup(ph)); }
                                            #[allow(deprecated)]
                                            egui::popup_below_widget(ui, ph, &rh, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                                                if let Some(rgb) = color_palette(ui) { md_color = Some((rgb, true)); }
                                            });
                                            ui.separator();
                                            if md_button(ui, Md::Hr, "Línea horizontal").clicked() { md_action = Some(Md::Hr); }
                                            if md_button(ui, Md::Fullscreen, "Pantalla completa").clicked() { toggle_fullscreen = true; }
                                        });
                                    });
                                });
                        }
                    }
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
                                        ui.separator();
                                        // Modo DOCUMENTO: escribir con teclado + diseño de pagina.
                                        // Icono de lapiz VECTORIAL + texto (el glifo "✍" no existe
                                        // en la fuente -> salia un cuadro).
                                        if doc_pencil_button(ui, self.write_mode).clicked() {
                                            toggle_write = true;
                                        }
                                        if ui.button("Página…").clicked() {
                                            toggle_setup = true;
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
                    // ---------------- BIBLIOTECA (v2): barra lateral + area de cuadrícula ----------------
                    let th = home_theme(self.lib_tweaks.theme);
                    // BARRA LATERAL izquierda: kicker + titulo + botones + archiveros (solo en grid).
                    if self.preview_idx.is_none() && !self.creating_nb {
                        egui::SidePanel::left("lib_sidebar")
                            .resizable(false)
                            .exact_width(340.0)
                            .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 36, right: 22, top: 24, bottom: 16 }))
                            .show(ctx, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.add(egui::Label::new(egui::RichText::new("INK · ").font(egui::FontId::new(11.0, egui::FontFamily::Monospace)).color(th.kicker)).selectable(false));
                                    ui.add(egui::Label::new(egui::RichText::new("BIBLIOTECA").font(egui::FontId::new(11.0, egui::FontFamily::Monospace)).color(th.kicker_b)).selectable(false));
                                });
                                ui.add_space(6.0);
                                let tf = if th.serif { egui::FontId::new(33.0, egui::FontFamily::Name("serif".into())) } else { egui::FontId::new(31.0, egui::FontFamily::Name("head".into())) };
                                ui.add(egui::Label::new(egui::RichText::new("Mis cuadernos").font(tf).color(th.title)).selectable(false));
                                let sub = if th.serif { egui::RichText::new("Tu biblioteca de cuadernos y notas").font(egui::FontId::new(14.0, egui::FontFamily::Name("serif_it".into()))).color(th.sub) } else { egui::RichText::new("Tu biblioteca de cuadernos y notas").size(13.0).color(th.sub) };
                                ui.add(egui::Label::new(sub).selectable(false));
                                ui.add_space(20.0);
                                let fw = ui.available_width();
                                if pill_button(ui, "Nuevo cuaderno", BtnIcon::Plus, th.primary, fw).clicked() { lib_open_new = true; }
                                ui.add_space(9.0);
                                if pill_button(ui, "Nota rápida", BtnIcon::Note, th.secondary, fw).clicked() { lib_quick_note = true; }
                                ui.add_space(22.0);
                                ui.horizontal(|ui| {
                                    ui.add(egui::Label::new(egui::RichText::new("ARCHIVEROS").font(egui::FontId::new(10.5, egui::FontFamily::Monospace)).color(th.kicker)).selectable(false));
                                });
                                ui.add_space(7.0);
                                // Registrar las zonas de soltar (px fisicos) para asignar cuadernos por arrastre.
                                self.archivero_drop.clear();
                                let ppp_s = ui.ctx().pixels_per_point();
                                // Estado de arrastre de un cuaderno: para iluminar el archivero destino.
                                let nb_dragging = self.dragging && self.drag_idx.is_some();
                                let cursor_pts = egui::pos2(self.cursor.x / ppp_s, self.cursor.y / ppp_s);
                                let pulse = (self.clock * 4.0).sin() * 0.5 + 0.5;
                                let r0 = archivero_row(ui, "Todos", self.archivero_count(""), self.active_archivero.is_empty(), &th, nb_dragging, cursor_pts, pulse);
                                self.archivero_drop.push((r0.rect.min.x * ppp_s, r0.rect.min.y * ppp_s, r0.rect.max.x * ppp_s, r0.rect.max.y * ppp_s, String::new()));
                                if r0.clicked() { lib_set_archivero = Some(String::new()); }
                                for a in self.archiveros.clone() {
                                    let r = archivero_row(ui, &a, self.archivero_count(&a), self.active_archivero == a, &th, nb_dragging, cursor_pts, pulse);
                                    self.archivero_drop.push((r.rect.min.x * ppp_s, r.rect.min.y * ppp_s, r.rect.max.x * ppp_s, r.rect.max.y * ppp_s, a.clone()));
                                    if r.clicked() { lib_set_archivero = Some(a.clone()); }
                                }
                                ui.add_space(4.0);
                                if self.creating_archivero {
                                    let r = ui.add(egui::TextEdit::singleline(&mut self.new_archivero_buf).hint_text("Nombre…").desired_width(fw));
                                    r.request_focus();
                                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) { lib_create_archivero = true; }
                                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { self.creating_archivero = false; self.new_archivero_buf.clear(); }
                                } else if add_action_row(ui, "Nuevo archivero", &th).clicked() {
                                    lib_new_archivero = true;
                                }
                            });
                    }
                    // ---------------- AREA DERECHA (cuadrícula) ----------------
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(40, 26)))
                        .show(ctx, |ui| {
                        // VISTA PREVIA (Alt+clic): el LIBRO grande lo dibuja wgpu (izquierda); aqui
                        // dibujamos la PAGINA con su contenido (a la derecha) que "se abre".
                        if self.preview_idx.is_some() {
                            let ppp = ctx.pixels_per_point().max(0.01);
                            let vw = self.camera.viewport.x / ppp;
                            let vh = self.camera.viewport.y / ppp;
                            let tt = self.preview_t.clamp(0.0, 1.0);
                            let zoom = tt * tt * (3.0 - 2.0 * tt); // acercarse (suave)
                            let (fc, fh) = self.preview_from; // carta de origen (px fisicos)
                            let lerp = |a: f32, b: f32| a + (b - a) * zoom;
                            let kind = self.preview_grid_kind();
                            // Tapa (color segun acento) y si es de espiral, para la encuadernacion.
                            let (cover_col, is_spiral) = self
                                .preview_idx
                                .and_then(|i| self.notebooks.get(i))
                                .map(|n| {
                                    let a = accent_color(n.accent);
                                    let cv = |c: f32| ((0.10 + c * 0.20) * 255.0) as u8;
                                    (egui::Color32::from_rgb(cv(a[0]), cv(a[1]), cv(a[2])), n.shape == 3)
                                })
                                .unwrap_or((egui::Color32::from_rgb(30, 28, 34), false));
                            // LIBRETA ABIERTA: doble pagina centrada, que crece desde su carta.
                            let ph_t = (vh * 0.62).clamp(180.0, 880.0);
                            let pw_t = ph_t * 0.74;
                            let g_t = ph_t * 0.02;
                            let cur_h = lerp(fh.y * 2.0 / ppp, ph_t);
                            let scale = (cur_h / ph_t).max(0.001);
                            let cur_pw = pw_t * scale;
                            let cur_g = g_t * scale;
                            let cx = lerp(fc.x / ppp, vw * 0.5);
                            let cy = lerp(fc.y / ppp, vh * 0.52);
                            let hh2 = cur_h * 0.5;
                            let lpage = egui::Rect::from_min_max(
                                egui::pos2(cx - cur_pw - cur_g * 0.5, cy - hh2),
                                egui::pos2(cx - cur_g * 0.5, cy + hh2),
                            );
                            let rpage = egui::Rect::from_min_max(
                                egui::pos2(cx + cur_g * 0.5, cy - hh2),
                                egui::pos2(cx + cur_g * 0.5 + cur_pw, cy + hh2),
                            );
                            let spread = egui::Rect::from_min_max(lpage.min, rpage.max);
                            let border = 11.0 * scale;
                            let painter = ui.painter().clone();
                            // Sombra del libro abierto.
                            painter.rect_filled(
                                spread.expand(border).translate(egui::vec2(0.0, 12.0 * scale)),
                                10.0,
                                egui::Color32::from_black_alpha(90),
                            );
                            // Tapa (borde de material alrededor de las hojas).
                            painter.rect_filled(spread.expand(border), 8.0, cover_col);
                            // Hojas (papel crema) con su CUADRICULA.
                            let paper = egui::Color32::from_rgb(248, 247, 243);
                            painter.rect_filled(lpage, 2.0, paper);
                            painter.rect_filled(rpage, 2.0, paper);
                            Self::draw_preview_grid(&painter, lpage, kind);
                            Self::draw_preview_grid(&painter, rpage, kind);
                            // Contenido: pagina ACTUAL a la derecha; la anterior a la izquierda.
                            let cur = self.preview_page;
                            if let Some(pg) = self.preview_pages.get(cur) {
                                Self::draw_preview_page(&painter, rpage, pg);
                            }
                            if cur > 0 {
                                if let Some(pg) = self.preview_pages.get(cur - 1) {
                                    Self::draw_preview_page(&painter, lpage, pg);
                                }
                            }
                            // Encuadernacion central: sombra del lomo + espiral si corresponde.
                            painter.rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(cx - cur_g * 0.5, cy - hh2),
                                    egui::pos2(cx + cur_g * 0.5, cy + hh2),
                                ),
                                0.0,
                                egui::Color32::from_black_alpha(55),
                            );
                            if is_spiral {
                                let rings = (cur_h / (22.0 * scale)).clamp(6.0, 40.0) as i32;
                                for k in 0..rings {
                                    let y = cy - hh2 + (k as f32 + 0.5) / rings as f32 * cur_h;
                                    painter.circle_stroke(
                                        egui::pos2(cx, y),
                                        cur_g * 0.9,
                                        egui::Stroke::new(2.0 * scale, egui::Color32::from_rgb(200, 205, 212)),
                                    );
                                }
                            }
                            // Nombre arriba, pie abajo.
                            painter.text(
                                egui::pos2(cx, cy - hh2 - border - 16.0),
                                egui::Align2::CENTER_CENTER,
                                &self.preview_name,
                                egui::FontId::proportional(20.0),
                                egui::Color32::from_gray(235),
                            );
                            let npages = self.preview_pages.len();
                            let foot = if npages > 1 {
                                format!(
                                    "Pág {} / {}   ·   rueda o ← → para pasar   ·   Esc o clic para cerrar",
                                    cur + 1, npages
                                )
                            } else {
                                "Esc o clic para cerrar".to_string()
                            };
                            painter.text(
                                egui::pos2(cx, cy + hh2 + border + 16.0),
                                egui::Align2::CENTER_CENTER,
                                foot,
                                egui::FontId::proportional(13.0),
                                egui::Color32::from_gray(150),
                            );
                            ui.input(|i| {
                                if i.key_pressed(egui::Key::Escape) {
                                    lib_close_preview = true;
                                }
                                if i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::ArrowDown) {
                                    lib_preview_next = true;
                                }
                                if i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowUp) {
                                    lib_preview_prev = true;
                                }
                            });
                            return;
                        }
                        // --- Barra superior del area derecha: titulo de seccion + buscador + pestañas ---
                        if !self.creating_nb {
                            ui.add_space(2.0);
                            let count = self.visible_notebooks().len();
                            ui.horizontal(|ui| {
                                let stitle = if self.active_archivero.is_empty() { "Todos".to_string() } else { self.active_archivero.clone() };
                                let sf = if th.serif { egui::FontId::new(23.0, egui::FontFamily::Name("serif".into())) } else { egui::FontId::new(21.0, egui::FontFamily::Name("head".into())) };
                                ui.add(egui::Label::new(egui::RichText::new(stitle).font(sf).color(th.title)).selectable(false));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.add_space(64.0); // hueco para la papelera (esquina sup. der.)
                                    if icon_btn(ui, BtnIcon::Sliders, th.icon_btn).clicked() {
                                        lib_toggle_tweaks = true;
                                    }
                                    ui.add_space(12.0);
                                    ui.add(egui::Label::new(egui::RichText::new(format!("{count} piezas")).font(egui::FontId::new(11.5, egui::FontFamily::Monospace)).color(th.sub)).selectable(false));
                                    ui.add_space(12.0);
                                    egui::Frame::NONE
                                        .fill(th.search_fill)
                                        .stroke(egui::Stroke::new(1.0, th.sep))
                                        .corner_radius(9)
                                        .inner_margin(egui::Margin::symmetric(12, 7))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                search_icon(ui, th.sub);
                                                ui.add(egui::TextEdit::singleline(&mut self.search_query).hint_text("Buscar…").frame(egui::Frame::NONE).desired_width(190.0).text_color(th.title));
                                            });
                                        });
                                });
                            });
                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 24.0;
                                for (i, t) in ["RECIENTES", "A — Z", "CUADERNOS", "NOTAS"].iter().enumerate() {
                                    let on = self.sort_mode == i as u32;
                                    let lbl = egui::Label::new(egui::RichText::new(*t).font(egui::FontId::new(11.5, egui::FontFamily::Monospace)).color(if on { th.title } else { th.kicker })).sense(egui::Sense::click());
                                    if ui.add(lbl).clicked() {
                                        lib_set_sort = Some(i as u32);
                                    }
                                }
                            });
                            ui.add_space(10.0);
                            if nb_list.is_empty() {
                                ui.label(
                                    egui::RichText::new("Aún no tienes cuadernos. Pulsa «Nuevo cuaderno».")
                                        .italics()
                                        .color(th.sub),
                                );
                            }
                            // Las CARTAS se dibujan con wgpu detras de la UI; aqui solo va, bajo
                            // cada carta, su NOMBRE. El clic/arrastre se maneja por hit-test.
                            let ppp = ctx.pixels_per_point().max(0.01);
                            let lp = ctx.layer_painter(egui::LayerId::new(
                                egui::Order::Foreground,
                                egui::Id::new("card_overlay"),
                            ));
                            for (i, (name, _inf, _path)) in nb_list.iter().enumerate() {
                                // La carta que se arrastra va al cursor: no dibujar su nombre fijo.
                                if self.dragging && self.drag_idx == Some(i) {
                                    continue;
                                }
                                // El nombre que se esta EDITANDO se dibuja como TextEdit (abajo).
                                if self.renaming == Some(i) {
                                    continue;
                                }
                                let Some((c, h)) = card_layout.get(i) else { continue };
                                let fl = self.card_flip.get(i).copied().unwrap_or(0.0);
                                // Las notas rapidas (hojas, finish 500..599) no tienen cinta: su
                                // nombre va siempre DEBAJO, aunque se volteen.
                                let is_sheet = self
                                    .notebooks
                                    .get(i)
                                    .map_or(false, |n| n.finish >= 500 && n.finish < 600);
                                if fl > 1.5708 && !is_sheet {
                                    // Reverso visible: el nombre va PEGADO a la cinta y ROTADO CON
                                    // ELLA (sigue su inclinacion/giro/flotacion con precision).
                                    // Proyecto el centro y los dos extremos del eje horizontal del
                                    // reverso con la MISMA pose que el shader (incluida la flotacion)
                                    // y dibujo el texto rotado al angulo de la cinta en pantalla.
                                    let a = self.card_anim.get(i).copied().unwrap_or([0.0; 3]);
                                    let nbk = self.notebooks.get(i);
                                    let (df, _, _) = shape_params(nbk.map_or(0, |n| n.shape));
                                    let hz = h.x * df * nbk.map_or(1.0, |n| n.thickness);
                                    let hv = a[0];
                                    // Misma flotacion que en build_card_instances (para no despegar).
                                    let t = self.clock;
                                    let ph = i as f32 * 1.7;
                                    let rx = a[1] + (t * 1.5 + ph).sin() * 0.055 * hv;
                                    let ry = a[2] + fl + (t * 1.2 + ph * 1.3).cos() * 0.065 * hv;
                                    let cby = c.y - (t * 1.3 + ph).sin() * 11.0 * hv;
                                    let cc = vec2(c.x, cby);
                                    let (cxs, cys, fac) = project_card_point(cc, rx, ry, hv, 0.0, 0.0, -hz);
                                    // Extremos del eje horizontal de la cinta (~0.7 del semiancho).
                                    let (pax, pay, _) = project_card_point(cc, rx, ry, hv, 0.7 * h.x, 0.0, -hz);
                                    let (pbx, pby, _) = project_card_point(cc, rx, ry, hv, -0.7 * h.x, 0.0, -hz);
                                    // Izquierda/derecha EN PANTALLA (para que el texto se lea bien
                                    // aunque el reverso quede espejado al voltear).
                                    let (lx_, ly_, rx2, ry2) = if pax <= pbx {
                                        (pax, pay, pbx, pby)
                                    } else {
                                        (pbx, pby, pax, pay)
                                    };
                                    let angle = (ry2 - ly_).atan2(rx2 - lx_);
                                    let tape_px = (rx2 - lx_).hypot(ry2 - ly_).max(20.0);
                                    let tape_w = tape_px / ppp;
                                    // Tamaño que CABE en la cinta; si al minimo no cabe, recorta.
                                    let len = name.chars().count().max(1) as f32;
                                    let base = 15.0 * fac.clamp(0.85, 1.25);
                                    let est_w = len * base * 0.55;
                                    let size = if est_w > tape_w { (tape_w / (len * 0.55)).max(7.5) } else { base };
                                    let max_chars = (tape_w / (size * 0.55)).floor() as usize;
                                    let shown = if name.chars().count() > max_chars && max_chars >= 2 {
                                        let mut s: String = name.chars().take(max_chars.saturating_sub(1)).collect();
                                        s.push('…');
                                        s
                                    } else {
                                        name.clone()
                                    };
                                    // Galley + TextShape ROTADO, centrado en el centro de la cinta.
                                    let col = egui::Color32::from_rgb(55, 45, 30);
                                    let galley =
                                        lp.layout_no_wrap(shown, egui::FontId::proportional(size), col);
                                    let sz = galley.size();
                                    let rot = egui::emath::Rot2::from_angle(angle);
                                    let center_pt = egui::pos2(cxs / ppp, cys / ppp);
                                    let pos = center_pt - rot * egui::vec2(sz.x * 0.5, sz.y * 0.5);
                                    lp.add(egui::epaint::TextShape::new(pos, galley, col).with_angle(angle));
                                } else {
                                    // Nombre BAJO la carta: hasta 3 renglones (ajustados), un poco
                                    // mas abajo para que no lo tape el cuaderno al girar/levantar.
                                    let size = 13.0_f32;
                                    let card_w_pts = 2.0 * h.x / ppp;
                                    let max_chars = (card_w_pts / (size * 0.55)).floor().max(4.0) as usize;
                                    let lines = wrap_name(name, max_chars, 3);
                                    let line_h = size * 1.18; // renglones juntos
                                    let top_y = (c.y + h.y + 28.0) / ppp;
                                    for (li, ln) in lines.iter().enumerate() {
                                        lp.text(
                                            egui::pos2(c.x / ppp, top_y + li as f32 * line_h),
                                            egui::Align2::CENTER_TOP,
                                            ln,
                                            egui::FontId::proportional(size),
                                            th.name,
                                        );
                                    }
                                    // KINDTAG: forma del cuaderno (o "NOTA RÁPIDA") en monoespaciada.
                                    if let Some(nbk) = self.notebooks.get(i) {
                                        let ktag = if nbk.finish >= 500 && nbk.finish < 600 {
                                            "NOTA RÁPIDA".to_string()
                                        } else {
                                            SHAPE_NAMES.get(nbk.shape as usize).copied().unwrap_or("").to_uppercase()
                                        };
                                        lp.text(
                                            egui::pos2(c.x / ppp, top_y + lines.len() as f32 * line_h + 5.0),
                                            egui::Align2::CENTER_TOP,
                                            ktag,
                                            egui::FontId::new(9.5, egui::FontFamily::Monospace),
                                            th.kicker,
                                        );
                                    }
                                    // Zona CLICABLE (invisible) de egui sobre TODO el nombre (los 3
                                    // renglones) para renombrar: el clic siempre se detecta aqui.
                                    let nh = 3.0 * line_h + 14.0;
                                    let tl = egui::pos2((c.x - h.x) / ppp, (c.y + h.y + 22.0) / ppp);
                                    egui::Area::new(egui::Id::new(("nmhit", i)))
                                        .order(egui::Order::Foreground)
                                        .fixed_pos(tl)
                                        .show(ctx, |ui| {
                                            let (_r, resp) = ui.allocate_exact_size(
                                                egui::vec2(card_w_pts, nh),
                                                egui::Sense::click(),
                                            );
                                            if resp.clicked() {
                                                lib_start_rename = Some(i);
                                            }
                                        });
                                }
                            }
                            // PAPELERA unica: arrastra una carta aqui (y suelta) para borrarla.
                            let (tzx, tzy, tzr) = self.trash_zone();
                            let drag_on = self.dragging && self.drag_idx.is_some();
                            let over = drag_on && self.over_trash_zone();
                            let tcol = if over {
                                egui::Color32::from_rgb(245, 90, 90)
                            } else if drag_on {
                                egui::Color32::from_rgb(225, 130, 130)
                            } else {
                                th.icon_btn
                            };
                            let tc = egui::pos2(tzx / ppp, tzy / ppp);
                            if drag_on {
                                lp.circle_stroke(tc, tzr / ppp, egui::Stroke::new(2.0, tcol));
                            }
                            let s = 1.8_f32;
                            let st = egui::Stroke::new(2.2, tcol);
                            let body = egui::Rect::from_min_max(
                                tc + egui::vec2(-6.5 * s, -2.0 * s),
                                tc + egui::vec2(6.5 * s, 9.5 * s),
                            );
                            lp.rect_stroke(body, egui::CornerRadius::same(2), st, egui::StrokeKind::Inside);
                            lp.line_segment([tc + egui::vec2(-9.0 * s, -2.0 * s), tc + egui::vec2(9.0 * s, -2.0 * s)], st);
                            lp.line_segment([tc + egui::vec2(-3.5 * s, -5.0 * s), tc + egui::vec2(3.5 * s, -5.0 * s)], st);
                            lp.line_segment([tc + egui::vec2(-3.5 * s, -5.0 * s), tc + egui::vec2(-3.5 * s, -2.0 * s)], st);
                            lp.line_segment([tc + egui::vec2(3.5 * s, -5.0 * s), tc + egui::vec2(3.5 * s, -2.0 * s)], st);
                            lp.line_segment([tc + egui::vec2(-2.5 * s, 0.5 * s), tc + egui::vec2(-2.5 * s, 7.0 * s)], st);
                            lp.line_segment([tc + egui::vec2(0.0, 0.5 * s), tc + egui::vec2(0.0, 7.0 * s)], st);
                            lp.line_segment([tc + egui::vec2(2.5 * s, 0.5 * s), tc + egui::vec2(2.5 * s, 7.0 * s)], st);
                            lp.text(
                                egui::pos2(tzx / ppp, (tzy + tzr + 6.0) / ppp),
                                egui::Align2::CENTER_TOP,
                                if drag_on { "Soltar para borrar" } else { "Borrar" },
                                egui::FontId::proportional(12.0),
                                tcol,
                            );
                            // CONMUTADOR de tema (abajo-centro): Tinta / Cuaderno.
                            egui::Area::new(egui::Id::new("theme_switch"))
                                .order(egui::Order::Foreground)
                                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
                                .show(ctx, |ui| {
                                    egui::Frame::NONE
                                        .fill(egui::Color32::from_rgba_unmultiplied(20, 18, 26, 235))
                                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(44, 41, 54)))
                                        .corner_radius(12)
                                        .inner_margin(egui::Margin::same(5))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.spacing_mut().item_spacing.x = 3.0;
                                                for (idx, name) in ["Tinta", "Cuaderno"].iter().enumerate() {
                                                    let on = self.lib_tweaks.theme == idx as u32;
                                                    let txt = egui::RichText::new(*name)
                                                        .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                                                        .color(if on { egui::Color32::from_rgb(22, 19, 28) } else { egui::Color32::from_gray(154) });
                                                    let b = egui::Button::new(txt)
                                                        .fill(if on { egui::Color32::from_rgb(202, 191, 167) } else { egui::Color32::TRANSPARENT })
                                                        .corner_radius(8)
                                                        .min_size(egui::vec2(78.0, 30.0));
                                                    if ui.add(b).clicked() {
                                                        lib_set_theme = Some(idx as u32);
                                                    }
                                                }
                                            });
                                        });
                                });
                            // RENOMBRAR en linea: caja de texto centrada sobre el nombre elegido.
                            if let Some(ri) = self.renaming {
                                if let Some((c, h)) = card_layout.get(ri) {
                                    let card_w_pts = 2.0 * h.x / ppp;
                                    let w = card_w_pts.clamp(70.0, 240.0);
                                    // Limite de caracteres = lo que cabe en 3 renglones.
                                    let per_line = (card_w_pts / (13.0 * 0.55)).floor().max(4.0) as usize;
                                    let char_limit = (per_line * 3).max(12);
                                    let pos = egui::pos2(c.x / ppp, (c.y + h.y + 22.0) / ppp);
                                    egui::Area::new(egui::Id::new("rename_edit"))
                                        .order(egui::Order::Foreground)
                                        .fixed_pos(pos)
                                        .pivot(egui::Align2::CENTER_TOP)
                                        .show(ctx, |ui| {
                                            let resp = ui.add(
                                                egui::TextEdit::singleline(&mut self.rename_buf)
                                                    .desired_width(w)
                                                    .char_limit(char_limit)
                                                    .horizontal_align(egui::Align::Center),
                                            );
                                            // Margen de gracia: durante unos frames forzamos el
                                            // foco para que el "soltar" del clic que inicio el
                                            // renombrado (aunque sea en el 2º/3er renglon, fuera
                                            // del cuadro) NO lo cierre al instante.
                                            if self.rename_grace > 0 {
                                                resp.request_focus();
                                                self.rename_grace -= 1;
                                            } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                                lib_cancel_rename = true;
                                            } else if resp.lost_focus() {
                                                lib_rename = Some(ri);
                                            }
                                        });
                                }
                            }
                            // MENU al soltar una HOJA sobre un cuaderno: cambiar posicion o guardar dentro.
                            if let Some((from, target)) = self.merge_prompt {
                                let note_name =
                                    self.notebooks.get(from).map(|n| n.name.clone()).unwrap_or_default();
                                let book_name =
                                    self.notebooks.get(target).map(|n| n.name.clone()).unwrap_or_default();
                                egui::Window::new(egui::RichText::new("¿Qué hago con la nota?").strong())
                                    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                                    .collapsible(false)
                                    .resizable(false)
                                    .show(ctx, |ui| {
                                        ui.label(format!("Nota: «{note_name}»"));
                                        ui.label(format!("Cuaderno: «{book_name}»"));
                                        ui.add_space(8.0);
                                        if ui
                                            .button(egui::RichText::new("📒  Guardar dentro de este cuaderno").strong())
                                            .clicked()
                                        {
                                            lib_merge_save = true;
                                        }
                                        ui.label(
                                            egui::RichText::new("La nota se añade al final de sus hojas.")
                                                .small()
                                                .color(egui::Color32::from_gray(150)),
                                        );
                                        ui.add_space(6.0);
                                        if ui.button("🔀  Cambiar de posición").clicked() {
                                            lib_merge_swap = true;
                                        }
                                        ui.label(
                                            egui::RichText::new("Intercambia su lugar con el del cuaderno.")
                                                .small()
                                                .color(egui::Color32::from_gray(150)),
                                        );
                                        ui.add_space(8.0);
                                        if ui.button("Cancelar").clicked() {
                                            lib_merge_cancel = true;
                                        }
                                    });
                            }
                        } else {
                            ui.label(
                                egui::RichText::new("Vista previa de la carátula a la izquierda; ajústala en el panel de la derecha.")
                                    .italics()
                                    .color(egui::Color32::from_gray(150)),
                            );
                        }

                        // Panel de CREACION (ventana a la derecha): la carta de vista previa la
                        // dibuja wgpu a la izquierda; aqui van las opciones de la carátula.
                        if self.creating_nb {
                            let gray = egui::Color32::from_gray(180);
                            let editing = self.editing_nb.is_some();
                            let title = if editing { "Editar carátula" } else { "Nueva carátula" };
                            egui::Window::new(egui::RichText::new(title).strong())
                                .anchor(egui::Align2::RIGHT_CENTER, egui::vec2(-48.0, 0.0))
                                .collapsible(false)
                                .resizable(false)
                                .default_width(380.0)
                                .show(ctx, |ui| {
                                    ui.add_space(2.0);
                                    // Nombre y tipo: solo al CREAR (al editar no se renombra ni cambia el tipo).
                                    if !editing {
                                        ui.horizontal(|ui| {
                                            ui.label("Nombre:");
                                            ui.add(
                                                egui::TextEdit::singleline(&mut self.new_nb_name)
                                                    .hint_text("Mi cuaderno")
                                                    .desired_width(240.0),
                                            );
                                        });
                                        ui.horizontal(|ui| {
                                            ui.selectable_value(&mut self.new_nb_infinite, true, "Lienzo infinito");
                                            ui.selectable_value(&mut self.new_nb_infinite, false, "Cuaderno de hojas");
                                        });
                                        ui.add_space(6.0);
                                        ui.separator();
                                    }
                                    ui.label(egui::RichText::new("Diseño base").strong());
                                    egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, false]).show(ui, |ui| {
                                        ui.label(egui::RichText::new("Foil").color(gray));
                                        ui.horizontal_wrapped(|ui| {
                                            for (id, name) in FOIL_DESIGNS {
                                                if ui.selectable_label(self.new_nb_finish == id, name).clicked() {
                                                    self.new_nb_finish = id;
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);
                                        ui.label(egui::RichText::new("Cargadores animados").color(gray));
                                        ui.horizontal_wrapped(|ui| {
                                            for (id, name) in LOADER_DESIGNS {
                                                if ui.selectable_label(self.new_nb_finish == id, name).clicked() {
                                                    self.new_nb_finish = id;
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);
                                        ui.label(egui::RichText::new("Cargadores 3D").color(gray));
                                        ui.horizontal_wrapped(|ui| {
                                            for (id, name) in LOADER3D_DESIGNS {
                                                if ui.selectable_label(self.new_nb_finish == id, name).clicked() {
                                                    self.new_nb_finish = id;
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);
                                        ui.label(egui::RichText::new("Escenas 3D").color(gray));
                                        ui.horizontal_wrapped(|ui| {
                                            for (id, name) in SCENE_DESIGNS {
                                                if ui.selectable_label(self.new_nb_finish == id, name).clicked() {
                                                    self.new_nb_finish = id;
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);
                                        ui.label(egui::RichText::new("Arcade y demos").color(gray));
                                        ui.horizontal_wrapped(|ui| {
                                            for (id, name) in ARCADE_DESIGNS {
                                                if ui.selectable_label(self.new_nb_finish == id, name).clicked() {
                                                    self.new_nb_finish = id;
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);
                                        ui.label(egui::RichText::new("Figuras 3D (giran)").color(gray));
                                        ui.horizontal_wrapped(|ui| {
                                            for (id, name) in FIGURE_DESIGNS {
                                                if ui.selectable_label(self.new_nb_finish == id, name).clicked() {
                                                    self.new_nb_finish = id;
                                                }
                                            }
                                        });
                                    });
                                    ui.add_space(8.0);
                                    ui.separator();
                                        ui.label(egui::RichText::new("Color · gama COPIC").strong());
                                        ui.label(egui::RichText::new("Un color (o degradado) para TODO el cuaderno.").size(11.0).color(gray));
                                        let c32 = |c: [f32; 3]| egui::Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8);
                                        ui.horizontal_wrapped(|ui| {
                                            ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                                            for &hx in COPIC_SOLIDS {
                                                let rgb = hexf(hx);
                                                let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
                                                let sel = self.new_nb_finish == 800 && self.new_nb_cover_a == rgb;
                                                let p = ui.painter();
                                                p.rect_filled(rect, egui::CornerRadius::same(5), c32(rgb));
                                                if sel {
                                                    p.rect_stroke(rect, egui::CornerRadius::same(5), egui::Stroke::new(2.0, egui::Color32::WHITE), egui::StrokeKind::Outside);
                                                } else if resp.hovered() {
                                                    p.rect_stroke(rect, egui::CornerRadius::same(5), egui::Stroke::new(1.5, egui::Color32::from_gray(210)), egui::StrokeKind::Outside);
                                                }
                                                if resp.clicked() {
                                                    self.new_nb_finish = 800;
                                                    self.new_nb_cover_a = rgb;
                                                }
                                            }
                                        });
                                        ui.add_space(3.0);
                                        ui.label(egui::RichText::new("Degradados").size(11.0).color(gray));
                                        ui.horizontal_wrapped(|ui| {
                                            ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                                            for &(ha, hb) in COPIC_GRADS {
                                                let (a, b) = (hexf(ha), hexf(hb));
                                                let (rect, resp) = ui.allocate_exact_size(egui::vec2(40.0, 22.0), egui::Sense::click());
                                                let sel = self.new_nb_finish == 801 && self.new_nb_cover_a == a && self.new_nb_cover_b == b;
                                                let p = ui.painter();
                                                let mut mesh = egui::Mesh::default();
                                                let (ca, cb) = (c32(a), c32(b));
                                                mesh.colored_vertex(rect.left_top(), ca);
                                                mesh.colored_vertex(rect.left_bottom(), ca);
                                                mesh.colored_vertex(rect.right_top(), cb);
                                                mesh.colored_vertex(rect.right_bottom(), cb);
                                                mesh.add_triangle(0, 1, 2);
                                                mesh.add_triangle(2, 1, 3);
                                                p.add(mesh);
                                                if sel {
                                                    p.rect_stroke(rect, egui::CornerRadius::same(5), egui::Stroke::new(2.0, egui::Color32::WHITE), egui::StrokeKind::Outside);
                                                } else if resp.hovered() {
                                                    p.rect_stroke(rect, egui::CornerRadius::same(5), egui::Stroke::new(1.5, egui::Color32::from_gray(210)), egui::StrokeKind::Outside);
                                                }
                                                if resp.clicked() {
                                                    self.new_nb_finish = 801;
                                                    self.new_nb_cover_a = a;
                                                    self.new_nb_cover_b = b;
                                                }
                                            }
                                        });
                                    ui.add_space(6.0);
                                    ui.separator();
                                    ui.label(egui::RichText::new("Forma del cuaderno").strong());
                                    ui.horizontal_wrapped(|ui| {
                                        for (idx, name) in SHAPE_NAMES.iter().enumerate() {
                                            if ui.selectable_label(self.new_nb_shape == idx as u32, *name).clicked() {
                                                self.new_nb_shape = idx as u32;
                                            }
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        ui.add(egui::Slider::new(&mut self.new_nb_thickness, 0.4..=1.8));
                                        ui.label("Grosor");
                                    });
                                    ui.horizontal(|ui| {
                                        ui.add(egui::Slider::new(&mut self.new_nb_overhang, 0.0..=2.0));
                                        ui.label("Ceja de tapa");
                                    });
                                    ui.add_space(6.0);
                                    ui.separator();
                                    ui.label(egui::RichText::new("Textura del material").strong());
                                    ui.label(
                                        egui::RichText::new("Se aplica a la portada y a las figuras 3D.")
                                            .small()
                                            .color(gray),
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        for (idx, name) in TEXTURE_NAMES.iter().enumerate() {
                                            if ui.selectable_label(self.new_nb_texture == idx as u32, *name).clicked() {
                                                self.new_nb_texture = idx as u32;
                                            }
                                        }
                                    });
                                    ui.add_space(6.0);
                                    ui.separator();
                                    ui.label(egui::RichText::new("Capas (combinables)").strong());
                                    ui.horizontal_wrapped(|ui| {
                                        for (bit, name) in FX_LAYERS {
                                            let on = (self.new_nb_fx & bit) != 0;
                                            if ui.selectable_label(on, name).clicked() {
                                                self.new_nb_fx ^= bit;
                                            }
                                        }
                                    });
                                    pct_row(ui, "Intensidad", &mut self.new_nb_intensity);
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label("Acento:");
                                        for (idx, name) in ACCENTS.iter().enumerate() {
                                            if ui.selectable_label(self.new_nb_accent == idx as u32, *name).clicked() {
                                                self.new_nb_accent = idx as u32;
                                            }
                                        }
                                    });
                                    ui.add_space(10.0);
                                    ui.horizontal(|ui| {
                                        let confirm = if editing { "Guardar" } else { "Crear" };
                                        if ui.button(egui::RichText::new(confirm).strong()).clicked() {
                                            if editing { lib_save_cover = true; } else { lib_create = true; }
                                        }
                                        if ui.button("Cancelar").clicked() {
                                            lib_cancel_new = true;
                                        }
                                    });
                                });
                        }

                        // Panel de TWEAKS (ajustes GLOBALES): pose en el estante + interaccion.
                        if self.show_tweaks && !self.creating_nb {
                            egui::Window::new(egui::RichText::new("Tweaks").strong())
                                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 64.0))
                                .collapsible(false)
                                .resizable(false)
                                .default_width(250.0)
                                .show(ctx, |ui| {
                                    ui.label(egui::RichText::new("Pose en el estante").strong());
                                    if ui.add(egui::Slider::new(&mut self.lib_tweaks.giro, 0.0..=40.0).text("Giro (lomo)")).changed() {
                                        tweaks_save = true;
                                    }
                                    if ui.add(egui::Slider::new(&mut self.lib_tweaks.inclinacion, -4.0..=24.0).text("Inclinación")).changed() {
                                        tweaks_save = true;
                                    }
                                    ui.add_space(6.0);
                                    ui.separator();
                                    ui.label(egui::RichText::new("Interacción").strong());
                                    ui.horizontal_wrapped(|ui| {
                                        for (idx, name) in HOVER_NAMES.iter().enumerate() {
                                            if ui.selectable_label(self.lib_tweaks.hover == idx as u32, *name).clicked() {
                                                self.lib_tweaks.hover = idx as u32;
                                                tweaks_save = true;
                                            }
                                        }
                                    });
                                    ui.add_space(6.0);
                                    ui.separator();
                                    if ui.checkbox(&mut self.lib_tweaks.animate, "Portadas animadas").changed() {
                                        tweaks_save = true;
                                    }
                                    ui.add_space(8.0);
                                    if ui.button("Cerrar").clicked() {
                                        lib_close_tweaks = true;
                                    }
                                });
                        }
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
                if lib_toggle_tweaks {
                    self.show_tweaks = !self.show_tweaks;
                }
                if lib_close_tweaks {
                    self.show_tweaks = false;
                }
                if tweaks_save {
                    notebook::save_tweaks(&self.lib_tweaks);
                }
                if let Some(ri) = lib_start_rename {
                    if self.renaming.is_none() {
                        self.renaming = Some(ri);
                        self.rename_buf = self.notebooks.get(ri).map(|n| n.name.clone()).unwrap_or_default();
                        self.rename_grace = 5;
                    }
                }
                if let Some(ri) = lib_rename {
                    let name = self.rename_buf.clone();
                    self.rename_notebook(ri, &name);
                    self.renaming = None;
                }
                if lib_cancel_rename {
                    self.renaming = None;
                }
                if lib_open_new {
                    self.editing_nb = None;
                    self.creating_nb = true;
                    self.renaming = None;
                }
                if lib_quick_note {
                    self.new_quick_note();
                }
                if lib_close_preview {
                    self.close_preview();
                }
                if let Some(t) = lib_set_theme {
                    self.lib_tweaks.theme = t;
                    notebook::save_tweaks(&self.lib_tweaks);
                    self.apply_titlebar();
                }
                if let Some(a) = lib_set_archivero {
                    self.active_archivero = a;
                    self.card_scroll = 0.0;
                }
                if let Some(s) = lib_set_sort {
                    self.sort_mode = s;
                    self.card_scroll = 0.0;
                }
                if lib_new_archivero {
                    self.creating_archivero = true;
                    self.new_archivero_buf.clear();
                }
                if lib_create_archivero {
                    let name = self.new_archivero_buf.trim().to_string();
                    if !name.is_empty() && !self.archiveros.iter().any(|a| a == &name) {
                        self.archiveros.push(name.clone());
                        notebook::save_archiveros(&self.archiveros);
                        self.active_archivero = name;
                    }
                    self.new_archivero_buf.clear();
                    self.creating_archivero = false;
                }
                if lib_preview_next {
                    self.preview_flip(1);
                }
                if lib_preview_prev {
                    self.preview_flip(-1);
                }
                if let Some((from, target)) = self.merge_prompt {
                    if lib_merge_save {
                        self.merge_note_into(from, target);
                        self.merge_prompt = None;
                    } else if lib_merge_swap {
                        self.swap_cards(from, target);
                        self.merge_prompt = None;
                    } else if lib_merge_cancel {
                        self.merge_prompt = None;
                    }
                }
                if lib_cancel_new {
                    self.creating_nb = false;
                    self.editing_nb = None;
                }
                if lib_save_cover {
                    if let Some(path) = self.editing_nb.take() {
                        self.save_cover_edit(&path);
                    }
                    self.creating_nb = false;
                }
                if lib_create {
                    let name = self.new_nb_name.clone();
                    self.new_notebook(
                        &name,
                        self.new_nb_infinite,
                        self.new_nb_finish,
                        self.new_nb_fx,
                        self.new_nb_intensity,
                        self.new_nb_accent,
                    );
                    self.new_nb_name.clear();
                    self.creating_nb = false;
                    self.editing_nb = None;
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
                if toggle_write {
                    self.write_mode = !self.write_mode;
                    // Al escribir conviene fijar la hoja (que el pan/zoom no estorbe al teclear).
                    if self.write_mode {
                        self.lock_page = true;
                        self.center_on_page();
                    }
                }
                if toggle_setup {
                    self.show_page_setup = !self.show_page_setup;
                }
                // Tamano de la proxima tabla ajustado en el popup hover (defaults para tablas nuevas).
                if (table_scale - self.doc_layout.table_scale).abs() > 1e-4 {
                    self.doc_layout.table_scale = table_scale.clamp(0.0, 1.0);
                }
                if (table_row - self.doc_layout.table_row).abs() > 1e-4 {
                    self.doc_layout.table_row = table_row.clamp(0.5, 3.0);
                }
                if let Some(md) = md_action {
                    let c = self.egui_ctx.clone();
                    self.apply_md(&c, md);
                }
                if let Some((rgb, hl)) = md_color {
                    let c = self.egui_ctx.clone();
                    self.apply_color(&c, rgb, hl);
                }
                if let Some(a) = md_align {
                    self.page_align = a;
                    self.egui_ctx.memory_mut(|m| m.request_focus(egui::Id::new("doc_te")));
                }
                if doc_undo_flag {
                    let c = self.egui_ctx.clone();
                    self.doc_undo_op(&c);
                }
                if doc_redo_flag {
                    let c = self.egui_ctx.clone();
                    self.doc_redo_op(&c);
                }
                if toggle_fullscreen {
                    self.fullscreen = !self.fullscreen;
                    if let Some(w) = &self.window {
                        w.set_fullscreen(if self.fullscreen {
                            Some(winit::window::Fullscreen::Borderless(None))
                        } else {
                            None
                        });
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

                // Rejilla del lienzo (geometria que se dibuja detras de la tinta). En la
                // biblioteca no hay lienzo (solo las cartas), asi que se omite.
                self.grid_mesh.clear();
                let infinite_canvas = matches!(self.settings.artboard, settings::Artboard::Infinite);
                if !in_library {
                    // Cuaderno de HOJAS: dibujar la PAGINA (con el color/papel elegido en Ajustes).
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
                }
                // Fondo: la BIBLIOTECA es oscura (resaltan las cartas); el lienzo usa el fondo
                // elegido (infinito) o la "mesa" gris (hojas).
                let bg = if in_library {
                    [0.07, 0.08, 0.11, 1.0]
                } else if infinite_canvas {
                    self.settings.bg_color()
                } else {
                    [0.20, 0.21, 0.24, 1.0]
                };

                // Recorte del contenido a la HOJA (cuadernos de hojas): el dibujo y la
                // rejilla no se salen del rectangulo de la pagina. En la biblioteca, sin recorte.
                let content_clip = if in_library || infinite_canvas {
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

                // Cartas de la biblioteca (vacio en el lienzo). Se construye antes de prestar
                // la GPU (build_card_instances usa &self).
                let mut cards = if !in_library {
                    Vec::new()
                } else if self.preview_idx.is_some() {
                    // La VISTA PREVIA (libreta abierta) se dibuja entera con egui (abajo).
                    Vec::new()
                } else if self.creating_nb {
                    self.build_preview_card()
                } else {
                    self.build_card_instances(&card_layout)
                };
                // Fondo del Home (degradado/viñeta) detras de todo, para dar profundidad.
                if in_library {
                    cards.insert(0, self.bg_card());
                }

                // --- Render (lienzo + UI encima) ---
                // Recorte superior de las cartas (solo en la cuadricula del Home): al desplazar,
                // los cuadernos que suban se recortan justo bajo la cabecera (sin tapar el titulo).
                let card_clip = if in_library && !self.creating_nb && self.preview_idx.is_none() {
                    self.library_header_px() as u32
                } else {
                    0
                };
                if let Some(g) = self.gpu.as_mut() {
                    let screen = egui_wgpu::ScreenDescriptor {
                        size_in_pixels: [g.width(), g.height()],
                        pixels_per_point: ppp,
                    };
                    g.set_bg(bg);
                    g.set_content_clip(content_clip);
                    g.set_card_clip_top(card_clip);
                    g.set_grid(&self.grid_mesh);
                    let card_time = if self.lib_tweaks.animate { self.clock } else { 0.0 };
                    g.set_cards(&cards, self.camera.viewport.x, self.camera.viewport.y, card_time);
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


/// Carga una fuente profesional y legible (Segoe UI / alternativas del sistema) como fuente
/// por defecto de TODA la interfaz; mantiene las de respaldo de egui para los iconos/emoji.
fn setup_fonts(ctx: &egui::Context) {
    use std::sync::Arc;
    let mut fonts = egui::FontDefinitions::default();
    // Tipografias EDITORIALES (incrustadas, OFL): Hanken Grotesk (texto) + JetBrains Mono (mono).
    fonts.font_data.insert(
        "hanken".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/HankenGrotesk-Regular.ttf"))),
    );
    fonts.font_data.insert(
        "hanken_b".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/HankenGrotesk-Bold.ttf"))),
    );
    fonts.font_data.insert(
        "mono".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"))),
    );
    // Serif (Spectral) para el tema "Cuaderno": titulo y subtitulo en estilo editorial impreso.
    fonts.font_data.insert("spectral".to_owned(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Spectral-Regular.ttf"))));
    fonts.font_data.insert("spectral_sb".to_owned(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Spectral-SemiBold.ttf"))));
    fonts.font_data.insert("spectral_it".to_owned(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Spectral-Italic.ttf"))));
    // Fuentes de ESCRITURA del modo documento (OFL): Lora, Merriweather, EB Garamond (serif),
    // Atkinson Hyperlegible y Source Sans 3 (sans legibles).
    fonts.font_data.insert("lora".to_owned(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Lora-Regular.ttf"))));
    fonts.font_data.insert("merri".to_owned(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Merriweather-Regular.ttf"))));
    fonts.font_data.insert("garamond".to_owned(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/EBGaramond-Regular.ttf"))));
    fonts.font_data.insert("atkinson".to_owned(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/AtkinsonHyperlegible-Regular.ttf"))));
    fonts.font_data.insert("sourcesans".to_owned(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/SourceSans3-Regular.ttf"))));
    // Respaldo del sistema (acentos/glifos que falten): Segoe UI / Calibri / DejaVu.
    let fallback = ["C:/Windows/Fonts/segoeui.ttf", "C:/Windows/Fonts/calibri.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"]
        .iter()
        .find_map(|p| std::fs::read(p).ok());
    if let Some(bytes) = fallback {
        fonts.font_data.insert("ui_fallback".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
    }
    // Proporcional: Hanken primero; emoji de egui + respaldo del sistema al final.
    let prop = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    prop.insert(0, "hanken".to_owned());
    if fonts.font_data.contains_key("ui_fallback") {
        prop.push("ui_fallback".to_owned());
    }
    // Monospace: JetBrains Mono (etiquetas tipo devtool).
    let monop = fonts.families.entry(egui::FontFamily::Monospace).or_default();
    monop.insert(0, "mono".to_owned());
    // Familia "head" (titulos Tinta): Hanken Bold.
    fonts
        .families
        .insert(egui::FontFamily::Name("head".into()), vec!["hanken_b".to_owned(), "hanken".to_owned()]);
    // Familias serif (titulos/subtitulos Cuaderno): Spectral.
    fonts.families.insert(egui::FontFamily::Name("serif".into()), vec!["spectral_sb".to_owned(), "spectral".to_owned()]);
    fonts.families.insert(egui::FontFamily::Name("serif_it".into()), vec!["spectral_it".to_owned(), "spectral".to_owned()]);
    // Familias de las fuentes de escritura (con respaldo "hanken" para glifos que falten).
    for (name, data) in [
        ("doc_lora", "lora"),
        ("doc_merri", "merri"),
        ("doc_garamond", "garamond"),
        ("doc_atkinson", "atkinson"),
        ("doc_sourcesans", "sourcesans"),
    ] {
        fonts.families.insert(egui::FontFamily::Name(name.into()), vec![data.to_owned(), "hanken".to_owned()]);
    }
    ctx.set_fonts(fonts);
}

/// Hex 0xRRGGBB -> [f32;3] lineal-de-pantalla (la superficie es WYSIWYG, no-sRGB).
fn hexf(h: u32) -> [f32; 3] {
    [
        ((h >> 16) & 0xFF) as f32 / 255.0,
        ((h >> 8) & 0xFF) as f32 / 255.0,
        (h & 0xFF) as f32 / 255.0,
    ]
}

/// Gama COPIC curada (hex sRGB aprox., por familias BV/V/RV/R/YR/Y/YG/G/BG/B/E + grises). Valores
/// aproximados (los marcadores varian al capar). Para el COLOR de todo el cuaderno (finish 800).
const COPIC_SOLIDS: &[u32] = &[
    0xECEDF6, 0x9E9FC6, 0x7C74B0, 0x5E4F97, 0x3B3A5E, // BV
    0xE6C7E0, 0xB98CC8, 0x9C5BA0, 0x6B3A8E, // V
    0xF6CBD6, 0xF79FB0, 0xE85C8E, 0xC0356E, // RV
    0xF8C9BC, 0xF59A86, 0xE85B52, 0xC0273A, 0x8E2238, // R
    0xFBD9B0, 0xF6B25A, 0xF0852E, 0xDA5A1E, // YR
    0xF7EFB0, 0xF6E45A, 0xF4D000, 0xEAC12A, // Y
    0xDDE89A, 0xAFD060, 0x6FB23A, 0x4A8E2E, // YG
    0xBFE3C2, 0x7FCB86, 0x36A85A, 0x1E7A4A, // G
    0xC6E8E6, 0x7ED0CE, 0x2EB6B0, 0x0E8E96, 0x2A6E7A, // BG
    0xCFE8F6, 0x8EC6EC, 0x3E9EDC, 0x1E6FC0, 0x224E9E, // B
    0xF6E6D8, 0xE6C0A0, 0xCF9A6E, 0xA86A44, 0x6E3F26, // E
    0xEBEDED, 0xC9CFCF, 0x9BA2A2, 0x6E7474, 0x444A4A, 0x1E1C22, // grises + negro
];

/// Degradados COPIC (par de hex): claro->oscuro por familia + combinaciones vibrantes.
const COPIC_GRADS: &[(u32, u32)] = &[
    (0xECEDF6, 0x3B3A5E), (0xE6C7E0, 0x6B3A8E), (0xF6CBD6, 0xC0356E), (0xF8C9BC, 0x8E2238),
    (0xFBD9B0, 0xDA5A1E), (0xF7EFB0, 0xF4D000), (0xDDE89A, 0x4A8E2E), (0xBFE3C2, 0x1E7A4A),
    (0xC6E8E6, 0x0E8E96), (0xCFE8F6, 0x224E9E), (0xF6E6D8, 0x6E3F26), (0xEBEDED, 0x1E1C22),
    (0xF6E45A, 0xE85B52), (0x7ED0CE, 0x1E6FC0), (0xF79FB0, 0x6B3A8E), (0xAFD060, 0x1E7A4A),
    (0x8EC6EC, 0x9C5BA0), (0xF6B25A, 0xC0273A),
];

/// Diseños FOIL (id, nombre) que se eligen como carátula base.
const FOIL_DESIGNS: [(u32, &str); 12] = [
    (0, "Mate"), (1, "Holográfico"), (2, "Galaxia"), (3, "Oro"), (4, "Prisma"), (5, "Destellos"),
    (6, "Aurora"), (7, "Neón"), (8, "Esmeralda"), (9, "Rubí"), (10, "Cromo"), (11, "Atardecer"),
];

/// Cargadores ORGANICOS animados (id >= 100, nombre).
const LOADER_DESIGNS: [(u32, &str); 12] = [
    (100, "Gota"), (101, "Metábolas"), (102, "Onda"), (103, "Pulso"), (104, "Órbita"),
    (105, "Espiral"), (106, "Ameba"), (107, "Burbujas"), (108, "Cometa"), (109, "Flor"),
    (110, "Gusano"), (111, "Lava"),
];

/// Escenas 3D animadas (id >= 200): sistema solar y atractores extraños (el color de los
/// atractores usa el Acento elegido).
const SCENE_DESIGNS: [(u32, &str); 6] = [
    (200, "Sistema solar"), (210, "Lorenz"), (211, "Aizawa"), (212, "Halvorsen"),
    (213, "Thomas"), (214, "Rössler"),
];

/// Cargadores en 3D real (esferas con perspectiva/sombreado, id >= 300). Usan el Acento.
const LOADER3D_DESIGNS: [(u32, &str); 6] = [
    (300, "Átomo"), (301, "Hélice"), (302, "Esfera"), (303, "Anillo 3D"),
    (304, "Cúmulo"), (305, "Espiral 3D"),
];

/// Animaciones tipo retrowave / arcade clasico / generativas (id >= 400). Usan el Acento.
const ARCADE_DESIGNS: [(u32, &str); 9] = [
    (400, "Retrowave"), (401, "Hiperespacio"), (402, "Vórtice"), (403, "Invaders"),
    (404, "Tetris"), (405, "Vida"), (406, "Flores"), (407, "Pac-Man"), (408, "Relámpagos"),
];

/// Figuras 3D solidas raymarcheadas que giran (id >= 600). Usan el Acento (color) y la Textura.
const FIGURE_DESIGNS: [(u32, &str); 5] = [
    (600, "Cubo"), (601, "Esfera"), (602, "Pirámide"), (603, "Toro"), (604, "Octaedro"),
];

/// Materiales realistas (idx = texture; 0 = ninguno). Son fotos CC0 (Poly Haven) incrustadas;
/// el orden DEBE coincidir con MATERIAL_JPGS en renderer.rs (capa = idx - 1). Se aplican a TODO
/// el cuaderno (tapa+lomo+contratapa) y a las figuras 3D.
const TEXTURE_NAMES: [&str; 12] = [
    "Ninguno", "Cuero", "Madera", "Lino", "Denim", "Lana bouclé", "Cuero rojo", "Tela de libro",
    "Contrachapado", "Azulejo", "Piedras preciosas", "Diamante",
];

/// Capas COMBINABLES (bit, nombre).
const FX_LAYERS: [(u32, &str); 3] = [(1, "Destellos"), (2, "Brillo animado"), (4, "Resplandor")];

/// Paleta de ACENTO (idx, nombre). El 0 es blanco (B&N en los cargadores).
const ACCENTS: [&str; 8] = ["Blanco", "Cian", "Magenta", "Ámbar", "Verde", "Rojo", "Violeta", "Azul"];

/// FORMAS de cuaderno (idx = shape): geometria, no portada.
const SHAPE_NAMES: [&str; 5] = ["Cuaderno", "Tapa dura", "Moleskine", "Espiral", "Minimalista"];

/// Parametros de geometria de cada forma: (grosor relativo, ceja [fraccion], tabla de tapa
/// en el canto [fraccion del grosor]). El cuaderno es la caja 3D del shader.
fn shape_params(shape: u32) -> (f32, f32, f32) {
    match shape {
        1 => (0.16, 0.060, 0.22), // Tapa dura: gruesa, ceja marcada, tabla ancha
        2 => (0.12, 0.012, 0.10), // Moleskine: flexible, casi a ras
        3 => (0.12, 0.035, 0.12), // Espiral
        4 => (0.06, 0.000, 0.10), // Minimalista: fina
        _ => (0.13, 0.000, 0.00), // Actual (la de ahora: a ras, hojas de borde a borde)
    }
}

/// Modos de hover (idx). 0=levantar, 1=abrir, 2=girar, 3=sutil.
const HOVER_NAMES: [&str; 4] = ["Levantar", "Abrir", "Girar", "Sutil"];

/// Parte un nombre en hasta `max_lines` renglones de `max_chars` caracteres (corta por
/// palabras; parte palabras muy largas; recorta con elipsis si sigue sin caber).
fn wrap_name(name: &str, max_chars: usize, max_lines: usize) -> Vec<String> {
    let max_chars = max_chars.max(3);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in name.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > max_chars {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            lines.push(word.chars().take(max_chars).collect());
            word = word.chars().skip(max_chars).collect();
        }
        let extra = if cur.is_empty() { 0 } else { 1 };
        if cur.chars().count() + extra + word.chars().count() <= max_chars {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(&word);
        } else {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            cur = word;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.last_mut().unwrap();
        while last.chars().count() > max_chars.saturating_sub(1) {
            last.pop();
        }
        last.push('…');
    }
    lines
}

/// Proyecta un punto LOCAL del cuaderno (caja) a pantalla, replicando la transformacion del
/// vertex shader (rotacion Y, rotacion X, elevacion por hover y perspectiva). Devuelve
/// (x_px, y_px, factor) para colocar el nombre EXACTAMENTE sobre la cinta del reverso.
fn project_card_point(c: Vec2, rotx: f32, roty: f32, hover: f32, lx: f32, ly: f32, lz: f32) -> (f32, f32, f32) {
    let focal = 900.0_f32;
    let (cyr, syr) = (roty.cos(), roty.sin());
    let p1x = lx * cyr + lz * syr;
    let p1z = -lx * syr + lz * cyr;
    let (cxr, sxr) = (rotx.cos(), rotx.sin());
    let p2x = p1x;
    let p2y = ly * cxr - p1z * sxr;
    let p2z = ly * sxr + p1z * cxr + hover * 70.0;
    let factor = focal / (focal - p2z).max(1.0);
    (c.x + p2x * factor, c.y + p2y * factor, factor)
}

/// Fecha y hora LOCAL actual como (año, mes, día, hora, min, seg). En Windows usa GetLocalTime.
#[cfg(windows)]
fn local_now() -> (u16, u16, u16, u16, u16, u16) {
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond)
}
#[cfg(not(windows))]
fn local_now() -> (u16, u16, u16, u16, u16, u16) {
    // Fallback (UTC) desde el reloj del sistema, con el algoritmo civil de Howard Hinnant.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (h, mi, s) = (((secs % 86400) / 3600) as u16, ((secs % 3600) / 60) as u16, (secs % 60) as u16);
    let z = secs.div_euclid(86400) + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u16;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u16;
    let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as u16;
    (y, m, d, h, mi, s)
}

/// Nombre de una nota rápida: la fecha y hora de creación. Los ":" se sanean en el archivo.
fn quick_note_name() -> String {
    let (y, mo, d, h, mi, s) = local_now();
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
}

/// Carátula (finish) de una hoja segun el papel/cuadricula activa: 500 rayas, 501 milimetrado,
/// 502 puntos, 503 blanca, 504 iso, 505 triangular. (P1/P2/P3 -> blanca.)
fn sheet_finish_for_grid(grid: ink_core::GridKind) -> u32 {
    match grid {
        ink_core::GridKind::Lines => 500,
        ink_core::GridKind::Squares => 501,
        ink_core::GridKind::Dots => 502,
        ink_core::GridKind::Iso => 504,
        ink_core::GridKind::Triangle => 505,
        _ => 503,
    }
}

/// Color base de cada diseño (el shader de cartas anade el efecto encima).
fn finish_base_color(finish: u32) -> [f32; 3] {
    match finish {
        0 => [0.82, 0.82, 0.86],  // Mate (claro)
        2 => [0.06, 0.05, 0.16],  // Galaxia (azul casi negro)
        3 => [0.34, 0.24, 0.07],  // Oro (marron oscuro)
        4 => [0.12, 0.13, 0.18],  // Prisma (gris oscuro)
        5 => [0.17, 0.10, 0.24],  // Destellos (morado oscuro)
        6 => [0.04, 0.10, 0.10],  // Aurora (verde-azulado oscuro)
        7 => [0.05, 0.04, 0.10],  // Neón (azul muy oscuro)
        8 => [0.03, 0.10, 0.06],  // Esmeralda (verde oscuro)
        9 => [0.12, 0.03, 0.05],  // Rubí (rojo oscuro)
        10 => [0.20, 0.22, 0.26], // Cromo (gris medio)
        11 => [0.10, 0.05, 0.10], // Atardecer (calido oscuro)
        f if (800..900).contains(&f) => [0.6, 0.6, 0.6], // Color solido/degradado (lo fija la instancia)
        f if f >= 600 => [0.03, 0.035, 0.05], // Figuras 3D: fondo oscuro
        f if f >= 500 => [0.97, 0.97, 0.95], // Nota rapida (hoja de papel): casi blanco
        f if f >= 400 => [0.02, 0.02, 0.04], // Arcade y demos: fondo oscuro
        f if f >= 300 => [0.04, 0.04, 0.06], // Cargadores 3D: fondo oscuro
        f if f >= 200 => [0.02, 0.02, 0.05], // Escenas 3D: espacio oscuro
        f if f >= 100 => [0.05, 0.05, 0.06], // Cargadores: fondo oscuro neutro
        _ => [0.17, 0.20, 0.42],  // Holografico (azul-violeta)
    }
}

/// Color del acento elegido (idx 0 = blanco; el resto, colores para cargadores/resplandor).
fn accent_color(accent: u32) -> [f32; 3] {
    match accent {
        1 => [0.20, 0.85, 1.00], // Cian
        2 => [1.00, 0.25, 0.75], // Magenta
        3 => [1.00, 0.72, 0.25], // Ámbar
        4 => [0.35, 0.95, 0.45], // Verde
        5 => [1.00, 0.32, 0.30], // Rojo
        6 => [0.65, 0.45, 1.00], // Violeta
        7 => [0.35, 0.55, 1.00], // Azul
        _ => [0.95, 0.95, 0.97], // Blanco (B&N)
    }
}

/// Oscurece la barra de TITULO de la ventana (Windows 11) para que combine con el Home y no
/// haya un corte de color abrupto: modo oscuro + color de barra/borde = fondo de la app.
#[cfg(windows)]
fn set_dark_titlebar(hwnd: isize, caption: u32) {
    use windows::Win32::Foundation::{BOOL, COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
    };
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let cap = COLORREF(caption); // 0x00BBGGRR
    unsafe {
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const core::ffi::c_void,
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            &cap as *const _ as *const core::ffi::c_void,
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &cap as *const _ as *const core::ffi::c_void,
            4,
        );
    }
}

/// Icono vectorial (dibujado, no depende de glifos de fuente) para los botones del Home.
#[derive(Clone, Copy, PartialEq)]
enum BtnIcon {
    Plus,    // nuevo
    Note,    // hoja con renglones (nota rapida)
    Sliders, // ajustes
}

fn draw_btn_icon(p: &egui::Painter, icon: BtnIcon, c: egui::Pos2, col: egui::Color32) {
    let st = egui::Stroke::new(1.7, col);
    match icon {
        BtnIcon::Plus => {
            p.line_segment([egui::pos2(c.x - 6.0, c.y), egui::pos2(c.x + 6.0, c.y)], egui::Stroke::new(2.0, col));
            p.line_segment([egui::pos2(c.x, c.y - 6.0), egui::pos2(c.x, c.y + 6.0)], egui::Stroke::new(2.0, col));
        }
        BtnIcon::Note => {
            let r = egui::Rect::from_center_size(c, egui::vec2(11.0, 14.0));
            p.rect_stroke(r, egui::CornerRadius::same(2), st, egui::StrokeKind::Inside);
            for k in 0..3 {
                let y = r.top() + 4.0 + k as f32 * 3.4;
                p.line_segment([egui::pos2(r.left() + 2.6, y), egui::pos2(r.right() - 2.6, y)], egui::Stroke::new(1.0, col));
            }
        }
        BtnIcon::Sliders => {
            let (y1, y2) = (c.y - 3.6, c.y + 3.6);
            p.line_segment([egui::pos2(c.x - 7.0, y1), egui::pos2(c.x + 7.0, y1)], st);
            p.line_segment([egui::pos2(c.x - 7.0, y2), egui::pos2(c.x + 7.0, y2)], st);
            p.circle_filled(egui::pos2(c.x + 2.5, y1), 2.4, col);
            p.circle_filled(egui::pos2(c.x - 2.5, y2), 2.4, col);
        }
    }
}

/// Estilo de un boton segun el tema (relleno, texto, icono, borde, y sus variantes al pasar el cursor).
#[derive(Clone, Copy)]
struct BtnStyle {
    fill: egui::Color32,
    text: egui::Color32,
    icon: egui::Color32,
    border: egui::Color32,
    hover_fill: egui::Color32,
    hover_text: egui::Color32,
}

/// Paleta del CHROME (interfaz) por tema. No afecta a los notebooks (su diseño es aparte).
struct HomeTheme {
    title: egui::Color32,
    sub: egui::Color32,
    kicker: egui::Color32,
    kicker_b: egui::Color32,
    name: egui::Color32,
    sep: egui::Color32,
    icon_btn: egui::Color32,
    search_fill: egui::Color32,
    serif: bool, // Cuaderno usa titulo/subtitulo serif (Spectral)
    primary: BtnStyle,
    secondary: BtnStyle,
}

/// Botón solo-icono (Ajustes, etc.): 34x34, hover sutil.
fn icon_btn(ui: &mut egui::Ui, icon: BtnIcon, col: egui::Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::click());
    let p = ui.painter();
    let c = if resp.hovered() {
        p.rect_filled(rect, egui::CornerRadius::same(8), egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14));
        egui::Color32::from_rgb(col.r().saturating_add(45), col.g().saturating_add(45), col.b().saturating_add(45))
    } else {
        col
    };
    draw_btn_icon(p, icon, rect.center(), c);
    resp
}

fn home_theme(theme: u32) -> HomeTheme {
    let rgba = egui::Color32::from_rgba_unmultiplied;
    let rgb = egui::Color32::from_rgb;
    if theme == 1 {
        // CUADERNO: papel claro, texto oscuro, acento rojo, serif.
        HomeTheme {
            title: rgb(34, 28, 20),
            sub: rgb(138, 125, 100),
            kicker: rgb(154, 143, 120),
            kicker_b: rgb(192, 89, 79),
            name: rgb(43, 37, 27),
            sep: rgba(60, 48, 30, 46),
            icon_btn: rgb(111, 99, 80),
            search_fill: rgba(255, 255, 255, 110),
            serif: true,
            primary: BtnStyle {
                fill: rgb(33, 27, 18),
                text: rgb(240, 233, 216),
                icon: rgb(192, 89, 79),
                border: egui::Color32::TRANSPARENT,
                hover_fill: rgb(45, 37, 25),
                hover_text: rgb(240, 233, 216),
            },
            secondary: BtnStyle {
                fill: egui::Color32::TRANSPARENT,
                text: rgb(74, 66, 50),
                icon: rgb(138, 125, 100),
                border: rgba(60, 48, 30, 80),
                hover_fill: rgba(192, 89, 79, 22),
                hover_text: rgb(36, 31, 23),
            },
        }
    } else {
        // TINTA: galeria oscura, texto claro, acento crema (#cabfa7).
        let cream = rgb(202, 191, 167);
        HomeTheme {
            title: rgb(236, 233, 242),
            sub: rgb(134, 131, 143),
            kicker: rgb(125, 122, 135),
            kicker_b: cream,
            name: rgb(205, 202, 214),
            sep: rgba(202, 191, 167, 26),
            icon_btn: rgb(127, 124, 138),
            search_fill: rgb(21, 19, 29),
            serif: false,
            primary: BtnStyle {
                fill: egui::Color32::TRANSPARENT,
                text: cream,
                icon: cream,
                border: cream,
                hover_fill: cream,
                hover_text: rgb(14, 12, 19), // tinta llena -> texto oscuro
            },
            secondary: BtnStyle {
                fill: egui::Color32::TRANSPARENT,
                text: rgb(183, 179, 193),
                icon: rgb(127, 124, 138),
                border: rgb(44, 41, 54),
                hover_fill: rgba(255, 255, 255, 8),
                hover_text: rgb(230, 227, 238),
            },
        }
    }
}

/// Botón con estilo de tema (relleno/borde/hover) + icono vectorial + texto (Hanken peso 600).
/// `min_w` > 0 fija el ancho (icono+texto centrados); 0 = ancho automatico segun el texto.
fn pill_button(ui: &mut egui::Ui, text: &str, icon: BtnIcon, st: BtnStyle, min_w: f32) -> egui::Response {
    let font = egui::FontId::new(15.0, egui::FontFamily::Name("head".into()));
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), st.text);
    let (icon_w, pad, gap) = (16.0_f32, 17.0_f32, 9.0_f32);
    let content = icon_w + gap + galley.size().x;
    let w = (pad * 2.0 + content).max(min_w);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 38.0), egui::Sense::click());
    let hov = resp.hovered();
    let off = if resp.is_pointer_button_down_on() { egui::vec2(0.0, 1.0) } else { egui::Vec2::ZERO };
    let r = rect.translate(off);
    let p = ui.painter();
    let bg = if hov { st.hover_fill } else { st.fill };
    if bg.a() > 0 {
        p.rect_filled(r, egui::CornerRadius::same(7), bg);
    }
    if st.border.a() > 0 {
        p.rect_stroke(r, egui::CornerRadius::same(7), egui::Stroke::new(1.5, st.border), egui::StrokeKind::Inside);
    }
    let (tcol, icol) = if hov { (st.hover_text, st.hover_text) } else { (st.text, st.icon) };
    // Contenido centrado dentro del boton (importante cuando min_w lo hace ancho).
    let cstart = r.left() + (r.width() - content) * 0.5;
    let ic = egui::pos2(cstart + icon_w * 0.5, r.center().y);
    draw_btn_icon(p, icon, ic, icol);
    let g = if hov { ui.painter().layout_no_wrap(text.to_string(), font, tcol) } else { galley };
    p.galley(egui::pos2(cstart + icon_w + gap, r.center().y - g.size().y * 0.5), g, tcol);
    resp
}

/// Fila de un ARCHIVERO en la barra lateral: icono + nombre + contador; resalta el activo.
/// Si `dragging` y el cursor (`cursor_pts`, en PUNTOS) cae sobre la fila, se ilumina con un
/// latido (`pulse` 0..1) para dejar claro a qué archivero se va a guardar el cuaderno arrastrado.
fn archivero_row(
    ui: &mut egui::Ui,
    label: &str,
    count: usize,
    active: bool,
    th: &HomeTheme,
    dragging: bool,
    cursor_pts: egui::Pos2,
    pulse: f32,
) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 34.0), egui::Sense::click());
    let p = ui.painter();
    // Destino de arrastre: el cuaderno se guardará en este archivero (o "Todos" = sin archivero).
    let drop_target = dragging && rect.contains(cursor_pts);
    let mut col = if active { th.title } else { th.sub };
    if drop_target {
        // Halo + relleno crema PULSANTE + borde, para señalar el destino con claridad.
        let halo = rect.expand(2.0 + pulse * 3.0);
        p.rect_filled(halo, egui::CornerRadius::same(10), egui::Color32::from_rgba_unmultiplied(202, 191, 167, (26.0 + pulse * 34.0) as u8));
        p.rect_filled(rect, egui::CornerRadius::same(8), egui::Color32::from_rgba_unmultiplied(202, 191, 167, (120.0 + pulse * 80.0) as u8));
        p.rect_stroke(rect, egui::CornerRadius::same(8), egui::Stroke::new(1.5, egui::Color32::from_rgb(202, 191, 167)), egui::StrokeKind::Inside);
        col = egui::Color32::from_rgb(26, 22, 30); // texto/icono oscuro para contraste sobre la crema
    } else if active {
        p.rect_filled(rect, egui::CornerRadius::same(8), egui::Color32::from_rgba_unmultiplied(202, 191, 167, 22));
    } else if resp.hovered() {
        p.rect_filled(rect, egui::CornerRadius::same(8), egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10));
    }
    let ic = egui::pos2(rect.left() + 15.0, rect.center().y);
    let st = egui::Stroke::new(1.4, col);
    if label == "Todos" {
        // "Todos" = rejilla 2x2 (ver todo).
        let (s, g) = (5.0_f32, 2.2_f32);
        for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let c = egui::pos2(ic.x + dx * (s + g) * 0.5, ic.y + dy * (s + g) * 0.5);
            p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(s, s)), egui::CornerRadius::same(1), st, egui::StrokeKind::Inside);
        }
    } else {
        // Icono ARCHIVERO (caja de archivo): tapa que sobresale + cuerpo + tirador central.
        let lid = egui::Rect::from_min_size(egui::pos2(ic.x - 8.0, ic.y - 7.0), egui::vec2(16.0, 4.2));
        p.rect_stroke(lid, egui::CornerRadius { nw: 2, ne: 2, sw: 1, se: 1 }, st, egui::StrokeKind::Inside);
        let body = egui::Rect::from_min_size(egui::pos2(ic.x - 6.5, ic.y - 2.8), egui::vec2(13.0, 9.8));
        p.rect_stroke(body, egui::CornerRadius { nw: 0, ne: 0, sw: 2, se: 2 }, st, egui::StrokeKind::Inside);
        p.line_segment([egui::pos2(ic.x - 2.6, ic.y + 2.2), egui::pos2(ic.x + 2.6, ic.y + 2.2)], st);
    }
    p.text(egui::pos2(rect.left() + 34.0, rect.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(14.5), col);
    let count_col = if drop_target { col } else { th.sub };
    p.text(egui::pos2(rect.right() - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, count.to_string(), egui::FontId::new(11.5, egui::FontFamily::Monospace), count_col);
    resp
}

/// Fila de accion de la barra lateral (p.ej. "Nuevo archivero"): "+" VECTORIAL + texto, alineada
/// con las filas de archivero. (Evita el carácter "＋" que la fuente no dibuja → salia un cuadro.)
fn add_action_row(ui: &mut egui::Ui, label: &str, th: &HomeTheme) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 32.0), egui::Sense::click());
    let p = ui.painter();
    let col = if resp.hovered() { th.title } else { th.sub };
    let ic = egui::pos2(rect.left() + 15.0, rect.center().y);
    let st = egui::Stroke::new(1.7, col);
    let r = 5.5;
    p.line_segment([egui::pos2(ic.x - r, ic.y), egui::pos2(ic.x + r, ic.y)], st);
    p.line_segment([egui::pos2(ic.x, ic.y - r), egui::pos2(ic.x, ic.y + r)], st);
    p.text(egui::pos2(rect.left() + 34.0, rect.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(14.0), col);
    resp
}

/// Lupa VECTORIAL para el buscador (anillo + mango). Evita el glifo "⌕", que la fuente no dibuja
/// (salía como un cuadro). Reserva su espacio con `allocate_exact_size` y la pinta encima.
fn search_icon(ui: &mut egui::Ui, col: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let p = ui.painter();
    let c = egui::pos2(rect.left() + 6.5, rect.center().y - 1.0);
    let st = egui::Stroke::new(1.6, col);
    p.circle_stroke(c, 4.6, st);
    let d = 4.6 * std::f32::consts::FRAC_1_SQRT_2;
    p.line_segment([egui::pos2(c.x + d, c.y + d), egui::pos2(c.x + d + 3.6, c.y + d + 3.6)], st);
}

/// Boton "Escribir" (modo documento): lapiz VECTORIAL + texto. Resalta si esta activo.
fn doc_pencil_button(ui: &mut egui::Ui, active: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        "Escribir".to_string(),
        egui::FontId::proportional(14.0),
        egui::Color32::PLACEHOLDER,
    );
    let w = 22.0 + galley.size().x + 10.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
    let p = ui.painter();
    let bg = if active {
        egui::Color32::from_rgb(120, 178, 235)
    } else if resp.hovered() {
        egui::Color32::from_gray(228)
    } else {
        egui::Color32::from_gray(238)
    };
    p.rect_filled(rect, egui::CornerRadius::same(5), bg);
    let col = if active { egui::Color32::WHITE } else { egui::Color32::from_gray(60) };
    // Teclado de PC: cuerpo + teclas + barra espaciadora.
    let c = egui::pos2(rect.left() + 14.0, rect.center().y);
    let st = egui::Stroke::new(1.4, col);
    let body = egui::Rect::from_center_size(c, egui::vec2(17.0, 11.0));
    p.rect_stroke(body, egui::CornerRadius::same(2), st, egui::StrokeKind::Inside);
    for &ky in &[c.y - 2.5, c.y + 0.5] {
        for dx in [-5.0_f32, -1.7, 1.7, 5.0] {
            p.rect_filled(egui::Rect::from_center_size(egui::pos2(c.x + dx, ky), egui::vec2(1.6, 1.6)), egui::CornerRadius::same(0), col);
        }
    }
    p.line_segment([egui::pos2(c.x - 4.0, c.y + 3.4), egui::pos2(c.x + 4.0, c.y + 3.4)], egui::Stroke::new(1.4, col));
    p.text(egui::pos2(rect.left() + 24.0, rect.center().y), egui::Align2::LEFT_CENTER, "Escribir", egui::FontId::proportional(14.0), col);
    resp
}

/// Comando de formato Markdown de la toolbar de edicion.
#[derive(Clone, Copy, PartialEq)]
enum Md {
    Bold,
    Italic,
    Underline,
    Strike,
    Highlight,
    Code,
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    AlignLeft,
    AlignCenter,
    AlignRight,
    AlignJustify,
    Bullet,
    Number,
    Task,
    Quote,
    Hr,
    Link,
    Indent,
    Outdent,
    Clear,
    FontColor,
    HighlightColor,
    Undo,
    Redo,
    CodeBlock,
    Callout,
    Fullscreen,
    Hn,
    Align,
    Image,
    Table,
}

/// Boton de la toolbar de edicion: icono VECTORIAL (o letra de la fuente real, que SI existe).
fn md_button(ui: &mut egui::Ui, md: Md, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 28.0), egui::Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, egui::CornerRadius::same(6), egui::Color32::from_rgba_unmultiplied(0, 0, 0, 18));
    }
    let col = egui::Color32::from_gray(55);
    let c = rect.center();
    let head = egui::FontFamily::Name("head".into());
    let st = egui::Stroke::new(1.5, col);
    match md {
        Md::Bold => { p.text(c, egui::Align2::CENTER_CENTER, "B", egui::FontId::new(16.0, head), col); }
        Md::Italic => { p.text(c, egui::Align2::CENTER_CENTER, "I", egui::FontId::new(16.0, egui::FontFamily::Name("serif_it".into())), col); }
        Md::Underline => {
            p.text(egui::pos2(c.x, c.y - 1.0), egui::Align2::CENTER_CENTER, "U", egui::FontId::proportional(15.0), col);
            p.line_segment([egui::pos2(c.x - 6.0, c.y + 8.0), egui::pos2(c.x + 6.0, c.y + 8.0)], egui::Stroke::new(1.5, col));
        }
        Md::Strike => {
            p.text(c, egui::Align2::CENTER_CENTER, "S", egui::FontId::proportional(15.0), col);
            p.line_segment([egui::pos2(c.x - 6.0, c.y), egui::pos2(c.x + 6.0, c.y)], egui::Stroke::new(1.4, col));
        }
        Md::Highlight => {
            // Trazo de resaltador (rectangulo amarillo) + linea base.
            let r = egui::Rect::from_center_size(egui::pos2(c.x, c.y - 1.0), egui::vec2(15.0, 9.0));
            p.rect_filled(r, egui::CornerRadius::same(2), egui::Color32::from_rgb(250, 224, 90));
            p.text(egui::pos2(c.x, c.y - 1.0), egui::Align2::CENTER_CENTER, "a", egui::FontId::new(12.0, egui::FontFamily::Name("head".into())), egui::Color32::from_gray(40));
            p.line_segment([egui::pos2(c.x - 8.0, c.y + 7.0), egui::pos2(c.x + 8.0, c.y + 7.0)], egui::Stroke::new(2.0, egui::Color32::from_rgb(230, 200, 70)));
        }
        Md::Code => {
            let st2 = egui::Stroke::new(1.6, col);
            // chevron "<"
            p.line_segment([egui::pos2(c.x - 3.0, c.y - 4.0), egui::pos2(c.x - 8.0, c.y)], st2);
            p.line_segment([egui::pos2(c.x - 8.0, c.y), egui::pos2(c.x - 3.0, c.y + 4.0)], st2);
            // chevron ">"
            p.line_segment([egui::pos2(c.x + 3.0, c.y - 4.0), egui::pos2(c.x + 8.0, c.y)], st2);
            p.line_segment([egui::pos2(c.x + 8.0, c.y), egui::pos2(c.x + 3.0, c.y + 4.0)], st2);
        }
        Md::H1 => { p.text(c, egui::Align2::CENTER_CENTER, "H1", egui::FontId::new(12.5, head), col); }
        Md::H2 => { p.text(c, egui::Align2::CENTER_CENTER, "H2", egui::FontId::new(11.5, head), col); }
        Md::H3 => { p.text(c, egui::Align2::CENTER_CENTER, "H3", egui::FontId::new(10.5, head), col); }
        Md::H4 => { p.text(c, egui::Align2::CENTER_CENTER, "H4", egui::FontId::new(10.0, head), col); }
        Md::H5 => { p.text(c, egui::Align2::CENTER_CENTER, "H5", egui::FontId::new(9.5, head), col); }
        Md::H6 => { p.text(c, egui::Align2::CENTER_CENTER, "H6", egui::FontId::new(9.0, head), col); }
        Md::AlignLeft | Md::AlignCenter | Md::AlignRight | Md::AlignJustify => {
            // 4 lineas; la 2a y 4a varian segun la alineacion.
            let ys = [c.y - 4.5, c.y - 1.5, c.y + 1.5, c.y + 4.5];
            for (i, &y) in ys.iter().enumerate() {
                let (x0, x1) = match (md, i) {
                    (Md::AlignCenter, 1) | (Md::AlignCenter, 3) => (c.x - 5.0, c.x + 5.0),
                    (Md::AlignRight, 1) | (Md::AlignRight, 3) => (c.x - 2.0, c.x + 8.0),
                    (Md::AlignJustify, _) => (c.x - 8.0, c.x + 8.0),
                    (_, 1) | (_, 3) => (c.x - 8.0, c.x + 2.0), // left (filas cortas)
                    _ => (c.x - 8.0, c.x + 8.0),               // filas largas
                };
                p.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], egui::Stroke::new(1.4, col));
            }
        }
        Md::Bullet => {
            for dy in [-4.5_f32, 0.0, 4.5] {
                p.circle_filled(egui::pos2(c.x - 7.0, c.y + dy), 1.3, col);
                p.line_segment([egui::pos2(c.x - 3.0, c.y + dy), egui::pos2(c.x + 8.0, c.y + dy)], st);
            }
        }
        Md::Number => { p.text(c, egui::Align2::CENTER_CENTER, "1.", egui::FontId::new(13.0, egui::FontFamily::Monospace), col); }
        Md::Task => {
            let r = egui::Rect::from_center_size(c, egui::vec2(13.0, 13.0));
            p.rect_stroke(r, egui::CornerRadius::same(3), st, egui::StrokeKind::Inside);
            p.line_segment([egui::pos2(c.x - 3.0, c.y + 0.5), egui::pos2(c.x - 1.0, c.y + 3.0)], egui::Stroke::new(1.7, col));
            p.line_segment([egui::pos2(c.x - 1.0, c.y + 3.0), egui::pos2(c.x + 4.0, c.y - 3.0)], egui::Stroke::new(1.7, col));
        }
        Md::Quote => {
            p.line_segment([egui::pos2(c.x - 7.0, c.y - 5.0), egui::pos2(c.x - 7.0, c.y + 5.0)], egui::Stroke::new(2.6, col));
            p.line_segment([egui::pos2(c.x - 3.0, c.y - 3.0), egui::pos2(c.x + 8.0, c.y - 3.0)], st);
            p.line_segment([egui::pos2(c.x - 3.0, c.y + 2.5), egui::pos2(c.x + 8.0, c.y + 2.5)], st);
        }
        Md::Hr => {
            p.line_segment([egui::pos2(c.x - 8.0, c.y), egui::pos2(c.x + 8.0, c.y)], egui::Stroke::new(2.0, col));
        }
        Md::Link => {
            // Dos eslabones (cadena) unidos por una diagonal.
            let lk = egui::Stroke::new(1.6, col);
            p.line_segment([egui::pos2(c.x - 2.0, c.y - 2.0), egui::pos2(c.x + 2.0, c.y + 2.0)], lk);
            for (sx, sy) in [(-1.0_f32, -1.0_f32), (1.0, 1.0)] {
                let cc = egui::pos2(c.x + sx * 4.5, c.y + sy * 4.5);
                let r = egui::Rect::from_center_size(cc, egui::vec2(7.0, 5.0));
                p.rect_stroke(r, egui::CornerRadius::same(2), lk, egui::StrokeKind::Inside);
            }
        }
        Md::Indent | Md::Outdent => {
            // Tres lineas + flecha (derecha = sangrar, izquierda = quitar sangria).
            for dy in [-5.0_f32, 0.0, 5.0] {
                p.line_segment([egui::pos2(c.x - 2.0, c.y + dy), egui::pos2(c.x + 8.0, c.y + dy)], st);
            }
            let dir = if matches!(md, Md::Indent) { 1.0 } else { -1.0 };
            let tipx = c.x - 8.0 + if dir > 0.0 { 0.0 } else { 4.0 };
            p.line_segment([egui::pos2(tipx, c.y - 3.0), egui::pos2(tipx + dir * 4.0, c.y)], st);
            p.line_segment([egui::pos2(tipx + dir * 4.0, c.y), egui::pos2(tipx, c.y + 3.0)], st);
        }
        Md::Clear => {
            // "A" con una diagonal (limpiar formato).
            p.text(egui::pos2(c.x - 1.0, c.y), egui::Align2::CENTER_CENTER, "A", egui::FontId::new(14.0, egui::FontFamily::Name("head".into())), col);
            p.line_segment([egui::pos2(c.x - 8.0, c.y + 7.0), egui::pos2(c.x + 8.0, c.y - 7.0)], egui::Stroke::new(1.6, egui::Color32::from_rgb(200, 90, 80)));
        }
        Md::FontColor => {
            // "A" con barra de color debajo (color de letra).
            p.text(egui::pos2(c.x, c.y - 2.0), egui::Align2::CENTER_CENTER, "A", egui::FontId::new(14.0, egui::FontFamily::Name("head".into())), col);
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(c.x - 8.0, c.y + 6.0), egui::pos2(c.x + 8.0, c.y + 9.0)), egui::CornerRadius::same(1), egui::Color32::from_rgb(210, 70, 60));
        }
        Md::HighlightColor => {
            // Marcador con barra de color (color de resaltado).
            let r = egui::Rect::from_center_size(egui::pos2(c.x, c.y - 2.0), egui::vec2(13.0, 9.0));
            p.rect_stroke(r, egui::CornerRadius::same(2), st, egui::StrokeKind::Inside);
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(c.x - 8.0, c.y + 6.0), egui::pos2(c.x + 8.0, c.y + 9.0)), egui::CornerRadius::same(1), egui::Color32::from_rgb(245, 210, 70));
        }
        Md::Undo | Md::Redo => {
            // Flecha curva (deshacer = izquierda, rehacer = derecha).
            let dir = if matches!(md, Md::Undo) { -1.0_f32 } else { 1.0 };
            let r = 5.5_f32;
            let arc: Vec<egui::Pos2> = (0..=9)
                .map(|i| {
                    let a = std::f32::consts::PI * (0.08 + 0.9 * i as f32 / 9.0);
                    egui::pos2(c.x - dir * r * a.cos(), c.y - 1.0 - r * a.sin())
                })
                .collect();
            p.add(egui::Shape::line(arc.clone(), st));
            // punta de flecha en el extremo inferior.
            if let Some(end) = arc.first() {
                p.line_segment([*end, egui::pos2(end.x + dir * 4.0, end.y - 1.0)], st);
                p.line_segment([*end, egui::pos2(end.x + dir * 1.0, end.y + 4.0)], st);
            }
        }
        Md::CodeBlock => {
            let r = egui::Rect::from_center_size(c, egui::vec2(17.0, 14.0));
            p.rect_stroke(r, egui::CornerRadius::same(3), st, egui::StrokeKind::Inside);
            let s2 = egui::Stroke::new(1.3, col);
            p.line_segment([egui::pos2(c.x - 2.0, c.y - 3.0), egui::pos2(c.x - 5.0, c.y)], s2);
            p.line_segment([egui::pos2(c.x - 5.0, c.y), egui::pos2(c.x - 2.0, c.y + 3.0)], s2);
            p.line_segment([egui::pos2(c.x + 2.0, c.y - 3.0), egui::pos2(c.x + 5.0, c.y)], s2);
            p.line_segment([egui::pos2(c.x + 5.0, c.y), egui::pos2(c.x + 2.0, c.y + 3.0)], s2);
        }
        Md::Callout => {
            let r = egui::Rect::from_center_size(c, egui::vec2(16.0, 13.0));
            p.rect_stroke(r, egui::CornerRadius::same(3), st, egui::StrokeKind::Inside);
            p.line_segment([egui::pos2(r.left() + 3.0, r.top() + 2.0), egui::pos2(r.left() + 3.0, r.bottom() - 2.0)], egui::Stroke::new(2.4, egui::Color32::from_rgb(90, 150, 220)));
            p.text(egui::pos2(c.x + 2.0, c.y), egui::Align2::CENTER_CENTER, "i", egui::FontId::new(11.0, egui::FontFamily::Name("serif_it".into())), col);
        }
        Md::Fullscreen => {
            // Cuatro esquinas (entrar a pantalla completa).
            let q = 6.0_f32;
            for (sx, sy) in [(-1.0_f32, -1.0_f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let cx = c.x + sx * q;
                let cy = c.y + sy * q;
                p.line_segment([egui::pos2(cx, cy), egui::pos2(cx - sx * 4.0, cy)], st);
                p.line_segment([egui::pos2(cx, cy), egui::pos2(cx, cy - sy * 4.0)], st);
            }
        }
        Md::Hn => {
            // "H" + chevron (mas encabezados).
            p.text(egui::pos2(c.x - 3.0, c.y), egui::Align2::CENTER_CENTER, "H", egui::FontId::new(13.0, head), col);
            p.line_segment([egui::pos2(c.x + 3.0, c.y - 1.0), egui::pos2(c.x + 6.0, c.y + 2.0)], st);
            p.line_segment([egui::pos2(c.x + 6.0, c.y + 2.0), egui::pos2(c.x + 9.0, c.y - 1.0)], st);
        }
        Md::Align => {
            // Lineas de alineacion + chevron.
            for (i, &y) in [c.y - 4.0, c.y - 1.0, c.y + 2.0].iter().enumerate() {
                let x1 = if i == 1 { c.x + 1.0 } else { c.x + 5.0 };
                p.line_segment([egui::pos2(c.x - 7.0, y), egui::pos2(x1, y)], st);
            }
            p.line_segment([egui::pos2(c.x + 2.0, c.y + 5.0), egui::pos2(c.x + 5.0, c.y + 8.0)], st);
            p.line_segment([egui::pos2(c.x + 5.0, c.y + 8.0), egui::pos2(c.x + 8.0, c.y + 5.0)], st);
        }
        Md::Image => {
            // Marco + montaña + sol (imagen).
            let r = egui::Rect::from_center_size(c, egui::vec2(16.0, 13.0));
            p.rect_stroke(r, egui::CornerRadius::same(2), st, egui::StrokeKind::Inside);
            p.circle_filled(egui::pos2(r.left() + 4.0, r.top() + 4.0), 1.6, col);
            p.add(egui::Shape::line(vec![egui::pos2(r.left() + 1.0, r.bottom() - 2.0), egui::pos2(r.left() + 6.0, r.center().y), egui::pos2(r.right() - 1.0, r.bottom() - 2.0)], st));
        }
        Md::Table => {
            let r = egui::Rect::from_center_size(c, egui::vec2(16.0, 13.0));
            p.rect_stroke(r, egui::CornerRadius::same(2), st, egui::StrokeKind::Inside);
            p.line_segment([egui::pos2(r.left(), r.center().y), egui::pos2(r.right(), r.center().y)], st);
            p.line_segment([egui::pos2(r.center().x, r.top()), egui::pos2(r.center().x, r.bottom())], st);
        }
    }
    resp.on_hover_text(tip)
}

/// Paleta de colores para texto/resaltado de la toolbar (negro, grises, y colores).
const MD_PALETTE: [[u8; 3]; 12] = [
    [30, 30, 34],
    [120, 120, 128],
    [210, 70, 60],
    [225, 130, 40],
    [220, 180, 40],
    [90, 170, 80],
    [50, 150, 150],
    [60, 120, 210],
    [120, 90, 200],
    [200, 90, 160],
    [150, 110, 70],
    [240, 240, 240],
];

/// Popup con una rejilla de swatches; devuelve el color elegido (si se pulso uno).
fn color_palette(ui: &mut egui::Ui) -> Option<[u8; 3]> {
    let mut chosen = None;
    ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
    egui::Grid::new("md_palette_grid").spacing(egui::vec2(4.0, 4.0)).show(ui, |ui| {
        for (i, rgb) in MD_PALETTE.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
            let c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            ui.painter().rect_filled(rect, egui::CornerRadius::same(4), c);
            if resp.hovered() {
                ui.painter().rect_stroke(rect, egui::CornerRadius::same(4), egui::Stroke::new(2.0, egui::Color32::WHITE), egui::StrokeKind::Outside);
            }
            if resp.clicked() {
                chosen = Some(*rgb);
            }
            if (i + 1) % 4 == 0 {
                ui.end_row();
            }
        }
    });
    chosen
}

/// Nivel de encabezado de una linea Markdown (1..3) y el indice de CARACTER donde empieza el
/// contenido (tras "# "). 0 = no es encabezado.
fn heading_level(line: &str) -> (u32, usize) {
    let mut hashes = 0usize;
    for ch in line.chars() {
        if ch == '#' {
            hashes += 1;
        } else {
            break;
        }
    }
    if (1..=6).contains(&hashes) && line.chars().nth(hashes) == Some(' ') {
        (hashes as u32, hashes + 1)
    } else {
        (0, 0)
    }
}

/// Convierte texto Markdown en un `LayoutJob` con FORMATO en vivo (negrita/cursiva/tachado +
/// encabezados). Conserva TODOS los caracteres (las marcas se ven atenuadas) para que el cursor
/// del editor siga mapeando bien. `base` = tamano de fuente; `fam` = familia base.
/// Parseo EN LINEA de Markdown: **negrita**, *cursiva*, __subrayado__, ~~tachado~~, ==resaltado==,
/// `codigo` y enlaces [texto](url). Conserva todos los caracteres (marcas atenuadas) para que el
/// cursor del editor siga mapeando bien. Anexa los tramos a `job` con su formato.
/// Quita las marcas Markdown y etiquetas HTML de un texto (para "limpiar formato").
fn strip_md(s: &str) -> String {
    // 1) quitar etiquetas <...>
    let mut no_tags = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => no_tags.push(ch),
            _ => {}
        }
    }
    // 2) quitar marcas de formato.
    no_tags
        .replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace("==", "")
        .replace('*', "")
        .replace('`', "")
}

/// Extrae un color `#RRGGBB` del primer `#` que aparezca en `s`.
fn parse_hex_color(s: &str) -> Option<egui::Color32> {
    let h = s.find('#')?;
    let hex: String = s[h + 1..].chars().take(6).collect();
    if hex.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}

/// Decoracion DIBUJADA sobre una linea (lo que sustituye a la marca oculta).
#[derive(Clone)]
enum Deco {
    Check(bool),
    Quote,
    Hr,
    /// Imagen `![alt](ruta)`: se carga y dibuja la textura en una fila alta.
    Image(String),
    /// Tabla Markdown: celdas (fila 0 = encabezado), y rango de CARACTERES del bloque en el
    /// texto (para editar: añadir columna/fila). `col_scale`/`row_scale` salen de la directiva
    /// `<!--tbl ...-->` que precede a la tabla (tamaño propio de cada tabla). Rejilla real.
    Table { cells: Vec<Vec<String>>, cstart: usize, cend: usize, col_scale: f32, row_scale: f32 },
}

/// Divide una fila Markdown `| a | b |` en celdas (sin los `|` de los extremos).
fn parse_table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// ¿La fila es la SEPARADORA de una tabla (`| --- | :--: |`)?
/// DEBE contener al menos un `-`; si no, una fila de celdas vacías `|  |  |` se
/// confundiria con la separadora y las filas de datos no se acumularian.
fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Serializa celdas a un bloque de tabla Markdown (con fila separadora tras el encabezado).
fn serialize_table(cells: &[Vec<String>]) -> String {
    if cells.is_empty() {
        return String::new();
    }
    let ncols = cells.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
    let mut out = String::new();
    for (i, row) in cells.iter().enumerate() {
        let mut line = String::from("|");
        for c in 0..ncols {
            let cell = row.get(c).map(|s| s.as_str()).unwrap_or("");
            line.push_str(&format!(" {cell} |"));
        }
        out.push_str(&line);
        out.push('\n');
        if i == 0 {
            // fila separadora tras el encabezado
            let mut sep = String::from("|");
            for _ in 0..ncols {
                sep.push_str(" --- |");
            }
            out.push_str(&sep);
            out.push('\n');
        }
    }
    out.pop(); // quitar el ultimo '\n'
    out
}

fn push_inline(
    job: &mut egui::text::LayoutJob,
    chars: &[char],
    size: f32,
    lh: f32,
    base_fam: &egui::FontFamily,
    base_col: egui::Color32,
    base_italic: bool,
    reveal: bool,
) {
    use egui::{Color32, FontFamily, FontId, Stroke, TextFormat};
    let head = FontFamily::Name("head".into());
    let mono = FontFamily::Monospace;
    let dim = Color32::from_rgba_unmultiplied(base_col.r(), base_col.g(), base_col.b(), 95);
    let link_col = Color32::from_rgb(58, 120, 210);
    let hi_bg = Color32::from_rgb(252, 232, 120);
    let code_bg = Color32::from_rgba_unmultiplied(130, 130, 140, 46);
    // Anexa una marca (los `*`, `_`, etc.): atenuada si `reveal` (linea del cursor); si no,
    // OCULTA (tamano ~0 + transparente) conservando el caracter para no descolocar el cursor.
    let mark = |job: &mut egui::text::LayoutJob, txt: &str| {
        if reveal {
            job.append(txt, 0.0, TextFormat { font_id: FontId::new(size, base_fam.clone()), color: dim, line_height: Some(lh), ..Default::default() });
        } else {
            job.append(txt, 0.0, TextFormat { font_id: FontId::new(0.01, base_fam.clone()), color: Color32::TRANSPARENT, line_height: Some(lh), ..Default::default() });
        }
    };
    // Anexa el texto acumulado con el estilo actual (con override de color/fondo por <span>/<mark>).
    let emit = |job: &mut egui::text::LayoutJob, run: &mut String, b: bool, it: bool, u: bool, s: bool, h: bool, c: bool, col_ovr: Option<Color32>, bg_ovr: Option<Color32>| {
        if run.is_empty() {
            return;
        }
        let fam = if c { &mono } else if b { &head } else { base_fam };
        let color = col_ovr.unwrap_or(base_col);
        let bg = bg_ovr.unwrap_or(if h { hi_bg } else if c { code_bg } else { Color32::TRANSPARENT });
        job.append(
            run,
            0.0,
            TextFormat {
                font_id: FontId::new(size, fam.clone()),
                color,
                background: bg,
                italics: it,
                underline: if u { Stroke::new(1.0, color) } else { Stroke::NONE },
                strikethrough: if s { Stroke::new((size * 0.06).max(1.0), color) } else { Stroke::NONE },
                line_height: Some(lh),
                ..Default::default()
            },
        );
        run.clear();
    };
    let (mut b, mut it, mut u, mut s, mut h, mut c) = (false, base_italic, false, false, false, false);
    let (mut cur_col, mut cur_bg): (Option<Color32>, Option<Color32>) = (None, None);
    let two = |chars: &[char], i: usize, a: char| i + 1 < chars.len() && chars[i] == a && chars[i + 1] == a;
    let mut i = 0;
    let mut run = String::new();
    while i < chars.len() {
        if chars[i] == '`' {
            emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
            mark(job, "`");
            c = !c;
            i += 1;
        } else if !c && two(chars, i, '*') {
            emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
            mark(job, "**");
            b = !b;
            i += 2;
        } else if !c && two(chars, i, '=') {
            emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
            mark(job, "==");
            h = !h;
            i += 2;
        } else if !c && two(chars, i, '_') {
            emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
            mark(job, "__");
            u = !u;
            i += 2;
        } else if !c && two(chars, i, '~') {
            emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
            mark(job, "~~");
            s = !s;
            i += 2;
        } else if !c && chars[i] == '*' {
            emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
            mark(job, "*");
            it = !it;
            i += 1;
        } else if !c && chars[i] == '[' {
            // Enlace [texto](url): buscar "](" y luego ")".
            let mut rb = None;
            let mut j = i + 1;
            while j + 1 < chars.len() {
                if chars[j] == ']' && chars[j + 1] == '(' {
                    rb = Some(j);
                    break;
                }
                if chars[j] == '[' {
                    break;
                }
                j += 1;
            }
            let mut handled = false;
            if let Some(rb) = rb {
                let mut k = rb + 2;
                while k < chars.len() && chars[k] != ')' {
                    k += 1;
                }
                if k < chars.len() {
                    emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
                    mark(job, "[");
                    let label: String = chars[i + 1..rb].iter().collect();
                    job.append(&label, 0.0, TextFormat { font_id: FontId::new(size, base_fam.clone()), color: link_col, underline: Stroke::new(1.0, link_col), line_height: Some(lh), ..Default::default() });
                    let tail: String = chars[rb..=k].iter().collect(); // "](url)"
                    mark(job, &tail);
                    i = k + 1;
                    handled = true;
                }
            }
            if !handled {
                run.push('[');
                i += 1;
            }
        } else if !c && chars[i] == '<' {
            // Etiqueta HTML: <span style="color:#hex"> / <mark style="background:#hex"> y cierres.
            let mut k = i + 1;
            while k < chars.len() && chars[k] != '>' {
                k += 1;
            }
            if k < chars.len() {
                emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
                let tag: String = chars[i..=k].iter().collect();
                mark(job, &tag);
                let tl = tag.to_ascii_lowercase();
                if tl.starts_with("</span") {
                    cur_col = None;
                } else if tl.starts_with("</mark") {
                    cur_bg = None;
                } else if tl.starts_with("<span") {
                    cur_col = parse_hex_color(&tag);
                } else if tl.starts_with("<mark") {
                    cur_bg = parse_hex_color(&tag);
                }
                i = k + 1;
            } else {
                run.push('<');
                i += 1;
            }
        } else {
            run.push(chars[i]);
            i += 1;
        }
    }
    emit(job, &mut run, b, it, u, s, h, c, cur_col, cur_bg);
}

/// Convierte Markdown en un `LayoutJob` con FORMATO en vivo estilo WYSIWYG: las marcas se
/// OCULTAN salvo en `reveal_line` (la linea del cursor, para editarlas). Devuelve tambien las
/// DECORACIONES a dibujar encima (casilla de tarea, barra de cita, regla) en el `char` indicado
/// (indice del primer caracter de la linea, para posicionar con `galley.pos_from_cursor`).
fn markdown_job(
    text: &str,
    base: f32,
    line_spacing: f32,
    fam: egui::FontFamily,
    col: egui::Color32,
    reveal_line: Option<usize>,
    align: u32,
) -> (egui::text::LayoutJob, Vec<(usize, Deco)>) {
    use egui::text::LayoutJob;
    use egui::{Color32, FontFamily, FontId, TextFormat};
    let head = FontFamily::Name("head".into());
    let dim = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 95);
    let accent = Color32::from_rgb(150, 120, 84);
    let quote_col = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 175);
    let mut job = LayoutJob::default();
    let mut decos: Vec<(usize, Deco)> = Vec::new();
    let plain = |job: &mut LayoutJob, txt: &str, size: f32, lh: f32, color: Color32| {
        job.append(txt, 0.0, TextFormat { font_id: FontId::new(size, fam.clone()), color, line_height: Some(lh), ..Default::default() });
    };
    // Reserva el ancho de un texto SIN mostrarlo (transparente, tamano normal): deja hueco
    // para dibujar la decoracion (casilla/barra) encima.
    let reserve = |job: &mut LayoutJob, txt: &str, size: f32, lh: f32| {
        job.append(txt, 0.0, TextFormat { font_id: FontId::new(size, fam.clone()), color: Color32::TRANSPARENT, line_height: Some(lh), ..Default::default() });
    };
    let code_bg = Color32::from_rgba_unmultiplied(130, 130, 140, 40);
    let mut in_code = false;
    // Las TABLAS siempre se dibujan como rejilla (nunca se ve el Markdown).
    let is_t = |s: &str| {
        let t = s.trim_start();
        t.starts_with('|') && t.len() > 1
    };
    let mut tbl: Option<(usize, Vec<Vec<String>>)> = None;
    // Dimensiones de la SIGUIENTE tabla (de su directiva `<!--tbl c=.. r=..-->`); por defecto
    // ancho medio y alto normal para tablas sin directiva (las viejas).
    let mut pending_dims: (f32, f32) = (0.5, 1.0);
    let mut char_pos = 0usize;
    for (li, line) in text.split('\n').enumerate() {
        let lh = base * line_spacing;
        if li > 0 {
            plain(&mut job, "\n", base, lh, col);
            char_pos += 1;
        }
        let reveal = reveal_line == Some(li);
        let start = char_pos;
        let chars: Vec<char> = line.chars().collect();
        char_pos += chars.len();
        let full: String = chars.iter().collect();
        // Si esta linea NO es de tabla, cerrar la tabla pendiente (emitir su rejilla).
        if !is_t(&full) {
            if let Some((cs, cells)) = tbl.take() {
                decos.push((cs, Deco::Table { cells, cstart: cs, cend: start.saturating_sub(1), col_scale: pending_dims.0, row_scale: pending_dims.1 }));
                pending_dims = (0.5, 1.0);
            }
        }
        // --- Directiva de tamaño de tabla `<!--tbl c=.. r=..-->`: oculta; fija las dimensiones
        //     de la tabla que viene justo despues (cada tabla guarda su propio tamaño) ---
        if full.trim_start().starts_with("<!--tbl") {
            job.append(&full, 0.0, TextFormat { font_id: FontId::new(0.01, fam.clone()), color: Color32::TRANSPARENT, line_height: Some(0.01), ..Default::default() });
            let num = |key: &str| -> Option<f32> {
                full.find(key).and_then(|i| full[i + key.len()..].split([' ', '>', '-']).next().and_then(|s| s.trim().parse::<f32>().ok()))
            };
            pending_dims = (num("c=").unwrap_or(0.5).clamp(0.0, 1.0), num("r=").unwrap_or(1.0).clamp(0.5, 3.0));
            continue;
        }
        // --- Bloque de codigo: vallas ``` (estado entre lineas) ---
        if full.trim() == "```" {
            if reveal {
                plain(&mut job, &full, base, lh, dim);
            } else {
                reserve(&mut job, &full, base, lh);
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            job.append(&full, 0.0, TextFormat { font_id: FontId::new(base, FontFamily::Monospace), color: col, background: code_bg, line_height: Some(lh), ..Default::default() });
            continue;
        }
        // --- Imagen ![alt](ruta): fila alta + textura dibujada (salvo en la linea del cursor) ---
        if !reveal && full.starts_with("![") && full.contains("](") && full.ends_with(')') {
            if let Some(open) = full.find("](") {
                let path = full[open + 2..full.len() - 1].to_string();
                job.append(&full, 0.0, TextFormat { font_id: FontId::new(0.01, fam.clone()), color: Color32::TRANSPARENT, line_height: Some(base * 9.0), ..Default::default() });
                decos.push((start, Deco::Image(path)));
                continue;
            }
        }
        // --- Tabla (| ... |): se OCULTA el Markdown y se dibuja la rejilla (decoracion) ---
        if is_t(&full) {
            let is_sep = is_table_separator(&full);
            // Reservar el alto real de fila (segun la directiva) para que el texto que sigue no
            // se solape con la rejilla.
            let rh = if is_sep { 0.5 } else { base * 1.9 * pending_dims.1 };
            job.append(&full, 0.0, TextFormat { font_id: FontId::new(0.01, fam.clone()), color: Color32::TRANSPARENT, line_height: Some(rh), ..Default::default() });
            if tbl.is_none() {
                tbl = Some((start, Vec::new()));
            }
            if !is_sep {
                if let Some((_, cells)) = tbl.as_mut() {
                    cells.push(parse_table_row(&full));
                }
            }
            continue;
        }
        // --- Encabezado ---
        let (hl, cstart) = heading_level(line);
        if hl > 0 {
            let lsize = match hl {
                1 => base * 1.7,
                2 => base * 1.4,
                3 => base * 1.18,
                4 => base * 1.06,
                5 => base * 0.96,
                _ => base * 0.9,
            };
            let lh2 = lsize * line_spacing;
            let prefix: String = chars[..cstart].iter().collect();
            if reveal {
                plain(&mut job, &prefix, lsize, lh2, dim);
            } else {
                job.append(&prefix, 0.0, TextFormat { font_id: FontId::new(0.01, fam.clone()), color: Color32::TRANSPARENT, line_height: Some(lh2), ..Default::default() });
            }
            push_inline(&mut job, &chars[cstart..], lsize, lh2, &head, col, false, reveal);
            continue;
        }
        // --- Regla horizontal (linea solo de '-', 3+) ---
        if chars.len() >= 3 && chars.iter().all(|&c| c == '-') {
            let bar: String = chars.iter().collect();
            if reveal {
                plain(&mut job, &bar, base, lh, dim);
            } else {
                reserve(&mut job, &bar, base, lh); // fila vacia; la linea se dibuja como decoracion
                decos.push((start, Deco::Hr));
            }
            continue;
        }
        // --- Cita ("> ") ---
        if chars.first() == Some(&'>') {
            let cs = if chars.get(1) == Some(&' ') { 2 } else { 1 };
            let prefix: String = chars[..cs].iter().collect();
            if reveal {
                plain(&mut job, &prefix, base, lh, accent);
            } else {
                reserve(&mut job, &prefix, base, lh); // hueco para la barra
                decos.push((start, Deco::Quote));
            }
            push_inline(&mut job, &chars[cs..], base, lh, &fam, quote_col, true, reveal);
            continue;
        }
        // --- Tarea ("- [ ] " / "- [x] ") ---
        if chars.len() >= 6 && chars[0] == '-' && chars[1] == ' ' && chars[2] == '[' && chars[4] == ']' && chars[5] == ' ' && (chars[3] == ' ' || chars[3] == 'x' || chars[3] == 'X') {
            let done = chars[3] != ' ';
            let prefix: String = chars[..6].iter().collect();
            if reveal {
                plain(&mut job, &prefix, base, lh, dim);
            } else {
                reserve(&mut job, &prefix, base, lh); // hueco para la casilla
                decos.push((start, Deco::Check(done)));
            }
            push_inline(&mut job, &chars[6..], base, lh, &fam, if done { quote_col } else { col }, false, reveal);
            continue;
        }
        // --- Vineta ("- " / "* ") -> bullet ---
        if (chars.first() == Some(&'-') || chars.first() == Some(&'*')) && chars.get(1) == Some(&' ') {
            if reveal {
                plain(&mut job, &chars[0].to_string(), base, lh, accent);
            } else {
                plain(&mut job, "•", base, lh, accent);
            }
            plain(&mut job, " ", base, lh, col);
            push_inline(&mut job, &chars[2..], base, lh, &fam, col, false, reveal);
            continue;
        }
        // --- Lista numerada ("N. ") ---
        let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && chars.get(digits) == Some(&'.') && chars.get(digits + 1) == Some(&' ') {
            let num: String = chars[..=digits].iter().collect(); // "N."
            plain(&mut job, &num, base, lh, accent);
            plain(&mut job, " ", base, lh, col);
            push_inline(&mut job, &chars[digits + 2..], base, lh, &fam, col, false, reveal);
            continue;
        }
        // --- Linea normal ---
        push_inline(&mut job, &chars, base, lh, &fam, col, false, reveal);
    }
    if let Some((cs, cells)) = tbl {
        decos.push((cs, Deco::Table { cells, cstart: cs, cend: char_pos, col_scale: pending_dims.0, row_scale: pending_dims.1 }));
    }
    job.halign = match align {
        1 => egui::Align::Center,
        2 => egui::Align::RIGHT,
        _ => egui::Align::LEFT,
    };
    job.justify = align == 3;
    (job, decos)
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
