//! UI estilo Concepts: una RUEDA RADIAL flotante, pequena, arrastrable y con los
//! mismos controles que Concepts en el donut interior:
//!   - arriba  = GROSOR (pts)
//!   - derecha = OPACIDAD (%)
//!   - izquierda = SUAVIDAD (%)
//! Arrastra cada zona para ajustar su valor; arrastra el centro para MOVER la rueda;
//! toca el centro para abrir la espiral de color tipo COPIC. Los segmentos exteriores
//! son presets de pincel/herramientas. Debajo, la lista de opciones.

#![allow(dead_code)]

use egui::{Align2, Color32, FontId, Pos2, Shape, Stroke, Vec2};
use ink_core::{Brush, Document};

/// Paleta de colores rapidos (teclas 1..8 en main).
pub const PALETTE: [[f32; 4]; 8] = [
    [0.10, 0.10, 0.13, 1.0],
    [0.85, 0.18, 0.18, 1.0],
    [0.18, 0.45, 0.90, 1.0],
    [0.15, 0.62, 0.35, 1.0],
    [0.95, 0.62, 0.10, 1.0],
    [0.60, 0.25, 0.75, 1.0],
    [0.95, 0.35, 0.60, 1.0],
    [0.18, 0.70, 0.75, 1.0],
];

#[derive(Clone, Copy)]
pub struct Stats {
    pub fps: f32,
    pub strokes: usize,
    pub verts: usize,
    pub zoom: f32,
    pub can_undo: bool,
    pub can_redo: bool,
}

/// Panel de ajuste desplegado al tocar un control de la rueda (estilo Concepts).
#[derive(Clone, Copy, PartialEq)]
pub enum Popup {
    None,
    Size,
    Smoothing,
    Opacity,
}

/// Modo del selector de color: paleta COPIC, o sliders HSL / RGB en arco.
#[derive(Clone, Copy, PartialEq)]
pub enum ColorMode {
    Copic,
    Hsl,
    Rgb,
}

/// Pestana activa del panel de ajustes.
#[derive(Clone, Copy, PartialEq)]
pub enum SettingsTab {
    Workspace,
    Interaction,
}

pub struct UiState {
    pub open: bool,
    pub selected_seg: usize,
    pub show_colors: bool,
    pub grid: bool,
    pub snap: bool,
    pub measure: bool,
    pub wheel_pos: Option<Pos2>, // esquina sup-izq del area de la rueda (arrastrable)
    pub dragging: bool,          // moviendo la rueda
    pub rotating: bool,          // girando el aro exterior
    pub wheel_rot: f32,          // rotacion del aro exterior (grados)
    pub popup: Popup,
    pub brush_panel: bool,       // panel "Mis pinceles" abierto
    pub editing_slot: usize,     // slot que el panel esta editando
    pub slots: [SlotItem; N_SEG],
    pub color_mode: ColorMode,   // COPIC / HSL / RGB
    pub eyedropper: bool,        // cuentagotas activo (siguiente clic toma color)
    pub show_settings: bool,     // panel de ajustes abierto
    pub settings_tab: SettingsTab,
    pub grid_editor: bool,       // sub-panel "Editar cuadricula" abierto
    pub show_layers: bool,       // panel de capas abierto
    pub show_ps_panel: bool,     // selector de pinceles de Photoshop desplegado
    pub last_grid: ink_core::GridKind, // ultimo tipo de cuadricula (para alternar)
    pub collapsed: bool,         // rueda oculta (solo queda el circulo de color)
    pub spiral_rot: f32,         // rotacion de la espiral de colores COPIC (grados)
    pub copic_pop: Option<(usize, usize)>, // swatch COPIC recien elegido (para el "salto")
    pub copic_pop_t: f64,        // tiempo (s) de esa eleccion, para animar el salto
    pub copic_close_at: Option<f64>, // instante (s) en que cerrar el selector COPIC tras el salto
    pub active_arc: Option<usize>, // banda HSL/RGB que se esta arrastrando (no saltar de banda)
    pub undo_press: f64,         // tiempo (s) del ultimo clic en deshacer (animacion de pulsacion)
    pub redo_press: f64,         // tiempo (s) del ultimo clic en rehacer
    pub dot_drag: bool,          // arrastrando el punto colapsado (solo si el gesto empezo dentro)
    /// TextureId de la miniatura de cada pincel de Photoshop (paralelo al catalogo
    /// `ps_brushes`), para dibujar los slots que contengan un `PsBrush`. `None` = aun
    /// no se ha subido a egui.
    pub ps_thumb_ids: Vec<Option<egui::TextureId>>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            open: true,
            selected_seg: 0,
            show_colors: false,
            grid: true,
            snap: false,
            measure: false,
            wheel_pos: None,
            dragging: false,
            rotating: false,
            wheel_rot: 0.0,
            popup: Popup::None,
            brush_panel: false,
            editing_slot: 0,
            slots: [
                SlotItem::Brush(0),  // Pluma (arriba)
                SlotItem::Brush(1),  // Fuente
                SlotItem::Tool(0),   // Seleccion
                SlotItem::Brush(2),  // Pluma dinamica
                SlotItem::Tool(1),   // Empujar
                SlotItem::Brush(7),  // Rotulador
                SlotItem::Brush(8),  // Acuarela
                SlotItem::Empty,     // vacio (X)
                SlotItem::Brush(3),  // Ancho fijo
            ],
            color_mode: ColorMode::Copic,
            eyedropper: false,
            show_settings: false,
            settings_tab: SettingsTab::Workspace,
            grid_editor: false,
            show_layers: false,
            show_ps_panel: false,
            last_grid: ink_core::GridKind::Squares,
            collapsed: false,
            spiral_rot: 0.0,
            copic_pop: None,
            copic_pop_t: 0.0,
            copic_close_at: None,
            active_arc: None,
            undo_press: -1.0,
            redo_press: -1.0,
            dot_drag: false,
            ps_thumb_ids: Vec::new(),
        }
    }
}

impl UiState {
    /// Solo se dibuja si el slot seleccionado es un PINCEL (no vacio, no herramienta).
    pub fn drawing_enabled(&self) -> bool {
        matches!(self.slots.get(self.selected_seg), Some(SlotItem::Brush(_)))
    }

    /// Si el slot seleccionado es una herramienta, devuelve su motor.
    pub fn active_tool(&self) -> Option<ink_core::Tool> {
        match self.slots.get(self.selected_seg) {
            Some(SlotItem::Tool(ti)) => Some(tool_kind_for(TOOLS[*ti])),
            _ => None,
        }
    }
}

/// Nombre legible del tipo de cuadricula (para el panel inferior).
fn grid_name(g: ink_core::GridKind) -> &'static str {
    use ink_core::GridKind::*;
    match g {
        None => "Sin cuadrícula",
        Dots => "Cuadrícula de puntos",
        Squares => "Papel milimetrado",
        Lines => "Papel rayado",
        Iso => "Cuadrícula isométrica",
        Triangle => "Triángulo",
        P1 => "1 punto",
        P2 => "2 puntos",
        P3 => "3 puntos",
    }
}

/// Motor (comportamiento real) correspondiente a cada herramienta.
fn tool_kind_for(name: &str) -> ink_core::Tool {
    use ink_core::Tool::*;
    match name {
        "Empujar" => Push,
        "Sector" => Sector,
        "Máscara dura" => MaskHard,
        "Máscara suave" => MaskSoft,
        "Texto" => Text,
        _ => Select, // Seleccion
    }
}

/// Pinceles y herramientas del panel "Mis pinceles" (estilo Concepts).
const BRUSHES: [&str; 12] = [
    "Pluma", "Fuente", "Pluma dinámica", "Ancho fijo", "Cable", "Lápiz suave",
    "Lápiz duro", "Rotulador", "Acuarela", "Aerógrafo", "Rellenar", "Punteado",
];
const TOOLS: [&str; 6] = ["Selección", "Empujar", "Sector", "Máscara dura", "Máscara suave", "Texto"];

fn brush_width_for(name: &str) -> f32 {
    match name {
        "Cable" | "Lápiz duro" => 2.0,
        "Lápiz suave" | "Ancho fijo" => 3.0,
        "Punteado" => 6.0,
        "Rotulador" => 10.0,
        "Aerógrafo" => 12.0,
        "Acuarela" => 16.0,
        "Rellenar" => 22.0,
        _ => 4.0,
    }
}

/// Motor de dibujo correspondiente a cada pincel.
fn brush_kind_for(name: &str) -> ink_core::BrushKind {
    use ink_core::BrushKind::*;
    match name {
        "Ancho fijo" | "Cable" => FixedWidth,
        "Rotulador" => Marker,
        "Lápiz suave" | "Lápiz duro" => Pencil,
        "Acuarela" | "Rellenar" => Watercolor,
        "Aerógrafo" => Airbrush,
        "Punteado" => Dotted,
        _ => Pen, // Pluma, Fuente, Pluma dinamica...
    }
}

#[derive(Default, Clone, Copy)]
pub struct UiActions {
    pub undo: bool,
    pub redo: bool,
    pub clear: bool,
    pub layers_dirty: bool, // el panel de capas modifico el documento (re-subir malla)
    /// Se selecciono un slot de la RUEDA con pincel procedural/herramienta: hay que
    /// salir del pincel de Photoshop y volver al pincel del slot.
    pub exit_ps: bool,
    /// Se selecciono un slot de la rueda que contiene un pincel de Photoshop: activarlo.
    pub activate_ps: Option<u32>,
    /// Slot recien seleccionado/asignado en la rueda. main.rs lo usa para restaurar la
    /// ultima configuracion (tamano/opacidad/suavidad) guardada de ese item.
    pub slot_selected: Option<usize>,
}

// --- Geometria de la rueda (mas pequena que antes) ---
const R_OUT: f32 = 112.0;
const R_MID: f32 = 70.0;
const R_HOLE: f32 = 28.0;
const N_SEG: usize = 9;
const SEG_DEG: f32 = 360.0 / N_SEG as f32;

/// Contenido de un slot de la rueda: vacio (X), un pincel, una herramienta o un
/// pincel de Photoshop (indice en el catalogo `ps_brushes`).
#[derive(Clone, Copy, PartialEq)]
pub enum SlotItem {
    Empty,
    Brush(usize), // indice en BRUSHES
    Tool(usize),  // indice en TOOLS
    PsBrush(u32), // indice en el catalogo de pinceles de Photoshop
    Eraser,       // goma (borrador por zona); usa el tamano/opacidad de la rueda
}

fn c32(c: [f32; 4]) -> Color32 {
    Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
}

fn dir(deg: f32) -> Vec2 {
    let r = deg.to_radians();
    egui::vec2(r.sin(), -r.cos())
}

fn num_label(w: f32) -> String {
    let r = (w * 10.0).round() / 10.0;
    if (r.fract()).abs() < 0.05 {
        format!("{}", r.round() as i32)
    } else {
        format!("{r:.1}")
    }
}

fn fill_sector(p: &egui::Painter, c: Pos2, r0: f32, r1: f32, a0: f32, a1: f32, fill: Color32) {
    let steps = 24;
    for i in 0..steps {
        let t0 = a0 + (a1 - a0) * (i as f32 / steps as f32);
        let t1 = a0 + (a1 - a0) * ((i + 1) as f32 / steps as f32);
        let quad = vec![c + dir(t0) * r1, c + dir(t1) * r1, c + dir(t1) * r0, c + dir(t0) * r0];
        p.add(Shape::convex_polygon(quad, fill, Stroke::NONE));
    }
}

// --- Iconos vectoriales ---
fn icon_wave(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    let n = 16;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            c + egui::vec2(-s + 2.0 * s * t, (t * std::f32::consts::TAU).sin() * s * 0.45)
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(2.0, col)));
}

fn icon_arrow(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    let a = c + egui::vec2(-s * 0.7, s * 0.7);
    let b = c + egui::vec2(s * 0.7, -s * 0.7);
    p.line_segment([a, b], Stroke::new(2.0, col));
    p.line_segment([b, b + egui::vec2(-s * 0.7, 0.0)], Stroke::new(2.0, col));
    p.line_segment([b, b + egui::vec2(0.0, s * 0.7)], Stroke::new(2.0, col));
}

fn icon_x(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    p.line_segment([c + egui::vec2(-s, -s), c + egui::vec2(s, s)], Stroke::new(2.4, col));
    p.line_segment([c + egui::vec2(-s, s), c + egui::vec2(s, -s)], Stroke::new(2.4, col));
}

fn icon_fill(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    let pts = vec![
        c + egui::vec2(-s, 0.0),
        c + egui::vec2(0.0, -s),
        c + egui::vec2(s, 0.0),
        c + egui::vec2(0.0, s),
    ];
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn icon_grip(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    for k in -1..=1 {
        let y = c.y + k as f32 * (s * 0.5);
        p.line_segment([egui::pos2(c.x - s, y), egui::pos2(c.x + s, y)], Stroke::new(1.8, col));
    }
}

fn icon_opacity(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    p.circle_stroke(c, s, Stroke::new(1.8, col));
    let n = 18;
    let mut pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (i as f32 / n as f32);
            c + egui::vec2(a.cos() * s, a.sin() * s)
        })
        .collect();
    pts.push(c);
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn icon_brush_sample(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    let n = 14;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            c + egui::vec2(-s + 2.0 * s * t, (t * 6.5).sin() * s * 0.35)
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(3.0, col)));
}

fn icon_curved_arrow(p: &egui::Painter, c: Pos2, s: f32, col: Color32, mirror: bool) {
    // Deshacer: flecha curva que apunta a la izquierda. Rehacer (mirror): a la derecha.
    let g = if mirror { -1.0 } else { 1.0 };
    let tail = vec![
        c + egui::vec2(g * s * 0.95, -s * 0.75),
        c + egui::vec2(g * s * 0.45, -s * 0.25),
        c + egui::vec2(g * -s * 0.2, s * 0.02),
        c + egui::vec2(g * -s * 0.8, s * 0.12),
    ];
    p.add(Shape::line(tail.clone(), Stroke::new(3.2, col)));
    let tip = *tail.last().unwrap();
    let head = vec![
        tip + egui::vec2(g * -s * 0.55, 0.0),
        tip + egui::vec2(g * 0.18 * s, -s * 0.5),
        tip + egui::vec2(g * 0.18 * s, s * 0.5),
    ];
    p.add(Shape::convex_polygon(head, col, Stroke::NONE));
}

fn icon_eraser(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    // Goma inclinada (bloque con banda), como en apps de dibujo.
    let body = vec![
        c + egui::vec2(-s, s * 0.45),
        c + egui::vec2(s * 0.3, -s),
        c + egui::vec2(s, -s * 0.45),
        c + egui::vec2(-s * 0.3, s),
    ];
    p.add(Shape::convex_polygon(body, Color32::from_gray(236), Stroke::new(1.8, col)));
    p.line_segment(
        [c + egui::vec2(-s * 0.65, 0.0), c + egui::vec2(s * 0.65, -s * 0.45)],
        Stroke::new(1.5, col),
    );
}

pub fn icon_dropper(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    // Cuentagotas (pipeta) sobre el eje diagonal: gota abajo-izq, tubo, bulbo arriba-der.
    let u = egui::vec2(0.7071, -0.7071); // hacia arriba-derecha (cuerpo)
    let n = egui::vec2(0.7071, 0.7071); // perpendicular
    let tip = c - u * s; // punta (gota)
                         // Tubo fino.
    p.line_segment([c - u * (s * 0.45), c + u * (s * 0.3)], Stroke::new(s * 0.32, col));
    // Bulbo: capsula gruesa alineada al eje.
    p.line_segment([c + u * (s * 0.28), c + u * (s * 0.92)], Stroke::new(s * 0.72, col));
    // Anillo/cuello entre tubo y bulbo.
    p.line_segment(
        [c - u * (s * 0.5) + n * (s * 0.28), c - u * (s * 0.5) - n * (s * 0.28)],
        Stroke::new(s * 0.16, col),
    );
    // Punta-gota triangular.
    p.add(Shape::convex_polygon(
        vec![tip, c - u * (s * 0.5) + n * (s * 0.16), c - u * (s * 0.5) - n * (s * 0.16)],
        col,
        Stroke::NONE,
    ));
}

/// Texto rotado para leer radialmente (codigos COPIC en los swatches).
fn draw_radial_text(p: &egui::Painter, center: Pos2, theta_deg: f32, text: &str, color: Color32) {
    let d = dir(theta_deg);
    let mut angle = d.y.atan2(d.x);
    if angle.abs() > std::f32::consts::FRAC_PI_2 {
        angle += std::f32::consts::PI; // evita texto al reves en la mitad izquierda
    }
    let galley = p.layout_no_wrap(text.to_owned(), FontId::proportional(9.0), color);
    let sz = galley.size();
    let rot = egui::emath::Rot2::from_angle(angle);
    let pos = center - rot * egui::vec2(sz.x / 2.0, sz.y / 2.0);
    let mut ts = egui::epaint::TextShape::new(pos, galley, color);
    ts.angle = angle;
    p.add(Shape::Text(ts));
}

// --- Vistas previas de pinceles/herramientas para "Mis pinceles" ---
fn preview_wave(p: &egui::Painter, c: Pos2, w: f32, thick: f32, col: Color32) {
    let n = 20;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            c + egui::vec2(-w + 2.0 * w * t, (t * 7.0).sin() * w * 0.26)
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(thick, col)));
}

/// Onda con grosor VARIABLE (fino en los extremos, grueso en el centro): imita el
/// trazo de una pluma sensible a la presion.
fn preview_wave_taper(p: &egui::Painter, c: Pos2, w: f32, max_thick: f32, col: Color32) {
    let n = 22;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            c + egui::vec2(-w + 2.0 * w * t, (t * 7.0).sin() * w * 0.26)
        })
        .collect();
    for i in 0..n {
        let t = (i as f32 + 0.5) / n as f32;
        let taper = (t * std::f32::consts::PI).sin(); // 0 en extremos -> 1 en el centro
        let thick = (0.7 + (max_thick - 0.7) * taper).max(0.5);
        p.line_segment([pts[i], pts[i + 1]], Stroke::new(thick, col));
    }
}

/// Mini "spray" de aerografo: puntos dispersos a lo largo de una linea.
fn preview_spray(p: &egui::Painter, c: Pos2, w: f32, col: Color32) {
    for i in 0..22 {
        let t = i as f32 / 21.0;
        let x = -w + 2.0 * w * t;
        let y = (i as f32 * 2.3).sin() * 5.0 + (i as f32 * 0.9).cos() * 2.5;
        let a = (140.0 + (i as f32 * 1.3).sin() * 90.0).clamp(40.0, 230.0) as u8;
        let cc = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a);
        p.circle_filled(c + egui::vec2(x, y), 1.0, cc);
    }
}

fn preview_blob(p: &egui::Painter, c: Pos2, rx: f32, ry: f32, col: Color32) {
    let n = 26;
    let pts: Vec<Pos2> = (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            let rr = 1.0 + 0.18 * (a * 3.0).sin();
            c + egui::vec2(a.cos() * rx * rr, a.sin() * ry * rr)
        })
        .collect();
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn preview_dots(p: &egui::Painter, c: Pos2, w: f32, col: Color32) {
    for i in 0..8 {
        let t = i as f32 / 7.0;
        p.circle_filled(c + egui::vec2(-w + 2.0 * w * t, (i as f32 * 1.7).sin() * 7.0), 2.2, col);
    }
}

fn icon_select_cursor(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    let pts = vec![
        c + egui::vec2(-s * 0.6, -s),
        c + egui::vec2(-s * 0.6, s * 0.6),
        c + egui::vec2(s * 0.15, -s * 0.15),
    ];
    p.add(Shape::convex_polygon(pts, col, Stroke::NONE));
}

fn icon_sector(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    let quad = vec![
        c + egui::vec2(-s, s * 0.45),
        c + egui::vec2(s * 0.4, -s * 0.65),
        c + egui::vec2(s, -s * 0.15),
        c + egui::vec2(-s * 0.4, s * 0.95),
    ];
    p.add(Shape::closed_line(quad, Stroke::new(1.8, col)));
    p.add(Shape::convex_polygon(
        vec![c + egui::vec2(-s, s * 0.45), c + egui::vec2(s * 0.4, -s * 0.65), c + egui::vec2(-s * 0.35, s * 0.18)],
        col,
        Stroke::NONE,
    ));
}

fn icon_mask(p: &egui::Painter, c: Pos2, s: f32, col: Color32, soft: bool) {
    let r = egui::Rect::from_center_size(c, egui::vec2(2.2 * s, 1.4 * s));
    let rr = if soft {
        egui::CornerRadius::same((s * 0.7) as u8)
    } else {
        egui::CornerRadius::same(2)
    };
    p.rect_stroke(r, rr, Stroke::new(1.8, col), egui::StrokeKind::Inside);
    let lh = egui::Rect::from_min_max(r.min, egui::pos2(c.x, r.max.y));
    p.rect_filled(lh, rr, col);
}

fn draw_preview(p: &egui::Painter, c: Pos2, name: &str) {
    let ink = Color32::from_gray(28);
    match name {
        "Pluma" => preview_wave(p, c, 30.0, 4.0, ink),
        "Fuente" => preview_wave(p, c, 30.0, 3.4, ink),
        "Pluma dinámica" => preview_wave(p, c, 30.0, 4.8, ink),
        "Ancho fijo" => preview_wave(p, c, 30.0, 3.0, ink),
        "Cable" => preview_wave(p, c, 30.0, 1.6, ink),
        "Lápiz suave" => preview_wave(p, c, 28.0, 2.4, Color32::from_gray(95)),
        "Lápiz duro" => preview_wave(p, c, 28.0, 1.8, Color32::from_gray(55)),
        "Rotulador" => preview_wave(p, c, 30.0, 8.0, ink),
        "Acuarela" => preview_blob(p, c, 22.0, 12.0, Color32::from_gray(80)),
        "Aerógrafo" => preview_blob(p, c, 20.0, 11.0, Color32::from_gray(130)),
        "Rellenar" => preview_blob(p, c, 24.0, 13.0, ink),
        "Punteado" => preview_dots(p, c, 26.0, ink),
        "Selección" => icon_select_cursor(p, c, 13.0, ink),
        "Empujar" => preview_wave(p, c, 24.0, 2.6, ink),
        "Sector" => icon_sector(p, c, 14.0, ink),
        "Máscara dura" => icon_mask(p, c, 13.0, ink, false),
        "Máscara suave" => icon_mask(p, c, 13.0, ink, true),
        "Texto" => {
            p.text(c, Align2::CENTER_CENTER, "Aa", FontId::proportional(26.0), ink);
        }
        "Goma" => icon_eraser(p, c, 16.0, ink),
        _ => preview_wave(p, c, 28.0, 3.0, ink),
    }
}

/// Mini-trazo de un pincel/herramienta para el slot de la rueda. Cada PINCEL muestra
/// un trazo con SU estilo (grosor/forma/textura propios) para distinguirlos de un
/// vistazo, igual que el panel "Mis pinceles" pero en pequeno.
fn draw_wheel_item(p: &egui::Painter, pos: Pos2, name: &str, col: Color32) {
    let w = 12.0; // medio-ancho del mini trazo
    match name {
        // --- Pinceles: cada uno con su trazo caracteristico ---
        "Pluma" => preview_wave(p, pos, w, 3.0, col),
        "Fuente" => preview_wave(p, pos, w, 2.2, col),
        "Pluma dinámica" => preview_wave_taper(p, pos, w, 4.6, col),
        "Ancho fijo" => preview_wave(p, pos, w, 2.4, col),
        "Cable" => preview_wave(p, pos, w, 1.2, col),
        "Lápiz suave" => preview_pencil(p, pos, w, 2.4, col),
        "Lápiz duro" => preview_pencil(p, pos, w, 1.5, col),
        "Rotulador" => preview_wave(p, pos, w, 6.0, col),
        "Acuarela" | "Rellenar" => preview_blob(p, pos, 10.0, 6.0, col),
        "Aerógrafo" => preview_spray(p, pos, w, col),
        "Punteado" => {
            for i in 0..5 {
                let x = -9.0 + 18.0 * (i as f32 / 4.0);
                p.circle_filled(pos + egui::vec2(x, (i as f32 * 1.6).sin() * 3.0), 1.6, col);
            }
        }
        // --- Herramientas: sus iconos ---
        "Selección" => icon_select_cursor(p, pos, 9.0, col),
        "Empujar" => icon_wave(p, pos, 9.0, col),
        "Sector" => icon_sector(p, pos, 9.0, col),
        "Máscara dura" => icon_mask(p, pos, 8.0, col, false),
        "Máscara suave" => icon_mask(p, pos, 8.0, col, true),
        "Texto" => {
            p.text(pos, Align2::CENTER_CENTER, "Aa", FontId::proportional(15.0), col);
        }
        _ => preview_wave(p, pos, w, 3.0, col),
    }
}

/// Mini-trazo de lapiz: onda con granitos a lo largo (textura granulada).
fn preview_pencil(p: &egui::Painter, c: Pos2, w: f32, thick: f32, col: Color32) {
    preview_wave(p, c, w, thick, col);
    for i in 0..10 {
        let t = i as f32 / 9.0;
        let x = -w + 2.0 * w * t;
        let y = (t * 7.0).sin() * w * 0.26 + (i as f32 * 2.1).cos() * 1.6;
        let a = (90.0 + (i as f32 * 1.7).sin() * 70.0).clamp(30.0, 180.0) as u8;
        p.circle_filled(c + egui::vec2(x, y), 0.8, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a));
    }
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s < 1e-6 {
        return (l, l, l);
    }
    let h = h.rem_euclid(360.0) / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |t: f32| -> f32 {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

/// Curva de "pulsacion" de un boton (sube y baja en ~0.22 s) para dar la
/// sensacion de que se presiona al hacer clic.
fn press_factor(age: f32) -> f32 {
    if age < 0.0 || age > 0.22 {
        0.0
    } else {
        (age / 0.22 * std::f32::consts::PI).sin()
    }
}

/// Curva de "salto" de un swatch COPIC recien elegido (sube y baja en ~0.42 s).
fn copic_pop_factor(age: f32) -> f32 {
    if age < 0.0 || age > 0.42 {
        0.0
    } else {
        (age / 0.42 * std::f32::consts::PI).sin()
    }
}

/// Dibuja un slider en arco con gradiente, tirador y cajita de valor.
/// `t` = animacion de aparicion (0..1, alfa). `value` = posicion del tirador (0..1).
fn draw_arc(p: &egui::Painter, c: Pos2, radius: f32, t: f32, value: f32, grad: &dyn Fn(f32) -> [u8; 3]) {
    let alpha = (t.clamp(0.0, 1.0) * 255.0) as u8;
    let thick = 7.0;
    let hw = thick * 0.5;
    let n = 110; // bastantes segmentos -> banda suave y sin huecos en las uniones
    // Tapa redondeada del extremo inicial (debajo de la banda).
    let ca = grad(0.0);
    p.circle_filled(c + dir(SP_START) * radius, hw, Color32::from_rgba_unmultiplied(ca[0], ca[1], ca[2], alpha));
    for i in 0..n {
        let s0 = i as f32 / n as f32;
        let s1 = (i + 1) as f32 / n as f32;
        let col = grad((s0 + s1) * 0.5);
        let stroke = Stroke::new(thick, Color32::from_rgba_unmultiplied(col[0], col[1], col[2], alpha));
        p.line_segment(
            [c + dir(SP_START + SP_SWEEP * s0) * radius, c + dir(SP_START + SP_SWEEP * s1) * radius],
            stroke,
        );
        // Circulo en cada union para redondear y tapar huecos (cap redondo continuo).
        p.circle_filled(c + dir(SP_START + SP_SWEEP * s1) * radius, hw, Color32::from_rgba_unmultiplied(col[0], col[1], col[2], alpha));
    }
    let ha = SP_START + SP_SWEEP * value.clamp(0.0, 1.0);
    let hp = c + dir(ha) * radius;
    p.circle_filled(hp, hw + 2.5, Color32::from_rgba_unmultiplied(255, 255, 255, alpha));
    p.circle_stroke(hp, hw + 2.5, Stroke::new(2.0, Color32::from_rgba_unmultiplied(70, 70, 70, alpha)));
}

/// Cajita con el valor de una banda. Se dibuja DESPUES de todas las bandas para
/// que los valores queden por encima y no los tape otra banda.
fn draw_arc_value(p: &egui::Painter, c: Pos2, radius: f32, value: f32, label: &str, t: f32) {
    let alpha = (t.clamp(0.0, 1.0) * 255.0) as u8;
    let ha = SP_START + SP_SWEEP * value.clamp(0.0, 1.0);
    let bp = c + dir(ha) * (radius + 22.0);
    let r = egui::Rect::from_center_size(bp, egui::vec2(48.0, 26.0));
    p.rect_filled(r, egui::CornerRadius::same(5), Color32::from_rgba_unmultiplied(255, 255, 255, alpha));
    p.rect_stroke(r, egui::CornerRadius::same(5), Stroke::new(1.0, Color32::from_rgba_unmultiplied(205, 205, 205, alpha)), egui::StrokeKind::Inside);
    p.text(bp, Align2::CENTER_CENTER, label, FontId::proportional(13.0), Color32::from_rgba_unmultiplied(45, 45, 45, alpha));
}

/// Factor de visibilidad [0,1] de la banda `i` (0 = interna) segun el progreso
/// global `gt` (1 = abierto, 0 = cerrado). Al cerrar, la banda interna se va
/// PRIMERO; al abrir, la externa aparece primero. Permite el cierre escalonado.
fn band_stagger(gt: f32, i: usize) -> f32 {
    let start = 0.5 - i as f32 * 0.22;
    ((gt - start) / 0.5).clamp(0.0, 1.0)
}

/// Valor (0..1) a lo largo del arco segun el ANGULO del punto (independiente del
/// radio). Sirve para arrastrar una banda fija sin saltar a otra.
fn arc_value(c: Pos2, pp: Pos2) -> f32 {
    let v = pp - c;
    let ang = v.x.atan2(-v.y).to_degrees().rem_euclid(360.0);
    let au = if ang >= SP_START { ang } else { ang + 360.0 };
    ((au - SP_START) / SP_SWEEP).clamp(0.0, 1.0)
}

/// Devuelve (indice de arco, valor 0..1) si el punto cae sobre alguno de los arcos.
fn arc_hit(c: Pos2, pp: Pos2, radii: &[f32]) -> Option<(usize, f32)> {
    let v = pp - c;
    let dist = v.length();
    let ang = v.x.atan2(-v.y).to_degrees().rem_euclid(360.0);
    let au = if ang >= SP_START {
        ang
    } else if ang <= SP_START + SP_SWEEP - 360.0 {
        ang + 360.0
    } else {
        return None;
    };
    let t = (au - SP_START) / SP_SWEEP;
    if !(-0.05..=1.05).contains(&t) {
        return None;
    }
    let mut best = 0;
    let mut bd = f32::MAX;
    for (i, &r) in radii.iter().enumerate() {
        let d = (dist - r).abs();
        if d < bd {
            bd = d;
            best = i;
        }
    }
    if bd > 16.0 {
        return None;
    }
    Some((best, t.clamp(0.0, 1.0)))
}

/// Celda-tarjeta del panel: vista previa arriba + nombre abajo, con fondo y borde
/// sutiles y realce al pasar el raton (estilo profesional). Devuelve true si se clicó.
fn item_cell(ui: &mut egui::Ui, name: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(114.0, 86.0), egui::Sense::click());
    let p = ui.painter_at(rect);
    let card = rect.shrink(3.0);
    let radius = egui::CornerRadius::same(10);
    let bg = if resp.is_pointer_button_down_on() {
        Color32::from_gray(226)
    } else if resp.hovered() {
        Color32::from_gray(237)
    } else {
        Color32::from_gray(249)
    };
    p.rect_filled(card, radius, bg);
    let border = if resp.hovered() {
        Stroke::new(1.4, Color32::from_rgb(150, 185, 235))
    } else {
        Stroke::new(1.0, Color32::from_gray(227))
    };
    p.rect_stroke(card, radius, border, egui::StrokeKind::Inside);
    draw_preview(&p, rect.center() - egui::vec2(0.0, 11.0), name);
    p.text(
        rect.center_bottom() - egui::vec2(0.0, 13.0),
        Align2::CENTER_CENTER,
        name,
        FontId::proportional(12.5),
        Color32::from_gray(55),
    );
    resp.clicked()
}

// --- Espiral de color COPIC (datos reales en `copic::COPIC`) ---
const SP_R0: f32 = R_OUT + 100.0; // radio interior del primer anillo (alejado para no tapar los chips)
const SP_DR: f32 = 28.0; // grosor de cada swatch radial
const SP_START: f32 = 300.0; // grados (horario desde arriba); hueco a la izquierda
const SP_SWEEP: f32 = 300.0;

fn rgb_to_color(rgb: u32) -> Color32 {
    Color32::from_rgb(((rgb >> 16) & 0xff) as u8, ((rgb >> 8) & 0xff) as u8, (rgb & 0xff) as u8)
}

fn luminance(rgb: u32) -> f32 {
    let r = ((rgb >> 16) & 0xff) as f32;
    let g = ((rgb >> 8) & 0xff) as f32;
    let b = (rgb & 0xff) as f32;
    0.299 * r + 0.587 * g + 0.114 * b
}

/// Separa un codigo COPIC en (familia alfabetica, parte numerica). Ej: "YR04" -> ("YR","04").
fn copic_split(code: &str) -> (&str, &str) {
    let i = code.find(|ch: char| ch.is_ascii_digit()).unwrap_or(code.len());
    (&code[..i], &code[i..])
}

fn copic_is_gray(fam: &str, code: &str) -> bool {
    matches!(fam, "C" | "N" | "T" | "W") || matches!(code, "0" | "100" | "110")
}

/// Orden por tono alrededor de la espiral (rojo -> ... -> marron), como Concepts.
fn copic_hue_rank(fam: &str) -> i32 {
    match fam {
        "R" => 0,
        "FRV" | "RV" => 1,
        "FV" | "V" => 2,
        "BV" => 3,
        "B" => 4,
        "BG" => 5,
        "G" => 6,
        "TG" | "YG" => 7,
        "FY" | "Y" => 8,
        "FYR" | "YR" => 9,
        "TR" | "E" => 10,
        _ => 50,
    }
}

/// Agrupa los colores COPIC en columnas (familia + primer digito), ordenadas por
/// tono; los grises van primero. Dentro de cada columna, de oscuro (interior) a claro.
fn copic_columns() -> Vec<Vec<(&'static str, u32)>> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(i32, &'static str, char), Vec<(&'static str, u32)>> = BTreeMap::new();
    for &(code, rgb) in crate::copic::COPIC {
        let (fam, num) = copic_split(code);
        let rank = if copic_is_gray(fam, code) { -10 } else { copic_hue_rank(fam) };
        let digit = num.chars().next().unwrap_or('0');
        groups.entry((rank, fam, digit)).or_default().push((code, rgb));
    }
    let mut cols = Vec::new();
    for (_k, mut v) in groups {
        v.sort_by(|a, b| luminance(b.1).total_cmp(&luminance(a.1))); // oscuro -> claro
        cols.push(v);
    }
    cols
}

// =====================================================================
//  PANEL DE AJUSTES (estilo Concepts): "Area de trabajo" + "Interaccion"
// =====================================================================

/// Interruptor tipo iOS. Devuelve true si cambio.
fn sw_switch(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(46.0, 26.0), egui::Sense::click());
    let changed = resp.clicked();
    if changed {
        *on = !*on;
    }
    let p = ui.painter();
    let col = if *on { Color32::from_rgb(122, 162, 150) } else { Color32::from_gray(206) };
    p.rect_filled(rect, egui::CornerRadius::same(13), col);
    let cx = if *on { rect.right() - 13.0 } else { rect.left() + 13.0 };
    p.circle_filled(egui::pos2(cx, rect.center().y), 10.0, Color32::WHITE);
    changed
}

/// Fila: etiqueta a la izquierda, interruptor a la derecha.
fn toggle_row(ui: &mut egui::Ui, label: &str, on: &mut bool) {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(label).size(15.0).color(Color32::from_gray(55)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            sw_switch(ui, on);
        });
    });
    ui.add_space(10.0);
}

/// Fila de slider con valor a la derecha.
fn slider_row(ui: &mut egui::Ui, label: &str, val: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(15.0).strong().color(Color32::from_gray(35)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("{:.1} {}", *val, suffix)).color(Color32::from_gray(80)));
        });
    });
    ui.add(egui::Slider::new(val, range).show_value(false));
    ui.add_space(10.0);
}

/// Boton de cerrar profesional: circulo sutil con una "X" dibujada (resalta al pasar).
fn close_button(ui: &mut egui::Ui) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::click());
    let c = rect.center();
    let bg = if resp.is_pointer_button_down_on() {
        Color32::from_gray(206)
    } else if resp.hovered() {
        Color32::from_gray(228)
    } else {
        Color32::from_gray(242)
    };
    let p = ui.painter();
    p.circle_filled(c, 13.0, bg);
    let s = 5.5;
    let col = if resp.hovered() { Color32::from_gray(30) } else { Color32::from_gray(100) };
    let st = Stroke::new(2.0, col);
    p.line_segment([c + egui::vec2(-s, -s), c + egui::vec2(s, s)], st);
    p.line_segment([c + egui::vec2(-s, s), c + egui::vec2(s, -s)], st);
    resp.clicked()
}

/// Chevron "v" dibujado (para encabezados de seccion).
fn chevron_down(p: &egui::Painter, c: Pos2, col: Color32) {
    let st = Stroke::new(2.0, col);
    p.line_segment([c + egui::vec2(-5.0, -2.5), c + egui::vec2(0.0, 2.5)], st);
    p.line_segment([c + egui::vec2(5.0, -2.5), c + egui::vec2(0.0, 2.5)], st);
}

/// Boton compacto con un icono dibujado a medida. `active` lo resalta (toggle).
fn icon_btn(ui: &mut egui::Ui, active: bool, w: f32, draw: impl FnOnce(&egui::Painter, Pos2)) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 28.0), egui::Sense::click());
    let bg = if active {
        Color32::from_rgb(206, 224, 248)
    } else if resp.is_pointer_button_down_on() {
        Color32::from_gray(205)
    } else if resp.hovered() {
        Color32::from_gray(224)
    } else {
        Color32::from_gray(238)
    };
    let p = ui.painter();
    let r = rect.shrink(1.0);
    p.rect_filled(r, egui::CornerRadius::same(7), bg);
    if active {
        p.rect_stroke(r, egui::CornerRadius::same(7), Stroke::new(1.2, Color32::from_rgb(70, 140, 230)), egui::StrokeKind::Inside);
    }
    draw(&p, rect.center());
    resp.clicked()
}

/// Icono de menu/hamburguesa (tres lineas).
fn icon_hamburger(p: &egui::Painter, c: Pos2) {
    let st = Stroke::new(2.0, Color32::from_gray(70));
    for dy in [-5.0, 0.0, 5.0] {
        p.line_segment([c + egui::vec2(-7.0, dy), c + egui::vec2(7.0, dy)], st);
    }
}

/// Icono de cuadricula (recuadro 3x3).
fn icon_grid_mini(p: &egui::Painter, c: Pos2, col: Color32) {
    let st = Stroke::new(1.3, col);
    let r = 7.0;
    p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(r * 2.0, r * 2.0)), egui::CornerRadius::same(1), st, egui::StrokeKind::Inside);
    let o = r / 3.0;
    p.line_segment([c + egui::vec2(-o, -r), c + egui::vec2(-o, r)], st);
    p.line_segment([c + egui::vec2(o, -r), c + egui::vec2(o, r)], st);
    p.line_segment([c + egui::vec2(-r, -o), c + egui::vec2(r, -o)], st);
    p.line_segment([c + egui::vec2(-r, o), c + egui::vec2(r, o)], st);
}

/// Icono de capas (dos rombos apilados).
fn icon_layers(p: &egui::Painter, c: Pos2) {
    let col = Color32::from_gray(70);
    let st = Stroke::new(1.4, col);
    let para = |cy: f32| {
        vec![
            c + egui::vec2(0.0, cy - 4.0),
            c + egui::vec2(8.0, cy),
            c + egui::vec2(0.0, cy + 4.0),
            c + egui::vec2(-8.0, cy),
        ]
    };
    p.add(Shape::closed_line(para(4.0), st));
    p.add(Shape::convex_polygon(para(-3.0), Color32::from_gray(232), st));
}

/// Icono de precision (mira/objetivo).
fn icon_precision(p: &egui::Painter, c: Pos2) {
    let st = Stroke::new(1.4, Color32::from_gray(70));
    p.circle_stroke(c, 5.5, st);
    p.line_segment([c + egui::vec2(-8.0, 0.0), c + egui::vec2(8.0, 0.0)], st);
    p.line_segment([c + egui::vec2(0.0, -8.0), c + egui::vec2(0.0, 8.0)], st);
}

/// Icono de engranaje (ajustes).
fn icon_gear(p: &egui::Painter, c: Pos2) {
    let col = Color32::from_gray(70);
    p.circle_stroke(c, 3.8, Stroke::new(1.5, col));
    for i in 0..8 {
        let a = (i as f32 / 8.0) * std::f32::consts::TAU;
        let d = egui::vec2(a.cos(), a.sin());
        p.line_segment([c + d * 5.5, c + d * 7.8], Stroke::new(1.8, col));
    }
}

/// Fila de opcion: icono dibujado + etiqueta clicable (estilo selectable).
fn icon_text_row(ui: &mut egui::Ui, active: bool, label: &str, draw: impl FnOnce(&egui::Painter, Pos2)) -> bool {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (ir, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
        draw(ui.painter(), ir.center());
        ui.selectable_label(active, label).clicked()
    })
    .inner
}

/// Flecha vertical (arriba/abajo) para reordenar capas.
fn icon_reorder(p: &egui::Painter, c: Pos2, up: bool) {
    let st = Stroke::new(2.0, Color32::from_gray(70));
    let s = 5.0;
    let tip = if up { c + egui::vec2(0.0, -s) } else { c + egui::vec2(0.0, s) };
    let tail = if up { c + egui::vec2(0.0, s) } else { c + egui::vec2(0.0, -s) };
    let wy = if up { 4.0 } else { -4.0 };
    p.line_segment([tail, tip], st);
    p.line_segment([tip, tip + egui::vec2(-4.0, wy)], st);
    p.line_segment([tip, tip + egui::vec2(4.0, wy)], st);
}

/// Icono de papelera (eliminar capa).
fn icon_trash(p: &egui::Painter, c: Pos2) {
    let col = Color32::from_gray(70);
    let st = Stroke::new(1.6, col);
    let body = egui::Rect::from_center_size(c + egui::vec2(0.0, 2.5), egui::vec2(10.0, 11.0));
    p.rect_stroke(body, egui::CornerRadius::same(1), st, egui::StrokeKind::Inside);
    p.line_segment([c + egui::vec2(-7.0, -4.5), c + egui::vec2(7.0, -4.5)], st);
    p.line_segment([c + egui::vec2(-3.0, -4.5), c + egui::vec2(-3.0, -7.5)], st);
    p.line_segment([c + egui::vec2(-3.0, -7.5), c + egui::vec2(3.0, -7.5)], st);
    p.line_segment([c + egui::vec2(3.0, -7.5), c + egui::vec2(3.0, -4.5)], st);
}

/// Encabezado de seccion: barra de acento + titulo + chevron. Da jerarquia visual.
fn section_head(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(egui::vec2(4.0, 22.0), egui::Sense::hover());
        ui.painter().rect_filled(bar, egui::CornerRadius::same(2), Color32::from_rgb(70, 140, 230));
        ui.add_space(4.0);
        ui.label(egui::RichText::new(title).size(21.0).strong().color(Color32::from_gray(22)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            chevron_down(ui.painter(), r.center(), Color32::from_gray(150));
        });
    });
    ui.add_space(8.0);
}

/// Pestana con subrayado de acento cuando esta activa (estilo profesional).
fn tab_button(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let col = if active { Color32::from_gray(18) } else { Color32::from_gray(125) };
    let resp = ui.add(egui::Label::new(egui::RichText::new(label).size(17.0).strong().color(col)).sense(egui::Sense::click()));
    if active {
        let r = resp.rect;
        ui.painter().line_segment(
            [egui::pos2(r.left(), r.bottom() + 4.0), egui::pos2(r.right(), r.bottom() + 4.0)],
            Stroke::new(2.5, Color32::from_rgb(70, 140, 230)),
        );
    }
    resp.clicked()
}

/// Sub-titulo en negrita + descripcion gris. Opcionalmente un enlace a la derecha.
fn sub_head(ui: &mut egui::Ui, title: &str, desc: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title).size(15.0).strong().color(Color32::from_gray(25)));
    if !desc.is_empty() {
        ui.label(egui::RichText::new(desc).size(13.0).color(Color32::from_gray(120)));
    }
    ui.add_space(8.0);
}

/// Opcion circular con icono y etiqueta debajo. Devuelve true si se clico.
fn opt_circle(
    ui: &mut egui::Ui,
    selected: bool,
    locked: bool,
    label: &str,
    draw: impl FnOnce(&egui::Painter, Pos2, f32),
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(96.0, 96.0), egui::Sense::click());
    let p = ui.painter_at(rect);
    let c = egui::pos2(rect.center().x, rect.top() + 32.0);
    let r = 30.0;
    let fill = if selected { Color32::from_rgb(228, 238, 252) } else { Color32::from_gray(238) };
    p.circle_filled(c, r, fill);
    if selected {
        p.circle_stroke(c, r, Stroke::new(2.5, Color32::from_rgb(40, 110, 210)));
    } else if resp.hovered() {
        p.circle_stroke(c, r, Stroke::new(1.5, Color32::from_gray(175)));
    }
    draw(&p, c, r);
    if locked {
        let lr = egui::Rect::from_center_size(c + egui::vec2(0.0, 2.0), egui::vec2(13.0, 10.0));
        p.rect_filled(lr, egui::CornerRadius::same(2), Color32::from_gray(70));
        p.circle_stroke(c + egui::vec2(0.0, -5.0), 5.0, Stroke::new(2.0, Color32::from_gray(70)));
    }
    let lc = if selected { Color32::from_gray(15) } else { Color32::from_gray(95) };
    let weight = if selected { 1.0 } else { 0.0 };
    let _ = weight;
    p.text(
        egui::pos2(rect.center().x, rect.bottom() - 2.0),
        Align2::CENTER_BOTTOM,
        label,
        FontId::proportional(11.5),
        lc,
    );
    resp.clicked() && !locked
}

/// Mini vista previa del tipo de cuadricula dentro de un circulo.
fn draw_grid_preview(p: &egui::Painter, c: Pos2, r: f32, kind: ink_core::GridKind) {
    use ink_core::GridKind::*;
    let col = Color32::from_gray(150);
    let s = Stroke::new(1.0, col);
    let rr = r * 0.7;
    let clip = |dx: f32, dy: f32| c + egui::vec2(dx, dy);
    match kind {
        None => {}
        Dots => {
            for gx in -1..=1 {
                for gy in -1..=1 {
                    p.circle_filled(clip(gx as f32 * 12.0, gy as f32 * 12.0), 1.6, col);
                }
            }
        }
        Squares => {
            for k in -1..=1 {
                let o = k as f32 * 11.0;
                p.line_segment([clip(-rr, o), clip(rr, o)], s);
                p.line_segment([clip(o, -rr), clip(o, rr)], s);
            }
        }
        Lines => {
            for k in -1..=1 {
                let o = k as f32 * 11.0;
                p.line_segment([clip(-rr, o), clip(rr, o)], s);
            }
        }
        Iso => {
            for k in -1..=1 {
                let o = k as f32 * 13.0;
                p.line_segment([clip(-rr, o + 8.0), clip(rr, o - 8.0)], s);
                p.line_segment([clip(-rr, o - 8.0), clip(rr, o + 8.0)], s);
            }
        }
        Triangle => {
            p.line_segment([clip(-rr, rr * 0.6), clip(rr, rr * 0.6)], s);
            p.line_segment([clip(-rr, rr * 0.6), clip(0.0, -rr * 0.7)], s);
            p.line_segment([clip(rr, rr * 0.6), clip(0.0, -rr * 0.7)], s);
        }
        P1 | P2 | P3 => {
            for i in 0..8 {
                let a = (i as f32 / 8.0) * std::f32::consts::TAU;
                p.line_segment([c, c + egui::vec2(a.cos(), a.sin()) * rr], s);
            }
            p.circle_filled(c, 2.0, Color32::from_gray(110));
        }
    }
}

/// Mini vista previa del papel/fondo dentro de un circulo.
fn draw_bg_preview(p: &egui::Painter, c: Pos2, r: f32, kind: crate::settings::BgKind, custom: [f32; 4]) {
    use crate::settings::BgKind::*;
    let fill = match kind {
        Custom => c32(custom),
        MatteWhite => Color32::WHITE,
        Transparent => Color32::from_gray(245),
        Crumpled => Color32::from_rgb(242, 240, 232),
        Light => Color32::from_gray(247),
        Heavy => Color32::from_gray(235),
        Wavy => Color32::from_rgb(240, 243, 246),
        FlatBlue => Color32::from_rgb(41, 115, 179),
        BrownPaper => Color32::from_rgb(148, 107, 66),
        FlatDark => Color32::from_rgb(26, 28, 33),
    };
    if matches!(kind, Transparent) {
        // Tablero de ajedrez para indicar transparencia.
        let n = 5;
        let cell = (r * 1.4) / n as f32;
        let o = c - egui::vec2(r * 0.7, r * 0.7);
        for i in 0..n {
            for j in 0..n {
                let dark = (i + j) % 2 == 0;
                let q = egui::Rect::from_min_size(o + egui::vec2(i as f32 * cell, j as f32 * cell), egui::vec2(cell, cell));
                p.rect_filled(q, egui::CornerRadius::ZERO, if dark { Color32::from_gray(205) } else { Color32::WHITE });
            }
        }
    } else {
        p.circle_filled(c, r - 2.0, fill);
    }
}

/// Iconito generico para acciones (dedos / toques). El texto debajo ya las nombra.
fn draw_action_icon(p: &egui::Painter, c: Pos2, _r: f32, key: &str) {
    let col = Color32::from_gray(70);
    let s = Stroke::new(2.0, col);
    match key {
        "nothing" => icon_x(p, c, 9.0, col),
        "undo" => icon_curved_arrow(p, c, 12.0, col, false),
        "redo" => icon_curved_arrow(p, c, 12.0, col, true),
        "tool" => {
            // mano/lapiz
            p.line_segment([c + egui::vec2(-7.0, 7.0), c + egui::vec2(6.0, -6.0)], Stroke::new(3.0, col));
        }
        "pan" => {
            for a in [0.0_f32, 90.0, 180.0, 270.0] {
                let d = dir(a);
                p.line_segment([c, c + d * 11.0], s);
                p.add(Shape::convex_polygon(
                    vec![c + d * 13.0, c + d * 7.0 + dir(a + 90.0) * 4.0, c + d * 7.0 - dir(a + 90.0) * 4.0],
                    col,
                    Stroke::NONE,
                ));
            }
        }
        "select" => icon_select_cursor(p, c, 12.0, col),
        "push" => preview_wave(p, c, 12.0, 2.0, col),
        "segment" => {
            p.add(Shape::convex_polygon(
                vec![c + egui::vec2(-11.0, 8.0), c + egui::vec2(2.0, 8.0), c + egui::vec2(11.0, -7.0), c + egui::vec2(-11.0, -7.0)],
                Color32::from_gray(90),
                Stroke::NONE,
            ));
        }
        "zoom" => {
            p.circle_stroke(c + egui::vec2(-2.0, -2.0), 8.0, s);
            p.line_segment([c + egui::vec2(4.0, 4.0), c + egui::vec2(10.0, 10.0)], Stroke::new(2.5, col));
        }
        "rotate" => icon_curved_arrow(p, c, 12.0, col, true),
        "lasso" => {
            p.circle_stroke(c, 10.0, Stroke::new(1.5, col));
            p.line_segment([c + egui::vec2(-3.0, 10.0), c + egui::vec2(2.0, 14.0)], s);
        }
        "element" => icon_x_plus(p, c, 10.0, col),
        "color" => icon_dropper(p, c, 11.0, col),
        "layers" => {
            for k in 0..3 {
                let y = -6.0 + k as f32 * 6.0;
                p.line_segment([c + egui::vec2(-10.0, y), c + egui::vec2(10.0, y)], Stroke::new(2.0, col));
            }
        }
        "colors" => {
            for (i, cc) in [Color32::from_rgb(220, 70, 70), Color32::from_rgb(70, 130, 220), Color32::from_rgb(70, 180, 90)].iter().enumerate() {
                p.circle_filled(c + dir(i as f32 * 120.0) * 6.0, 4.0, *cc);
            }
        }
        "config" => {
            p.circle_stroke(c, 7.0, s);
            for i in 0..6 {
                let d = dir(i as f32 * 60.0);
                p.line_segment([c + d * 7.0, c + d * 11.0], s);
            }
        }
        "objects" => {
            p.circle_stroke(c + egui::vec2(-4.0, 3.0), 5.0, s);
            p.add(Shape::convex_polygon(
                vec![c + egui::vec2(4.0, -8.0), c + egui::vec2(10.0, 2.0), c + egui::vec2(-2.0, 2.0)],
                Color32::TRANSPARENT,
                s,
            ));
        }
        "rot_canvas" => icon_curved_arrow(p, c, 12.0, col, true),
        "zoom_canvas" => {
            p.line_segment([c + egui::vec2(-9.0, -9.0), c + egui::vec2(9.0, 9.0)], s);
            p.line_segment([c + egui::vec2(-9.0, -9.0), c + egui::vec2(-3.0, -9.0)], s);
            p.line_segment([c + egui::vec2(9.0, 9.0), c + egui::vec2(3.0, 9.0)], s);
        }
        "all" => {
            p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(18.0, 18.0)), egui::CornerRadius::same(2), Stroke::new(1.0, col), egui::StrokeKind::Inside);
            icon_select_cursor(p, c, 9.0, col);
        }
        "interface" => {
            p.circle_stroke(c, 9.0, s);
            p.add(Shape::convex_polygon(vec![c, c + egui::vec2(9.0, -3.0), c + egui::vec2(9.0, 3.0)], col, Stroke::NONE));
        }
        "last" => {
            p.circle_stroke(c, 9.0, Stroke::new(1.5, col));
            p.text(c, Align2::CENTER_CENTER, "↺", FontId::proportional(14.0), col);
        }
        _ => {
            p.circle_filled(c, 4.0, Color32::from_gray(150));
        }
    }
}

/// Pequeno "+" dentro de un puntero (selector de elemento).
fn icon_x_plus(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    p.line_segment([c + egui::vec2(-s, 0.0), c + egui::vec2(s, 0.0)], Stroke::new(2.0, col));
    p.line_segment([c + egui::vec2(0.0, -s), c + egui::vec2(0.0, s)], Stroke::new(2.0, col));
}

/// Icono de "colapsar la rueda": una gota (circulo + punta), sugiere el punto final.
fn icon_collapse(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    p.circle_filled(c + egui::vec2(0.0, s * 0.28), s * 0.72, col);
    p.add(Shape::convex_polygon(
        vec![
            c + egui::vec2(0.0, -s * 1.05),
            c + egui::vec2(s * 0.62, s * 0.12),
            c + egui::vec2(-s * 0.62, s * 0.12),
        ],
        col,
        Stroke::NONE,
    ));
}

/// Ondas tipo "gota" durante la transicion de colapsar/expandir la rueda.
/// `t`: 0 = rueda abierta, 1 = solo el punto de color. Usa el color actual.
fn draw_collapse_ripple(ctx: &egui::Context, center: Pos2, t: f32, color: Color32) {
    if t <= 0.02 || t >= 0.985 {
        return;
    }
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("wheel_ripple")));
    // Anillos concentricos que crecen hacia afuera y se desvanecen.
    for k in 0..3 {
        let phase = (t + k as f32 * 0.22).min(1.0);
        let radius = R_HOLE + phase * (R_OUT * 1.25);
        let a = ((1.0 - phase).clamp(0.0, 1.0) * 120.0) as u8;
        p.circle_stroke(center, radius, Stroke::new(2.5, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)));
    }
    // La gota central que se va formando.
    let dot_r = R_HOLE * (0.55 + 0.45 * t);
    p.circle_filled(center, dot_r, color);
}

/// Multiplica el alfa de un color por `f` (para revelar/atenuar piezas).
fn fade(c: Color32, f: f32) -> Color32 {
    let f = f.clamp(0.0, 1.0);
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * f).round() as u8)
}

/// Factor de aparicion [0,1] de una pieza al RECONSTRUIR la rueda. `build` 0..1 es
/// el progreso global; `r_norm` 0(centro)..1(borde): las piezas exteriores
/// aparecen primero; `phase` 0..1 desfasa un poco cada pieza (no todas a la vez).
fn reveal(build: f32, r_norm: f32, phase: f32) -> f32 {
    let spread = 0.60; // cuanto se escalonan en total
    let dur = 0.45; // duracion de aparicion de cada pieza
    let start = (1.0 - r_norm).clamp(0.0, 1.2) * spread * 0.7 + phase * spread * 0.30;
    ((build - start) / dur).clamp(0.0, 1.0)
}

/// El panel de ajustes modal. No hace nada si esta cerrado.
fn settings_panel(
    ctx: &egui::Context,
    state: &mut UiState,
    cfg: &mut crate::settings::Settings,
    brush: &mut Brush,
) {
    if !state.show_settings {
        return;
    }
    let screen = ctx.content_rect();

    // Fondo atenuado que cierra al clicar fuera.
    egui::Area::new(egui::Id::new("settings_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, Color32::from_black_alpha(60));
            if resp.clicked() {
                state.show_settings = false;
            }
        });

    let w = (screen.width() * 0.94).min(1180.0);
    let h = (screen.height() * 0.9).min(980.0);

    egui::Window::new("ajustes")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .fixed_size(egui::vec2(w, h))
        .frame(egui::Frame::window(&ctx.global_style()).fill(Color32::from_gray(250)).inner_margin(egui::Margin::same(22)))
        .show(ctx, |ui| {
            // --- Barra de titulo: cerrar + pestanas con subrayado ---
            ui.horizontal(|ui| {
                if close_button(ui) {
                    state.show_settings = false;
                }
                ui.add_space(14.0);
                if tab_button(ui, "Área de trabajo", state.settings_tab == SettingsTab::Workspace) {
                    state.settings_tab = SettingsTab::Workspace;
                    state.grid_editor = false;
                }
                ui.add_space(18.0);
                if tab_button(ui, "Interacción", state.settings_tab == SettingsTab::Interaction) {
                    state.settings_tab = SettingsTab::Interaction;
                }
            });
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            // Barra de scroll fija y delgada: reserva su propio canal (no flotante)
            // para que NO se ensanche al arrastrarla ni tape el texto/enlaces.
            ui.style_mut().spacing.scroll = egui::style::ScrollStyle {
                floating: false,
                bar_width: 8.0,
                bar_inner_margin: 4.0,
                bar_outer_margin: 2.0,
                ..egui::style::ScrollStyle::solid()
            };

            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                match state.settings_tab {
                    SettingsTab::Workspace => {
                        if state.grid_editor {
                            grid_editor_panel(ui, cfg, state);
                        } else {
                            workspace_tab(ui, cfg, state, brush);
                        }
                    }
                    SettingsTab::Interaction => interaction_tab(ui, cfg),
                }
            });
        });
}

fn workspace_tab(ui: &mut egui::Ui, cfg: &mut crate::settings::Settings, state: &mut UiState, brush: &mut Brush) {
    use crate::settings::{Artboard, BgKind};
    use ink_core::GridKind;

    // ---------------- Lienzo ----------------
    section_head(ui, "Lienzo");
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Fondo").size(15.0).strong().color(Color32::from_gray(25)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.link(egui::RichText::new("Editar color").color(Color32::from_rgb(40, 110, 210))).clicked() {
                cfg.bg = BgKind::Custom;
            }
        });
    });
    ui.label(egui::RichText::new("¿Papel estándar o color de fondo personalizado?").size(13.0).color(Color32::from_gray(120)));
    ui.add_space(8.0);
    let bgs = [
        (BgKind::Custom, "Color personalizado"),
        (BgKind::MatteWhite, "Blanco mate"),
        (BgKind::Transparent, "Transparente"),
        (BgKind::Crumpled, "Arrugado"),
        (BgKind::Light, "Ligero"),
        (BgKind::Heavy, "Pesado"),
        (BgKind::Wavy, "Ondulado"),
        (BgKind::FlatBlue, "Plano azul"),
        (BgKind::BrownPaper, "Papel marrón"),
        (BgKind::FlatDark, "Plano oscuro"),
    ];
    let custom = cfg.bg_custom;
    ui.horizontal_wrapped(|ui| {
        for (k, name) in bgs {
            if opt_circle(ui, cfg.bg == k, false, name, |p, c, r| draw_bg_preview(p, c, r, k, custom)) {
                cfg.bg = k;
            }
        }
    });
    if cfg.bg == BgKind::Custom {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Color de fondo:").size(13.0).color(Color32::from_gray(90)));
            let mut rgba = egui::Rgba::from_rgba_premultiplied(custom[0], custom[1], custom[2], custom[3]);
            if egui::color_picker::color_edit_button_rgba(ui, &mut rgba, egui::color_picker::Alpha::Opaque).changed() {
                cfg.bg_custom = [rgba.r(), rgba.g(), rgba.b(), 1.0];
            }
        });
    }
    ui.add_space(16.0);

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Tipo de cuadrícula").size(15.0).strong().color(Color32::from_gray(25)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.link(egui::RichText::new("Editar cuadrícula").color(Color32::from_rgb(40, 110, 210))).clicked() {
                state.grid_editor = true;
            }
        });
    });
    ui.label(egui::RichText::new("Puedes alternar rápidamente la cuadrícula en los menús Precisión o Capas.").size(13.0).color(Color32::from_gray(120)));
    ui.add_space(8.0);
    let grids = [
        (GridKind::None, "Sin cuadrícula", false),
        (GridKind::Dots, "Cuadrícula de puntos", false),
        (GridKind::Squares, "Papel milimetrado", false),
        (GridKind::Lines, "Papel rayado", false),
        (GridKind::Iso, "Cuadrícula isométrica", false),
        (GridKind::Triangle, "Triángulo", false),
        (GridKind::P1, "1 punto", false),
        (GridKind::P2, "2 puntos", false),
        (GridKind::P3, "3 puntos", false),
    ];
    ui.horizontal_wrapped(|ui| {
        for (k, name, locked) in grids {
            if opt_circle(ui, cfg.grid == k, locked, name, |p, c, r| draw_grid_preview(p, c, r, k)) {
                cfg.grid = k;
                if k != GridKind::None {
                    state.last_grid = k;
                }
            }
        }
    });
    ui.add_space(18.0);

    // ---------------- Mesa de trabajo ----------------
    section_head(ui, "Mesa de trabajo");
    sub_head(ui, "Tamaño de la mesa de trabajo", "Establecer un marco de referencia para facilitar las exportaciones.");
    let (aw, ah) = cfg.artboard_size().map(|(w, h)| (format!("{:.0}", w), format!("{:.0}", h))).unwrap_or(("∞".into(), "∞".into()));
    ui.horizontal(|ui| {
        ui.label("Anch :");
        let _ = ui.add(egui::Button::new(egui::RichText::new(aw).size(14.0)).min_size(egui::vec2(120.0, 30.0)));
        ui.add_space(10.0);
        ui.label("Alt :");
        let _ = ui.add(egui::Button::new(egui::RichText::new(ah).size(14.0)).min_size(egui::vec2(120.0, 30.0)));
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        for (a, name) in [(Artboard::Infinite, "Infinito"), (Artboard::R1024x768, "1024x768"), (Artboard::A4, "A4"), (Artboard::R1080p, "1080p")] {
            if ui.selectable_label(cfg.artboard == a, name).clicked() {
                cfg.artboard = a;
            }
        }
    });
    ui.add_space(18.0);

    // ---------------- Medidas ----------------
    section_head(ui, "Medidas");
    sub_head(ui, "Escala de dibujo", "Definir cómo se comparan los objetos de la pantalla con la vida real.");
    ui.horizontal(|ui| {
        let _ = ui.add(egui::Button::new(egui::RichText::new(format!("{:.0} {}", cfg.scale_from, cfg.unit_abbrev())).size(14.0)).min_size(egui::vec2(96.0, 30.0)));
        ui.label(":");
        let _ = ui.add(egui::Button::new(egui::RichText::new(format!("{:.0} {}", cfg.scale_to, cfg.unit_abbrev())).size(14.0)).min_size(egui::vec2(96.0, 30.0)));
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        for (f, t, name) in [(1.0, 1.0, "1:1"), (1.0, 10.0, "1:10"), (1.0, 64.0, "1:64"), (1.0, 100.0, "1:100")] {
            let sel = (cfg.scale_from - f).abs() < 1e-3 && (cfg.scale_to - t).abs() < 1e-3;
            if ui.selectable_label(sel, name).clicked() {
                cfg.scale_from = f;
                cfg.scale_to = t;
            }
        }
    });
    ui.add_space(14.0);

    sub_head(ui, "Unidades", "Cualquier unidad mostrada o introducida en el lienzo se convertirá a este sistema.");
    use crate::settings::{Unit, UnitTab};
    ui.horizontal(|ui| {
        for (t, name) in [(UnitTab::Digital, "Digital"), (UnitTab::Metric, "Métrico"), (UnitTab::Imperial, "Imperial")] {
            if ui.selectable_label(cfg.unit_tab == t, egui::RichText::new(name).size(16.0).strong()).clicked() {
                cfg.unit_tab = t;
                cfg.unit = match t {
                    UnitTab::Digital => Unit::Pts,
                    UnitTab::Metric => Unit::Mm,
                    UnitTab::Imperial => Unit::In,
                };
            }
        }
    });
    ui.add_space(8.0);
    let units: &[(Unit, &str)] = match cfg.unit_tab {
        UnitTab::Digital => &[(Unit::Px, "px"), (Unit::Pts, "pts")],
        UnitTab::Metric => &[(Unit::Mm, "mm"), (Unit::Cm, "cm"), (Unit::M, "m")],
        UnitTab::Imperial => &[(Unit::In, "in"), (Unit::Ft, "ft")],
    };
    ui.horizontal_wrapped(|ui| {
        for (u, name) in units {
            let u = *u;
            if opt_circle(ui, cfg.unit == u, false, "", |p, c, _r| {
                p.text(c, Align2::CENTER_CENTER, name, FontId::proportional(15.0), Color32::from_gray(40));
            }) {
                cfg.unit = u;
            }
        }
    });
    ui.add_space(14.0);

    sub_head(ui, "Formato de visualización y precisión", "Selecciona tu notación preferida.");
    ui.horizontal_wrapped(|ui| {
        let ex_full = cfg.format_with(true, cfg.tenths, 6.5);
        let ex_abbr = cfg.format_with(false, cfg.tenths, 6.5);
        if opt_circle(ui, cfg.name_full, false, "Completo", |p, c, _r| {
            p.text(c, Align2::CENTER_CENTER, &ex_full, FontId::proportional(12.0), Color32::from_gray(40));
        }) {
            cfg.name_full = true;
        }
        if opt_circle(ui, !cfg.name_full, false, "Abreviado", |p, c, _r| {
            p.text(c, Align2::CENTER_CENTER, &ex_abbr, FontId::proportional(12.0), Color32::from_gray(40));
        }) {
            cfg.name_full = false;
        }
        ui.add_space(10.0);
        if opt_circle(ui, !cfg.tenths, false, "Redondeado", |p, c, _r| {
            p.text(c, Align2::CENTER_CENTER, "6", FontId::proportional(15.0), Color32::from_gray(40));
        }) {
            cfg.tenths = false;
        }
        if opt_circle(ui, cfg.tenths, false, "Décimas", |p, c, _r| {
            p.text(c, Align2::CENTER_CENTER, "6.0", FontId::proportional(15.0), Color32::from_gray(40));
        }) {
            cfg.tenths = true;
        }
    });
    ui.add_space(8.0);
    toggle_row(ui, "Mostrar la longitud del trazo en el lado derecho al dibujar", &mut cfg.show_stroke_length);
    toggle_row(ui, "Mostrar la escala en la barra de estado para las selecciones", &mut cfg.show_scale_statusbar);
    ui.add_space(8.0);

    // ---------------- Ajuste de herramienta ----------------
    section_head(ui, "Ajuste de herramienta");
    sub_head(ui, "Interfaz", "Elige tu paleta de herramientas preferida.");
    use crate::settings::ToolUi;
    ui.horizontal(|ui| {
        if opt_circle(ui, cfg.tool_ui == ToolUi::Wheel, false, "Rueda", |p, c, _r| {
            p.circle_stroke(c, 13.0, Stroke::new(2.5, Color32::from_gray(60)));
            p.circle_filled(c, 4.0, Color32::from_gray(60));
        }) {
            cfg.tool_ui = ToolUi::Wheel;
        }
        if opt_circle(ui, cfg.tool_ui == ToolUi::Bar, false, "Barra", |p, c, _r| {
            p.rect_filled(egui::Rect::from_center_size(c, egui::vec2(7.0, 24.0)), egui::CornerRadius::same(3), Color32::from_gray(60));
        }) {
            cfg.tool_ui = ToolUi::Bar;
        }
    });
    ui.add_space(20.0);

    if ui.link(egui::RichText::new("Restaurar a los ajustes predeterminados").color(Color32::from_rgb(40, 110, 210))).clicked() {
        *cfg = crate::settings::Settings::default();
        brush.color = cfg.default_ink();
    }
    ui.add_space(10.0);
}

fn interaction_tab(ui: &mut egui::Ui, cfg: &mut crate::settings::Settings) {
    use crate::settings::{FingerAction, HoldAction, TapAction};

    // ---------------- Raton y teclado ----------------
    section_head(ui, "Ratón y teclado");
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Atajos de teclado").size(15.0).strong().color(Color32::from_gray(25)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let _ = ui.link(egui::RichText::new("Editar accesos directos").color(Color32::from_rgb(40, 110, 210)));
        });
    });
    ui.label(egui::RichText::new("Ver y editar los atajos de teclado.").size(13.0).color(Color32::from_gray(120)));
    ui.add_space(8.0);
    toggle_row(ui, "Activar los atajos del teclado", &mut cfg.shortcuts_enabled);
    ui.add_space(10.0);

    // ---------------- Entrada tactil ----------------
    section_head(ui, "Entrada táctil");
    sub_head(ui, "Acción de dedos", "Mientras tu lápiz dibuja, puedes colocar tu dedo para hacer algo más.");
    let fingers = [
        (FingerAction::Nothing, "No hacer nada", "nothing"),
        (FingerAction::ActiveTool, "Usar herramienta activa", "tool"),
        (FingerAction::Pan, "Desplazar el lienzo", "pan"),
        (FingerAction::Select, "Seleccionar", "select"),
        (FingerAction::Push, "Empujar", "push"),
        (FingerAction::Segment, "Segmento", "segment"),
        (FingerAction::Zoom, "Zoom", "zoom"),
        (FingerAction::Rotate, "Rotar", "rotate"),
    ];
    ui.horizontal_wrapped(|ui| {
        for (a, name, icon) in fingers {
            if opt_circle(ui, cfg.finger_action == a, false, name, |p, c, r| draw_action_icon(p, c, r, icon)) {
                cfg.finger_action = a;
            }
        }
    });
    ui.add_space(14.0);

    sub_head(ui, "Dos dedos", "");
    toggle_row(ui, "Activar ampliación de lienzo", &mut cfg.two_finger_zoom);
    toggle_row(ui, "Activar foto con zoom", &mut cfg.two_finger_photo_zoom);
    toggle_row(ui, "Activar la rotación de lienzo", &mut cfg.two_finger_rotate);
    toggle_row(ui, "Activar foto en rotación", &mut cfg.two_finger_photo_rotate);
    ui.add_space(6.0);

    sub_head(ui, "Pulsar y mantener", "Mantén pulsado sobre los elementos para recogerlos.");
    let holds = [
        (HoldAction::LastUsed, "Último uso", "last"),
        (HoldAction::Nothing, "No hacer nada", "nothing"),
        (HoldAction::Lasso, "Lazo", "lasso"),
        (HoldAction::ElementPicker, "Selector de elemento", "element"),
        (HoldAction::ColorPicker, "Selector de color", "color"),
    ];
    ui.horizontal_wrapped(|ui| {
        for (a, name, icon) in holds {
            if opt_circle(ui, cfg.hold_action == a, false, name, |p, c, r| draw_action_icon(p, c, r, icon)) {
                cfg.hold_action = a;
            }
        }
    });
    ui.add_space(6.0);
    slider_row(ui, "Tiempo de activación", &mut cfg.hold_time, 0.1..=2.0, "segundos");
    toggle_row(ui, "Resaltar selección", &mut cfg.highlight_selection);
    ui.add_space(6.0);

    sub_head(ui, "Dibujar y mantener", "Al final del trazo, quédate quieto.");
    toggle_row(ui, "Activar el reconocimiento de forma", &mut cfg.shape_recognition);
    slider_row(ui, "Tiempo de activación", &mut cfg.draw_hold_time, 0.2..=2.0, "segundos");
    ui.add_space(8.0);

    let taps = [
        (TapAction::Nothing, "Hacer nada", "nothing"),
        (TapAction::Undo, "Deshacer", "undo"),
        (TapAction::Redo, "Rehacer", "redo"),
        (TapAction::SelectLast, "Seleccionar la última", "select"),
        (TapAction::ShowLayers, "Mostrar capas", "layers"),
        (TapAction::ShowColors, "Mostrar colores", "colors"),
        (TapAction::ToolConfig, "Configuración de herramienta", "config"),
        (TapAction::ShowObjects, "Mostrar objetos", "objects"),
        (TapAction::ToggleRotation, "Cambiar rotación del lienzo", "rot_canvas"),
        (TapAction::ToggleZoom, "Cambiar zoom del lienzo", "zoom_canvas"),
        (TapAction::SelectAll, "Seleccionar todo", "all"),
        (TapAction::ToggleInterface, "Cambiar interfaz", "interface"),
    ];
    sub_head(ui, "Toque con dos dedos", "");
    ui.horizontal_wrapped(|ui| {
        for (a, name, icon) in taps {
            if opt_circle(ui, cfg.two_finger_tap == a, false, name, |p, c, r| draw_action_icon(p, c, r, icon)) {
                cfg.two_finger_tap = a;
            }
        }
    });
    ui.add_space(10.0);
    sub_head(ui, "Toque con tres dedos", "");
    ui.horizontal_wrapped(|ui| {
        for (a, name, icon) in taps {
            if opt_circle(ui, cfg.three_finger_tap == a, false, name, |p, c, r| draw_action_icon(p, c, r, icon)) {
                cfg.three_finger_tap = a;
            }
        }
    });
    ui.add_space(12.0);
}

/// Fila de slider para el editor de cuadricula: titulo + valor a la derecha,
/// descripcion opcional y la barra.
fn grid_slider(ui: &mut egui::Ui, title: &str, desc: &str, val: &mut f32, range: std::ops::RangeInclusive<f32>, valtxt: &str) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).size(15.0).strong().color(Color32::from_gray(30)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(valtxt).color(Color32::from_gray(90)));
        });
    });
    if !desc.is_empty() {
        ui.label(egui::RichText::new(desc).size(12.5).color(Color32::from_gray(125)));
    }
    ui.add_space(2.0);
    ui.add(egui::Slider::new(val, range).show_value(false));
}

/// Sub-panel "Editar cuadricula" (se abre desde el enlace en "Tipo de cuadricula").
fn grid_editor_panel(ui: &mut egui::Ui, cfg: &mut crate::settings::Settings, state: &mut UiState) {
    if ui.add(egui::Button::new(egui::RichText::new("‹ Atrás").size(15.0).color(Color32::from_gray(60))).frame(false)).clicked() {
        state.grid_editor = false;
    }
    ui.add_space(6.0);
    ui.label(egui::RichText::new(grid_name(cfg.grid)).size(24.0).strong().color(Color32::from_gray(20)));
    ui.add_space(10.0);

    // --- Valor predefinido ---
    sub_head(ui, "Valor predefinido", "");
    let presets: [(&str, f32, u32); 3] = [("Cuadrícula Cuadrada", 24.0, 1), ("10 / 100", 100.0, 10), ("16 / 64", 64.0, 16)];
    ui.horizontal_wrapped(|ui| {
        for (name, sp, dv) in presets {
            let active = (cfg.grid_size - sp).abs() < 0.5 && cfg.grid_divisions == dv;
            if opt_circle(ui, active, false, name, |p, c, r| draw_grid_cells(p, c, r, dv.max(1))) {
                cfg.grid_size = sp;
                cfg.grid_divisions = dv;
            }
        }
        let any_preset = presets.iter().any(|&(_, sp, dv)| (cfg.grid_size - sp).abs() < 0.5 && cfg.grid_divisions == dv);
        let _ = opt_circle(ui, !any_preset, false, "Personalizado", |p, c, r| draw_grid_cells(p, c, r, 3));
    });
    ui.add_space(14.0);

    // --- Espaciado / Divisiones / Grosor ---
    let sp_txt = format!("{:.0} pts", cfg.grid_size);
    grid_slider(ui, "Espaciado", "Establece el espacio de tu cuadrícula. Las unidades están determinadas por las unidades del documento.", &mut cfg.grid_size, 4.0..=400.0, &sp_txt);
    let mut dv = cfg.grid_divisions as f32;
    let dv_txt = format!("{:.0}", dv);
    grid_slider(ui, "Divisiones", "Establece el número de divisiones entre las líneas principales. Establece el valor en 1 para mostrar solo las líneas principales.", &mut dv, 1.0..=20.0, &dv_txt);
    cfg.grid_divisions = dv.round() as u32;
    let gw_txt = format!("{:.0} pts", cfg.grid_line_width);
    grid_slider(ui, "Grosor de línea", "", &mut cfg.grid_line_width, 0.5..=6.0, &gw_txt);
    ui.add_space(8.0);

    // --- Color ---
    sub_head(ui, "Color", "El color automático se adapta al color de fondo. Los colores personalizados son independientes del color de fondo.");
    let cust = cfg.grid_color_custom;
    ui.horizontal(|ui| {
        if opt_circle(ui, cfg.grid_color_auto, false, "Automático", |p, c, r| {
            p.circle_filled(c, r - 4.0, Color32::from_gray(236));
            p.circle_stroke(c, r - 4.0, Stroke::new(1.0, Color32::from_gray(192)));
        }) {
            cfg.grid_color_auto = true;
        }
        if opt_circle(ui, !cfg.grid_color_auto, false, "Personalizado", |p, c, r| {
            p.circle_filled(c, r - 4.0, c32(cust));
        }) {
            cfg.grid_color_auto = false;
        }
    });
    if !cfg.grid_color_auto {
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(egui::RichText::new("Color de cuadrícula:").size(13.0).color(Color32::from_gray(90)));
            let mut rgba = egui::Rgba::from_rgba_premultiplied(cust[0], cust[1], cust[2], 1.0);
            if egui::color_picker::color_edit_button_rgba(ui, &mut rgba, egui::color_picker::Alpha::Opaque).changed() {
                cfg.grid_color_custom = [rgba.r(), rgba.g(), rgba.b(), 1.0];
            }
        });
    }
    ui.add_space(6.0);

    // --- Opacidad ---
    let op_txt = format!("{:.0}%", cfg.grid_opacity * 100.0);
    grid_slider(ui, "Opacidad", "", &mut cfg.grid_opacity, 0.0..=1.0, &op_txt);
    ui.add_space(10.0);

    // --- Limitar a la mesa de trabajo ---
    ui.label(egui::RichText::new("Limitar a la mesa de trabajo").size(15.0).strong().color(Color32::from_gray(25)));
    ui.add_space(4.0);
    ui.checkbox(&mut cfg.grid_limit_artboard, "Muestra solo las líneas de la cuadrícula dentro de la mesa de trabajo.");
    ui.add_space(12.0);
}

/// Mini-cuadricula para los iconos de "Valor predefinido" (densidad segun divisiones).
fn draw_grid_cells(p: &egui::Painter, c: Pos2, r: f32, divisions: u32) {
    let rr = r * 0.68;
    let major = Stroke::new(1.4, Color32::from_gray(120));
    let minor = Stroke::new(0.7, Color32::from_gray(180));
    // Lineas principales (cuadrante 2x2).
    for k in -1..=1 {
        let o = k as f32 * rr * 0.55;
        p.line_segment([c + egui::vec2(-rr, o), c + egui::vec2(rr, o)], major);
        p.line_segment([c + egui::vec2(o, -rr), c + egui::vec2(o, rr)], major);
    }
    // Subdivisiones segun divisiones.
    if divisions > 1 {
        let sub = (rr * 1.1) / divisions.min(6) as f32;
        let mut x = -rr;
        while x <= rr {
            p.line_segment([c + egui::vec2(x, -rr), c + egui::vec2(x, rr)], minor);
            p.line_segment([c + egui::vec2(-rr, x), c + egui::vec2(rr, x)], minor);
            x += sub;
        }
    }
}

/// Panel de capas (estilo Photoshop): lista de capas (frente arriba) con
/// visibilidad, miniatura, nombre editable, bloqueo y opacidad de la capa
/// activa, mas botones para anadir/duplicar/eliminar/mover.
fn layers_panel(ctx: &egui::Context, state: &mut UiState, doc: &mut Document, actions: &mut UiActions) {
    if !state.show_layers {
        return;
    }
    egui::Area::new(egui::Id::new("layers_panel"))
        .anchor(Align2::RIGHT_TOP, egui::vec2(-16.0, 64.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(286.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Capas").size(17.0).strong().color(Color32::from_gray(22)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if close_button(ui) {
                            state.show_layers = false;
                        }
                    });
                });
                ui.add_space(4.0);
                let act = doc.active;
                ui.horizontal(|ui| {
                    if ui.button("Nueva capa").clicked() {
                        doc.add_layer();
                        actions.layers_dirty = true;
                    }
                    if ui.button("Duplicar").clicked() {
                        doc.duplicate_layer(act);
                        actions.layers_dirty = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if doc.layer_count() > 1 && icon_btn(ui, false, 30.0, icon_trash) {
                            doc.delete_layer(act);
                            actions.layers_dirty = true;
                        }
                        if icon_btn(ui, false, 30.0, |p, c| icon_reorder(p, c, false)) {
                            doc.move_layer(act, false);
                            actions.layers_dirty = true;
                        }
                        if icon_btn(ui, false, 30.0, |p, c| icon_reorder(p, c, true)) {
                            doc.move_layer(act, true);
                            actions.layers_dirty = true;
                        }
                    });
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);

                let n = doc.layer_count();
                let active = doc.active;
                let mut set_active: Option<usize> = None;
                let mut toggle_vis: Option<usize> = None;
                let mut toggle_lock: Option<usize> = None;
                let mut set_opacity: Option<(usize, f32)> = None;

                egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, false]).show(ui, |ui| {
                    for i in (0..n).rev() {
                        let is_active = i == active;
                        let fill = if is_active { Color32::from_rgb(224, 235, 250) } else { Color32::from_gray(247) };
                        egui::Frame::new()
                            .fill(fill)
                            .inner_margin(egui::Margin::same(6))
                            .corner_radius(egui::CornerRadius::same(6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    // Ojo de visibilidad.
                                    let (er, eresp) = ui.allocate_exact_size(egui::vec2(22.0, 26.0), egui::Sense::click());
                                    {
                                        let p = ui.painter();
                                        let ec = er.center();
                                        let vis = doc.layers[i].visible;
                                        let col = if vis { Color32::from_gray(60) } else { Color32::from_gray(180) };
                                        p.circle_stroke(ec, 7.0, Stroke::new(1.4, col));
                                        if vis {
                                            p.circle_filled(ec, 3.0, col);
                                        } else {
                                            p.line_segment([ec + egui::vec2(-8.0, -8.0), ec + egui::vec2(8.0, 8.0)], Stroke::new(1.4, col));
                                        }
                                    }
                                    if eresp.clicked() {
                                        toggle_vis = Some(i);
                                    }
                                    // Miniatura (clic = activar la capa).
                                    let (mr, mresp) = ui.allocate_exact_size(egui::vec2(28.0, 26.0), egui::Sense::click());
                                    ui.painter().rect_filled(mr, egui::CornerRadius::same(4), Color32::WHITE);
                                    ui.painter().rect_stroke(mr, egui::CornerRadius::same(4), Stroke::new(1.0, Color32::from_gray(200)), egui::StrokeKind::Inside);
                                    if mresp.clicked() {
                                        set_active = Some(i);
                                    }
                                    // Nombre editable.
                                    ui.add(egui::TextEdit::singleline(&mut doc.layers[i].name).desired_width(112.0));
                                    // Candado (clic = bloquear/desbloquear).
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let (lr, lresp) = ui.allocate_exact_size(egui::vec2(22.0, 26.0), egui::Sense::click());
                                        let p = ui.painter();
                                        let lc = lr.center();
                                        let locked = doc.layers[i].locked;
                                        let col = if locked { Color32::from_rgb(200, 120, 40) } else { Color32::from_gray(175) };
                                        p.rect_filled(egui::Rect::from_center_size(lc + egui::vec2(0.0, 3.0), egui::vec2(11.0, 8.0)), egui::CornerRadius::same(2), col);
                                        p.circle_stroke(lc + egui::vec2(0.0, -3.5), 4.0, Stroke::new(1.5, col));
                                        if lresp.clicked() {
                                            toggle_lock = Some(i);
                                        }
                                    });
                                });
                                // Opacidad (solo la capa activa).
                                if is_active {
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new("Opacidad").size(11.0).color(Color32::from_gray(90)));
                                        let mut op = doc.layers[i].opacity;
                                        if ui.add(egui::Slider::new(&mut op, 0.0..=1.0).show_value(false)).changed() {
                                            set_opacity = Some((i, op));
                                        }
                                        ui.label(egui::RichText::new(format!("{:.0}%", doc.layers[i].opacity * 100.0)).size(11.0));
                                    });
                                }
                            });
                        ui.add_space(3.0);
                    }
                });

                // Aplicar las acciones diferidas (fuera de los prestamos de la lista).
                if let Some(i) = set_active {
                    doc.set_active(i);
                }
                if let Some(i) = toggle_vis {
                    let v = !doc.layers[i].visible;
                    doc.set_visible(i, v);
                    actions.layers_dirty = true;
                }
                if let Some(i) = toggle_lock {
                    let l = !doc.layers[i].locked;
                    doc.set_locked(i, l);
                }
                if let Some((i, op)) = set_opacity {
                    doc.set_layer_opacity(i, op);
                    actions.layers_dirty = true;
                }
            });
        });
}

/// Construye toda la UI. Devuelve las acciones a aplicar.
pub fn build_panel(
    ctx: &egui::Context,
    state: &mut UiState,
    brush: &mut Brush,
    cfg: &mut crate::settings::Settings,
    doc: &mut Document,
    stats: Stats,
) -> UiActions {
    let mut actions = UiActions::default();

    // Tema claro, como Concepts.
    ctx.set_visuals(egui::Visuals::light());
    // Sin fade automatico al aparecer/ocultar Areas (la rueda al colapsar). Nuestras
    // animaciones propias usan `animate_bool_with_time` con tiempo explicito, asi que
    // no se ven afectadas por esto.
    ctx.global_style_mut(|s| s.animation_time = 0.0);
    // Con el cuentagotas activo, ocultamos el cursor del sistema (dibujamos el icono
    // del gotero siguiendo el raton en `draw_overlays`).
    if state.eyedropper {
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }

    // Barra superior: hamburguesa, cuadricula rapida y ajustes.
    egui::Area::new(egui::Id::new("topbar"))
        .anchor(Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if icon_btn(ui, false, 32.0, icon_hamburger) {
                        state.open = !state.open;
                    }
                    ui.label(egui::RichText::new("Dibujo").strong());
                    ui.separator();
                    let grid_on = cfg.grid != ink_core::GridKind::None;
                    if icon_btn(ui, grid_on, 32.0, |p, c| {
                        icon_grid_mini(p, c, Color32::from_gray(70))
                    }) {
                        // Recordamos el ultimo tipo para alternar sin perderlo.
                        cfg.grid = if grid_on {
                            ink_core::GridKind::None
                        } else if state.last_grid != ink_core::GridKind::None {
                            state.last_grid
                        } else {
                            ink_core::GridKind::Squares
                        };
                    }
                    if icon_btn(ui, state.show_settings, 32.0, icon_gear) {
                        state.show_settings = true;
                    }
                });
            });
        });

    if state.open {

    let area_size = egui::vec2(2.0 * R_OUT + 110.0, 2.0 * R_OUT + 24.0);
    let wheel_pos = match state.wheel_pos {
        Some(p) => p,
        None => {
            let sr = ctx.content_rect();
            let p = egui::pos2(46.0, sr.center().y - area_size.y / 2.0);
            state.wheel_pos = Some(p);
            p
        }
    };

    let mut wheel_center = wheel_pos + area_size * 0.5 + egui::vec2(20.0, 0.0);

    if cfg.tool_ui == crate::settings::ToolUi::Wheel {
        // Animacion de colapso (0 = rueda abierta, 1 = solo el punto de color).
        let collapse_t = ctx.animate_bool_with_time(egui::Id::new("wheel_collapse"), state.collapsed, 0.42);
        let dot_r = R_HOLE + 8.0; // radio interactivo del punto colapsado
        // Colapsado: el Area mide SOLO el punto (centrado en el centro de la rueda), asi NO
        // captura gestos del lienzo alrededor (arrastrar fuera del punto ya no lo movia, y
        // ademas se puede dibujar pegado a el). Abierto: el Area cubre toda la rueda.
        let area_origin = if state.collapsed {
            wheel_center - egui::Vec2::splat(dot_r)
        } else {
            wheel_pos
        };
    // ---------- La RUEDA radial (o el punto de color si esta colapsada) ----------
    // Un SOLO Area: asi no hay dos widgets que se turnen y se roben el clic.
    egui::Area::new(egui::Id::new("wheel"))
        .fixed_pos(area_origin)
        .constrain(false) // no reubicar cerca de las orillas (evita el "salto" del circulo)
        .show(ctx, |ui| {
            if state.collapsed {
                // Solo el punto de color; al tocarlo se reabre la rueda. El rect (cuadrado del
                // tamano del dot) ya acota la zona; ademas verificamos el circulo mas abajo.
                let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(dot_r * 2.0), egui::Sense::click_and_drag());
                let c = rect.center();
                wheel_center = c;
                let time = ui.input(|i| i.time) as f32;
                // Ondas tipo gota centradas en el dot REAL (`c`), no en una posicion
                // precalculada: asi no aparece un segundo circulo cerca de las orillas.
                draw_collapse_ripple(ui.ctx(), c, collapse_t, c32(brush.color));
                let p = ui.painter();
                p.circle_filled(c + egui::vec2(0.0, 2.0), R_HOLE + 3.0, Color32::from_black_alpha(22));
                p.circle_filled(c, R_HOLE + 2.0, c32(brush.color));
                p.circle_stroke(c, R_HOLE + 2.0, Stroke::new(2.5, Color32::from_gray(248)));
                // Anillo que orbita la orilla del circulo (animacion idle, con cola que
                // se desvanece). El lienzo ya repinta cada frame, asi que avanza solo.
                let accent = Color32::from_rgb(70, 140, 230);
                let head = (time * 220.0).rem_euclid(360.0);
                let arc = 110.0;
                let rr = R_HOLE + 2.0;
                let n = 20;
                for k in 0..n {
                    let a0 = head - arc * (k as f32 / n as f32);
                    let a1 = head - arc * ((k + 1) as f32 / n as f32);
                    let tail = 1.0 - k as f32 / n as f32; // 1 en la cabeza -> 0 en la cola
                    let alpha = (tail.powf(1.4) * 235.0) as u8;
                    p.line_segment(
                        [c + dir(a0) * rr, c + dir(a1) * rr],
                        Stroke::new(2.8, Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha)),
                    );
                }
                // Mover el punto SOLO si el arrastre empezo dentro del circulo (no fuera).
                if resp.drag_started() {
                    state.dot_drag = resp.interact_pointer_pos().is_some_and(|pp| (pp - c).length() <= dot_r);
                }
                if resp.dragged() && state.dot_drag {
                    if let Some(wp) = state.wheel_pos.as_mut() {
                        *wp += resp.drag_delta();
                    }
                }
                if resp.drag_stopped() {
                    state.dot_drag = false;
                }
                // Click dentro del circulo: reabrir la rueda.
                if resp.clicked() {
                    if let Some(pp) = resp.interact_pointer_pos() {
                        if (pp - c).length() <= dot_r {
                            state.collapsed = false;
                        }
                    }
                }
                return;
            }
            // Rueda ABIERTA: el rect cubre toda la rueda (arrastrar desde cualquier parte la mueve).
            let (rect, resp) = ui.allocate_exact_size(area_size, egui::Sense::click_and_drag());
            let c = rect.center() + egui::vec2(20.0, 0.0);
            wheel_center = c;
            let rot = state.wheel_rot;
            let time = ui.input(|i| i.time);
            // Progreso de RECONSTRUCCION (0 = nada, 1 = rueda completa). Al reabrir,
            // collapse_t baja 1->0, asi que build sube 0->1 y las piezas aparecen
            // escalonadas de afuera hacia adentro.
            let build = (1.0 - collapse_t).clamp(0.0, 1.0);
            let f_outer = reveal(build, 1.05, 0.5); // deshacer/rehacer (lo mas externo)
            let f_ring = reveal(build, 1.0, 0.0); // aro exterior
            let f_donut = reveal(build, 0.42, 0.0); // donut gris
            let f_center = reveal(build, 0.10, 0.0); // centro de color
            let seg_f = |i: usize| reveal(build, 0.80, i as f32 / N_SEG as f32);

            let gray = Color32::from_gray(208);
            let ring_bg = Color32::from_gray(250);
            let ink = Color32::from_gray(70);

            // Anillo exterior.
            p_circle(ui, c, R_OUT, fade(ring_bg, f_ring));
            // Segmento seleccionado (con el factor de su propio segmento).
            let sa = state.selected_seg as f32 * SEG_DEG + rot;
            fill_sector(ui.painter(), c, R_MID, R_OUT, sa - SEG_DEG / 2.0, sa + SEG_DEG / 2.0, fade(Color32::from_gray(28), seg_f(state.selected_seg)));
            // Lineas divisorias (cada una con su escalon).
            for i in 0..N_SEG {
                let a = i as f32 * SEG_DEG + SEG_DEG / 2.0 + rot;
                ui.painter().line_segment([c + dir(a) * R_MID, c + dir(a) * R_OUT], Stroke::new(1.0, fade(Color32::from_gray(226), seg_f(i))));
            }
            ui.painter().circle_stroke(c, R_OUT, Stroke::new(1.5, fade(Color32::from_gray(215), f_ring)));
            // Donut gris.
            p_circle(ui, c, R_MID, fade(gray, f_donut));

            // Resaltar la zona del control con panel abierto.
            let hl = Color32::from_rgb(206, 224, 248);
            match state.popup {
                Popup::Size => fill_sector(ui.painter(), c, R_HOLE, R_MID, -60.0, 60.0, fade(hl, f_donut)),
                Popup::Opacity => fill_sector(ui.painter(), c, R_HOLE, R_MID, 60.0, 150.0, fade(hl, f_donut)),
                Popup::Smoothing => fill_sector(ui.painter(), c, R_HOLE, R_MID, 210.0, 300.0, fade(hl, f_donut)),
                Popup::None => {}
            }

            let p = ui.painter().clone();

            // Iconos por segmento: aparecen escalonados y "llegan" desde un poco mas afuera.
            let ric = (R_MID + R_OUT) / 2.0 + 1.0;
            for i in 0..N_SEG {
                let fi = seg_f(i);
                if fi <= 0.001 {
                    continue;
                }
                let sel = i == state.selected_seg;
                let col = fade(if sel { Color32::WHITE } else { ink }, fi);
                let ric_eff = ric + (1.0 - fi) * 14.0; // overshoot radial -> ensamblaje
                let ipos = c + dir(i as f32 * SEG_DEG + rot) * ric_eff;
                match state.slots[i] {
                    SlotItem::Empty => icon_x(&p, ipos, 10.0, fade(if sel { Color32::WHITE } else { Color32::from_gray(125) }, fi)),
                    SlotItem::Brush(bi) => draw_wheel_item(&p, ipos, BRUSHES[bi], col),
                    SlotItem::Tool(ti) => draw_wheel_item(&p, ipos, TOOLS[ti], col),
                    SlotItem::Eraser => icon_eraser(&p, ipos, 17.0, col),
                    // Pincel de Photoshop: dibujar su FORMA real (miniatura) en el slot.
                    SlotItem::PsBrush(pi) => {
                        if let Some(Some(tid)) = state.ps_thumb_ids.get(pi as usize) {
                            let rr = egui::Rect::from_center_size(ipos, egui::vec2(27.0, 27.0));
                            p.image(*tid, rr, egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), fade(col, fi));
                        } else {
                            p.circle_filled(ipos, 6.0, col);
                        }
                    }
                }
            }

            // --- Donut interior: grosor (arriba), suavidad (izq), opacidad (der) ---
            let rside = (R_HOLE + R_MID) / 2.0;
            icon_grip(&p, c + egui::vec2(-26.0, -(R_HOLE + 14.0)), 7.0, fade(Color32::from_gray(120), f_donut));
            icon_brush_sample(&p, c + egui::vec2(-rside, -1.0), 8.0, fade(Color32::from_gray(110), f_donut));
            icon_opacity(&p, c + egui::vec2(rside, -1.0), 7.0, fade(Color32::from_gray(110), f_donut));
            // Boton de colapsar (abajo): oculta la rueda dejando solo el punto de color.
            icon_collapse(&p, c + egui::vec2(0.0, rside), 6.5, fade(Color32::from_gray(120), f_donut));

            // Circulo de color central.
            p.circle_filled(c, R_HOLE, fade(c32(brush.color), f_center));
            p.circle_stroke(c, R_HOLE, Stroke::new(2.0, fade(Color32::from_gray(245), f_center)));

            // Valores: "X pts" arriba (junto al grip); suavidad y opacidad abajo a los lados.
            p.text(c + egui::vec2(12.0, -(R_HOLE + 14.0)), Align2::CENTER_CENTER,
                cfg.format_measure(brush.width), FontId::proportional(11.0), fade(Color32::from_gray(55), f_donut));
            p.text(c + egui::vec2(-(R_HOLE + 18.0), R_HOLE + 8.0), Align2::CENTER_CENTER,
                format!("{:.0}%", brush.smoothing * 100.0), FontId::proportional(11.0), fade(Color32::from_gray(85), f_donut));
            p.text(c + egui::vec2(R_HOLE + 18.0, R_HOLE + 8.0), Align2::CENTER_CENTER,
                format!("{:.0}%", brush.opacity * 100.0), FontId::proportional(11.0), fade(Color32::from_gray(85), f_donut));

            // Deshacer / Rehacer (lo mas externo) con animacion de pulsacion al clicar.
            let undo_p = c + egui::vec2(-(R_OUT + 26.0), -28.0);
            let redo_p = c + egui::vec2(-(R_OUT + 26.0), 30.0);
            let up = press_factor((time - state.undo_press) as f32);
            let rp = press_factor((time - state.redo_press) as f32);
            if up > 0.02 {
                p.circle_filled(undo_p, 16.0, Color32::from_rgba_unmultiplied(120, 120, 130, (up * 70.0) as u8));
            }
            if rp > 0.02 {
                p.circle_filled(redo_p, 16.0, Color32::from_rgba_unmultiplied(120, 120, 130, (rp * 70.0) as u8));
            }
            icon_curved_arrow(&p, undo_p, 12.0 * (1.0 - 0.22 * up), fade(Color32::from_gray(if stats.can_undo { 40 } else { 185 }), f_outer), false);
            icon_curved_arrow(&p, redo_p, 12.0 * (1.0 - 0.22 * rp), fade(Color32::from_gray(if stats.can_redo { 40 } else { 185 }), f_outer), true);

            // --- Arrastre: dentro del donut = mover; en el aro exterior = girar ---
            if resp.drag_started() {
                state.dragging = false;
                state.rotating = false;
                if let Some(pp) = resp.interact_pointer_pos() {
                    let d = (pp - c).length();
                    if d <= R_MID {
                        state.dragging = true;
                    } else if d <= R_OUT {
                        state.rotating = true;
                    }
                }
            }
            if resp.dragged() {
                if state.dragging {
                    if let Some(wp) = state.wheel_pos.as_mut() {
                        *wp += resp.drag_delta();
                    }
                } else if state.rotating {
                    if let Some(pp) = resp.interact_pointer_pos() {
                        let prev = pp - resp.drag_delta();
                        let an = (pp - c).x.atan2(-(pp - c).y).to_degrees();
                        let ap = (prev - c).x.atan2(-(prev - c).y).to_degrees();
                        let mut dd = an - ap;
                        if dd > 180.0 {
                            dd -= 360.0;
                        }
                        if dd < -180.0 {
                            dd += 360.0;
                        }
                        state.wheel_rot = (state.wheel_rot + dd).rem_euclid(360.0);
                    }
                }
            }
            if resp.drag_stopped() {
                state.dragging = false;
                state.rotating = false;
            }

            // --- Clic (sin arrastre) ---
            if resp.clicked() {
                if let Some(pp) = resp.interact_pointer_pos() {
                    let v = pp - c;
                    let dist = v.length();
                    let ang = v.x.atan2(-v.y).to_degrees().rem_euclid(360.0);
                    if (pp - undo_p).length() < 20.0 {
                        actions.undo = true;
                        state.undo_press = time;
                    } else if (pp - redo_p).length() < 20.0 {
                        actions.redo = true;
                        state.redo_press = time;
                    } else if dist < R_HOLE {
                        state.popup = Popup::None;
                        state.show_colors = !state.show_colors;
                    } else if dist <= R_MID {
                        if (150.0..210.0).contains(&ang) {
                            // Zona inferior del donut -> colapsar la rueda.
                            state.collapsed = true;
                            state.show_colors = false;
                            state.popup = Popup::None;
                        } else {
                            // Resto de zonas del donut -> abrir/cerrar el panel de ajuste.
                            let z = if !(60.0..300.0).contains(&ang) {
                                Popup::Size
                            } else if ang < 150.0 {
                                Popup::Opacity
                            } else {
                                Popup::Smoothing
                            };
                            state.popup = if state.popup == z { Popup::None } else { z };
                        }
                    } else if dist <= R_OUT + 3.0 {
                        let sa = (ang - rot).rem_euclid(360.0);
                        let seg = (((sa / SEG_DEG).round()) as usize) % N_SEG;
                        let was_selected = seg == state.selected_seg;
                        state.selected_seg = seg;
                        state.popup = Popup::None;
                        actions.slot_selected = Some(seg);
                        match state.slots[seg] {
                            // Slot vacio: abrir panel para asignar pincel/herramienta.
                            SlotItem::Empty => {
                                state.editing_slot = seg;
                                state.brush_panel = true;
                                actions.exit_ps = true;
                            }
                            SlotItem::Brush(bi) => {
                                brush.width = brush_width_for(BRUSHES[bi]);
                                brush.kind = brush_kind_for(BRUSHES[bi]);
                                actions.exit_ps = true;
                                if was_selected {
                                    state.editing_slot = seg;
                                    state.brush_panel = true;
                                }
                            }
                            SlotItem::Tool(_) => {
                                actions.exit_ps = true;
                                if was_selected {
                                    state.editing_slot = seg;
                                    state.brush_panel = true;
                                }
                            }
                            // Goma: al seleccionarla se activa el modo borrador (derivado en
                            // main.rs). Re-tocarla abre el panel para cambiar de herramienta.
                            SlotItem::Eraser => {
                                actions.exit_ps = true;
                                if was_selected {
                                    state.editing_slot = seg;
                                    state.brush_panel = true;
                                }
                            }
                            // Pincel de Photoshop asignado a este slot: activarlo (no borrarlo).
                            SlotItem::PsBrush(pi) => {
                                actions.activate_ps = Some(pi);
                            }
                        }
                    }
                }
            }
        });
    } else {
        // ---------- Interfaz de BARRA (alternativa a la rueda) ----------
        let item_h = 34.0;
        let pad = 10.0;
        let bw = 58.0;
        let rows = 15usize;
        let bh = pad * 2.0 + rows as f32 * item_h;
        egui::Area::new(egui::Id::new("toolbar"))
            .fixed_pos(wheel_pos)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(bw, bh), egui::Sense::click_and_drag());
                let p = ui.painter_at(rect.expand(46.0));
                p.rect_filled(rect, egui::CornerRadius::same(16), Color32::from_gray(250));
                p.rect_stroke(rect, egui::CornerRadius::same(16), Stroke::new(1.0, Color32::from_gray(220)), egui::StrokeKind::Inside);
                let cx = rect.center().x;
                let row_c = |i: usize| egui::pos2(cx, rect.top() + pad + item_h * (i as f32 + 0.5));

                // Color (fila 0): tambien ancla del selector de color y los popups.
                let color_c = row_c(0);
                wheel_center = color_c;
                p.circle_filled(color_c, 13.0, c32(brush.color));
                p.circle_stroke(color_c, 13.0, Stroke::new(2.0, Color32::from_gray(235)));

                // Separador.
                p.line_segment(
                    [egui::pos2(rect.left() + 8.0, row_c(0).y + item_h * 0.5), egui::pos2(rect.right() - 8.0, row_c(0).y + item_h * 0.5)],
                    Stroke::new(1.0, Color32::from_gray(228)),
                );

                // Slots 1..=9.
                for s in 0..N_SEG {
                    let c = row_c(1 + s);
                    let sel = s == state.selected_seg;
                    if sel {
                        p.rect_filled(
                            egui::Rect::from_center_size(c, egui::vec2(bw - 12.0, item_h - 4.0)),
                            egui::CornerRadius::same(8),
                            Color32::from_gray(28),
                        );
                    }
                    let col = if sel { Color32::WHITE } else { Color32::from_gray(70) };
                    match state.slots[s] {
                        SlotItem::Empty => icon_x(&p, c, 9.0, if sel { Color32::WHITE } else { Color32::from_gray(125) }),
                        SlotItem::Brush(bi) => draw_wheel_item(&p, c, BRUSHES[bi], col),
                        SlotItem::Tool(ti) => draw_wheel_item(&p, c, TOOLS[ti], col),
                        SlotItem::Eraser => icon_eraser(&p, c, 15.0, col),
                        SlotItem::PsBrush(pi) => {
                            if let Some(Some(tid)) = state.ps_thumb_ids.get(pi as usize) {
                                let rr = egui::Rect::from_center_size(c, egui::vec2(22.0, 22.0));
                                p.image(*tid, rr, egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), col);
                            } else {
                                p.circle_filled(c, 5.0, col);
                            }
                        }
                    }
                }

                // Controles: tamano (10), suavidad (11), opacidad (12).
                let size_c = row_c(10);
                icon_grip(&p, size_c - egui::vec2(13.0, 0.0), 6.0, Color32::from_gray(120));
                p.text(size_c + egui::vec2(5.0, 0.0), Align2::CENTER_CENTER, num_label(brush.width), FontId::proportional(10.0), Color32::from_gray(60));
                let smooth_c = row_c(11);
                icon_brush_sample(&p, smooth_c - egui::vec2(13.0, 0.0), 7.0, Color32::from_gray(110));
                p.text(smooth_c + egui::vec2(7.0, 0.0), Align2::CENTER_CENTER, format!("{:.0}%", brush.smoothing * 100.0), FontId::proportional(10.0), Color32::from_gray(85));
                let op_c = row_c(12);
                icon_opacity(&p, op_c - egui::vec2(13.0, 0.0), 6.0, Color32::from_gray(110));
                p.text(op_c + egui::vec2(7.0, 0.0), Align2::CENTER_CENTER, format!("{:.0}%", brush.opacity * 100.0), FontId::proportional(10.0), Color32::from_gray(85));

                // Deshacer (13) / Rehacer (14).
                icon_curved_arrow(&p, row_c(13), 11.0, Color32::from_gray(if stats.can_undo { 40 } else { 185 }), false);
                icon_curved_arrow(&p, row_c(14), 11.0, Color32::from_gray(if stats.can_redo { 40 } else { 185 }), true);

                // --- Interaccion ---
                if resp.dragged() {
                    if let Some(wp) = state.wheel_pos.as_mut() {
                        *wp += resp.drag_delta();
                    }
                }
                if resp.clicked() {
                    if let Some(pp) = resp.interact_pointer_pos() {
                        let i = ((pp.y - (rect.top() + pad)) / item_h).floor() as i64;
                        match i {
                            0 => state.show_colors = !state.show_colors,
                            1..=9 => {
                                let seg = (i - 1) as usize;
                                let was = seg == state.selected_seg;
                                state.selected_seg = seg;
                                state.popup = Popup::None;
                                actions.slot_selected = Some(seg);
                                match state.slots[seg] {
                                    SlotItem::Empty => {
                                        state.editing_slot = seg;
                                        state.brush_panel = true;
                                        actions.exit_ps = true;
                                    }
                                    SlotItem::Brush(bi) => {
                                        brush.width = brush_width_for(BRUSHES[bi]);
                                        brush.kind = brush_kind_for(BRUSHES[bi]);
                                        actions.exit_ps = true;
                                        if was {
                                            state.editing_slot = seg;
                                            state.brush_panel = true;
                                        }
                                    }
                                    SlotItem::Tool(_) => {
                                        actions.exit_ps = true;
                                        if was {
                                            state.editing_slot = seg;
                                            state.brush_panel = true;
                                        }
                                    }
                                    SlotItem::Eraser => {
                                        actions.exit_ps = true;
                                        if was {
                                            state.editing_slot = seg;
                                            state.brush_panel = true;
                                        }
                                    }
                                    SlotItem::PsBrush(pi) => {
                                        actions.activate_ps = Some(pi);
                                    }
                                }
                            }
                            10 => state.popup = if state.popup == Popup::Size { Popup::None } else { Popup::Size },
                            11 => state.popup = if state.popup == Popup::Smoothing { Popup::None } else { Popup::Smoothing },
                            12 => state.popup = if state.popup == Popup::Opacity { Popup::None } else { Popup::Opacity },
                            13 => actions.undo = true,
                            14 => actions.redo = true,
                            _ => {}
                        }
                    }
                }
            });
    }

    // ---------- Paneles de ajuste (grosor / suavidad / opacidad), dibujados a medida ----------
    if state.popup != Popup::None {
        let pos = egui::pos2(wheel_center.x + R_OUT + 18.0, wheel_center.y - 50.0);
        egui::Area::new(egui::Id::new("value_popup"))
            .fixed_pos(pos)
            .show(ctx, |ui| match state.popup {
                Popup::Size => {
                    // Presets de tamano en la unidad actual (px por defecto), rango amplio
                    // para cubrir pinceles grandes.
                    let presets: Vec<(String, f32)> = [4.0_f32, 20.0, 60.0, 150.0]
                        .iter()
                        .map(|&v| (cfg.format_measure(v), v))
                        .collect();
                    draw_popup(ui, "TAMAÑO", &presets, &mut brush.width, 1.0, 300.0, icon_grip);
                }
                Popup::Smoothing => {
                    let presets = [
                        ("0%".to_string(), 0.0),
                        ("10%".to_string(), 0.10),
                        ("50%".to_string(), 0.50),
                        ("100%".to_string(), 1.0),
                    ];
                    draw_popup(ui, "SUAVIDAD", &presets, &mut brush.smoothing, 0.0, 1.0, icon_brush_sample);
                }
                Popup::Opacity => {
                    let presets = [
                        ("0%".to_string(), 0.0),
                        ("25%".to_string(), 0.25),
                        ("50%".to_string(), 0.50),
                        ("100%".to_string(), 1.0),
                    ];
                    draw_popup(ui, "OPACIDAD", &presets, &mut brush.opacity, 0.0, 1.0, icon_opacity);
                }
                Popup::None => {}
            });
    }

    // ---------- Panel "Mis pinceles" (pinceles + herramientas) ----------
    if state.brush_panel {
        egui::Area::new(egui::Id::new("brush_panel"))
            .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(496.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Mis pinceles").size(20.0).strong().color(Color32::from_gray(20)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if close_button(ui) {
                                state.brush_panel = false;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("BÁSICO").size(12.0).strong().color(Color32::from_gray(135)));
                    ui.add_space(3.0);
                    ui.horizontal_wrapped(|ui| {
                        for (bi, name) in BRUSHES.iter().enumerate() {
                            if item_cell(ui, name) {
                                state.slots[state.editing_slot] = SlotItem::Brush(bi);
                                state.selected_seg = state.editing_slot;
                                brush.width = brush_width_for(name);
                                brush.kind = brush_kind_for(name);
                                state.brush_panel = false;
                                actions.exit_ps = true;
                                actions.slot_selected = Some(state.editing_slot);
                            }
                        }
                    });
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("HERRAMIENTAS").size(12.0).strong().color(Color32::from_gray(135)));
                    ui.add_space(3.0);
                    ui.horizontal_wrapped(|ui| {
                        for (ti, name) in TOOLS.iter().enumerate() {
                            if item_cell(ui, name) {
                                state.slots[state.editing_slot] = SlotItem::Tool(ti);
                                state.selected_seg = state.editing_slot;
                                state.brush_panel = false;
                                actions.exit_ps = true;
                                actions.slot_selected = Some(state.editing_slot);
                            }
                        }
                    });
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("BORRADOR").size(12.0).strong().color(Color32::from_gray(135)));
                    ui.add_space(3.0);
                    ui.horizontal_wrapped(|ui| {
                        if item_cell(ui, "Goma") {
                            // La goma es una herramienta de la rueda: ocupa este slot con su
                            // icono. Al seleccionarla se activa el borrado (derivado en main.rs).
                            state.slots[state.editing_slot] = SlotItem::Eraser;
                            state.selected_seg = state.editing_slot;
                            state.brush_panel = false;
                            actions.exit_ps = true;
                            actions.slot_selected = Some(state.editing_slot);
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("La goma borra solo la zona tocada. Su tamaño y opacidad se ajustan en la rueda (igual que un pincel). Cambia a otro slot para volver a dibujar.")
                            .size(11.0)
                            .color(Color32::from_gray(140)),
                    );
                });
            });
    }

    // ---------- Selector de color: COPIC / HSL / RGB (modal, animado) ----------
    let copic_t = ctx.animate_bool_with_time(egui::Id::new("m_copic"), state.show_colors && state.color_mode == ColorMode::Copic, 0.32);
    let hsl_t = ctx.animate_bool_with_time(egui::Id::new("m_hsl"), state.show_colors && state.color_mode == ColorMode::Hsl, 0.42);
    let rgb_t = ctx.animate_bool_with_time(egui::Id::new("m_rgb"), state.show_colors && state.color_mode == ColorMode::Rgb, 0.42);
    // Si el selector se cerro por otra via (p.ej. al pintar), anular el cierre diferido.
    if !state.show_colors {
        state.copic_close_at = None;
    }
    if state.show_colors || copic_t.max(hsl_t).max(rgb_t) > 0.01 {
        let radii = [SP_R0 + 6.0, SP_R0 + 42.0, SP_R0 + 78.0];
        let cols = copic_columns();
        let ncols = cols.len().max(1);
        // El area de captura cubre la espiral/bandas (para poder clicar TODOS los swatches,
        // incluso los mas externos) y un poco mas; un clic fuera de ella dibuja y cierra.
        let copic_outer = SP_R0 + cols.iter().map(|col| col.len()).max().unwrap_or(7) as f32 * SP_DR;
        let pick_half = if state.color_mode == ColorMode::Copic { copic_outer + 36.0 } else { radii[2] + 44.0 };
        egui::Area::new(egui::Id::new("color_picker"))
            .fixed_pos(wheel_center - egui::vec2(pick_half, pick_half))
            .constrain(false)
            .show(ctx, |ui| {
                let screen = ctx.content_rect();
                let (_r, resp) = ui.allocate_exact_size(egui::vec2(pick_half * 2.0, pick_half * 2.0), egui::Sense::click_and_drag());
                let time = ui.input(|i| i.time);
                // Cierre diferido del selector COPIC: tras el "salto" del color elegido,
                // se oculta la espiral con su animacion normal.
                if let Some(t) = state.copic_close_at {
                    if time >= t {
                        state.show_colors = false;
                        state.copic_close_at = None;
                    }
                }
                let p = ui.painter_at(screen);
                let c = wheel_center;

                // --- COPIC (espiral) ---
                if copic_t > 0.01 {
                    let grow = SP_DR * copic_t;
                    let alpha = (copic_t * 255.0) as u8;
                    let col_w = SP_SWEEP / ncols as f32;
                    let sr = state.spiral_rot;
                    for (ci, col) in cols.iter().enumerate() {
                        let amid = SP_START + sr + (ci as f32 + 0.5) * col_w;
                        let a0 = SP_START + sr + ci as f32 * col_w + 0.4;
                        let a1 = SP_START + sr + (ci as f32 + 1.0) * col_w - 0.4;
                        for (ri, &(code, rgb)) in col.iter().enumerate() {
                            // "Salto" de confirmacion del swatch recien elegido.
                            let pop = if state.copic_pop == Some((ci, ri)) {
                                copic_pop_factor((time - state.copic_pop_t) as f32)
                            } else {
                                0.0
                            };
                            let g = pop * 6.0;
                            let r_in = SP_R0 + ri as f32 * grow + 1.0 - g;
                            let r_out = SP_R0 + (ri as f32 + 1.0) * grow - 1.0 + g;
                            let quad = vec![c + dir(a0) * r_out, c + dir(a1) * r_out, c + dir(a1) * r_in, c + dir(a0) * r_in];
                            let cc = rgb_to_color(rgb);
                            p.add(Shape::convex_polygon(quad.clone(), Color32::from_rgba_unmultiplied(cc.r(), cc.g(), cc.b(), alpha), Stroke::NONE));
                            if pop > 0.02 {
                                // Borde blanco que resalta el cuadro elegido mientras salta.
                                p.add(Shape::closed_line(quad, Stroke::new(2.0 + pop * 2.5, Color32::from_rgba_unmultiplied(255, 255, 255, alpha))));
                            }
                            if copic_t > 0.65 {
                                let tc = if luminance(rgb) > 150.0 { Color32::from_gray(45) } else { Color32::from_gray(235) };
                                draw_radial_text(&p, c + dir(amid) * (r_in + r_out) * 0.5, amid, code, tc);
                            }
                        }
                    }
                }
                // Radio efectivo de la banda i: se desliza hacia el centro (R_HOLE)
                // conforme su factor escalonado baja al cerrar.
                let band_r = |i: usize, bi: f32| R_HOLE + (radii[i] - R_HOLE) * bi;

                // --- HSL --- (cada banda se mete al centro en su propio tiempo)
                if hsl_t > 0.01 {
                    let (h, s, l) = rgb_to_hsl(brush.color[0], brush.color[1], brush.color[2]);
                    let to3 = |t: (f32, f32, f32)| [(t.0 * 255.0) as u8, (t.1 * 255.0) as u8, (t.2 * 255.0) as u8];
                    let g0 = |x: f32| to3(hsl_to_rgb(x * 360.0, 1.0, 0.5));
                    let g1 = |x: f32| to3(hsl_to_rgb(h, x, l.clamp(0.25, 0.75)));
                    let g2 = |x: f32| to3(hsl_to_rgb(h, s, x));
                    let grads: [&dyn Fn(f32) -> [u8; 3]; 3] = [&g0, &g1, &g2];
                    let vals = [h / 360.0, s, l];
                    let labels = [format!("{:.0}°", h), format!("{:.0}%", s * 100.0), format!("{:.0}%", l * 100.0)];
                    let bi: Vec<f32> = (0..3).map(|i| band_stagger(hsl_t, i)).collect();
                    for i in 0..3 {
                        draw_arc(&p, c, band_r(i, bi[i]), bi[i], vals[i], grads[i]);
                    }
                    for i in 0..3 {
                        draw_arc_value(&p, c, band_r(i, bi[i]), vals[i], &labels[i], bi[i]);
                    }
                }
                // --- RGB ---
                if rgb_t > 0.01 {
                    let (r, g, b) = (brush.color[0], brush.color[1], brush.color[2]);
                    let gr = |x: f32| [(x * 255.0) as u8, 0u8, 0u8];
                    let gg = |x: f32| [0u8, (x * 255.0) as u8, 0u8];
                    let gb = |x: f32| [0u8, 0u8, (x * 255.0) as u8];
                    let grads: [&dyn Fn(f32) -> [u8; 3]; 3] = [&gr, &gg, &gb];
                    let vals = [r, g, b];
                    let labels = [format!("{}", (r * 255.0) as i32), format!("{}", (g * 255.0) as i32), format!("{}", (b * 255.0) as i32)];
                    let bi: Vec<f32> = (0..3).map(|i| band_stagger(rgb_t, i)).collect();
                    for i in 0..3 {
                        draw_arc(&p, c, band_r(i, bi[i]), bi[i], vals[i], grads[i]);
                    }
                    for i in 0..3 {
                        draw_arc_value(&p, c, band_r(i, bi[i]), vals[i], &labels[i], bi[i]);
                    }
                }

                // --- Etiquetas COPIC/HSL/RGB + cuentagotas (como "chips" con fondo, asi
                //     tapan los swatches detras y NO se traslapan con la espiral) ---
                let lx = c.x + R_OUT + 22.0;
                let chip_cx = lx + 30.0;
                let lbl = |y: f32, txt: &str, active: bool| {
                    let center = egui::pos2(chip_cx, c.y + y);
                    let r = egui::Rect::from_center_size(center, egui::vec2(64.0, 26.0));
                    let (fill, border, tcol) = if active {
                        (Color32::from_rgb(224, 235, 250), Color32::from_rgb(70, 140, 230), Color32::from_gray(20))
                    } else {
                        (Color32::from_gray(250), Color32::from_gray(212), Color32::from_gray(110))
                    };
                    p.rect_filled(r, egui::CornerRadius::same(8), fill);
                    p.rect_stroke(r, egui::CornerRadius::same(8), Stroke::new(1.0, border), egui::StrokeKind::Inside);
                    p.text(center, Align2::CENTER_CENTER, txt, FontId::proportional(13.0), tcol);
                };
                lbl(-42.0, "COPIC", state.color_mode == ColorMode::Copic);
                lbl(0.0, "HSL", state.color_mode == ColorMode::Hsl);
                lbl(42.0, "RGB", state.color_mode == ColorMode::Rgb);
                // Cuentagotas dentro de un chip circular.
                let drop_pos = egui::pos2(chip_cx, c.y + 82.0);
                p.circle_filled(drop_pos, 16.0, if state.eyedropper { Color32::from_rgb(224, 235, 250) } else { Color32::from_gray(250) });
                p.circle_stroke(drop_pos, 16.0, Stroke::new(1.0, if state.eyedropper { Color32::from_rgb(70, 140, 230) } else { Color32::from_gray(212) }));
                icon_dropper(&p, drop_pos, 10.0, if state.eyedropper { Color32::from_rgb(40, 120, 220) } else { Color32::from_gray(95) });

                // --- Interaccion ---
                if state.show_colors {
                    let set_hsl = |brush: &mut Brush, arc: usize, t: f32| {
                        let (mut h, mut s, mut l) = rgb_to_hsl(brush.color[0], brush.color[1], brush.color[2]);
                        match arc {
                            0 => h = t * 360.0,
                            1 => s = t,
                            _ => l = t,
                        }
                        let (r, g, b) = hsl_to_rgb(h, s, l);
                        brush.color = [r, g, b, 1.0];
                    };
                    if resp.clicked() {
                        if let Some(pp) = resp.interact_pointer_pos() {
                            // Zona del chip por etiqueta (alineada con el dibujo, sin solaparse).
                            let lbl_hit = |y: f32| {
                                (pp.x - chip_cx).abs() <= 36.0 && (pp.y - (c.y + y)).abs() <= 15.0
                            };
                            if lbl_hit(-42.0) {
                                state.color_mode = ColorMode::Copic;
                            } else if lbl_hit(0.0) {
                                state.color_mode = ColorMode::Hsl;
                            } else if lbl_hit(42.0) {
                                state.color_mode = ColorMode::Rgb;
                            } else if (pp - drop_pos).length() < 19.0 {
                                state.eyedropper = true;
                                state.show_colors = false;
                            } else {
                                match state.color_mode {
                                    ColorMode::Copic => {
                                        let v = pp - c;
                                        let dist = v.length();
                                        // Restamos la rotacion de la espiral para mapear el punto al color.
                                        let ang = (v.x.atan2(-v.y).to_degrees() - state.spiral_rot).rem_euclid(360.0);
                                        let au = if ang >= SP_START {
                                            ang
                                        } else if ang <= SP_START + SP_SWEEP - 360.0 {
                                            ang + 360.0
                                        } else {
                                            f32::NAN
                                        };
                                        let t = (au - SP_START) / SP_SWEEP;
                                        let mut chose = false;
                                        if t.is_finite() && (0.0..=1.0).contains(&t) && dist >= SP_R0 {
                                            let ci = ((t * ncols as f32) as usize).min(ncols - 1);
                                            if !cols[ci].is_empty() {
                                                let ri = (((dist - SP_R0) / SP_DR) as usize).min(cols[ci].len() - 1);
                                                let cc = rgb_to_color(cols[ci][ri].1);
                                                brush.color = [cc.r() as f32 / 255.0, cc.g() as f32 / 255.0, cc.b() as f32 / 255.0, 1.0];
                                                // Resaltar el color elegido (salto) y CERRAR DESPUES,
                                                // para que se vea cual se selecciono.
                                                state.copic_pop = Some((ci, ri));
                                                state.copic_pop_t = time;
                                                state.copic_close_at = Some(time + 0.45);
                                                chose = true;
                                            }
                                        }
                                        // Tocar el centro/hueco (sin elegir swatch) cierra de inmediato.
                                        if !chose {
                                            state.show_colors = false;
                                        }
                                    }
                                    ColorMode::Hsl => {
                                        if let Some((arc, t)) = arc_hit(c, pp, &radii) {
                                            set_hsl(brush, arc, t);
                                        } else {
                                            state.show_colors = false;
                                        }
                                    }
                                    ColorMode::Rgb => {
                                        if let Some((arc, t)) = arc_hit(c, pp, &radii) {
                                            brush.color[arc] = t;
                                        } else {
                                            state.show_colors = false;
                                        }
                                    }
                                }
                            }
                        }
                    } else if resp.dragged() {
                        if let Some(pp) = resp.interact_pointer_pos() {
                            if state.color_mode == ColorMode::Copic {
                                // Girar la espiral de colores (arrastrar = rotar, como la rueda).
                                let dist = (pp - c).length();
                                if dist >= R_HOLE + 8.0 {
                                    let prev = pp - resp.drag_delta();
                                    let an = (pp - c).x.atan2(-(pp - c).y).to_degrees();
                                    let ap = (prev - c).x.atan2(-(prev - c).y).to_degrees();
                                    let mut dd = an - ap;
                                    if dd > 180.0 {
                                        dd -= 360.0;
                                    }
                                    if dd < -180.0 {
                                        dd += 360.0;
                                    }
                                    state.spiral_rot = (state.spiral_rot + dd).rem_euclid(360.0);
                                }
                            } else {
                                // HSL/RGB: fijar la banda al empezar a arrastrar y mantenerla
                                // (no saltar a otra banda aunque el raton se acerque a ella).
                                if state.active_arc.is_none() {
                                    state.active_arc = arc_hit(c, pp, &radii).map(|(a, _)| a);
                                }
                                if let Some(arc) = state.active_arc {
                                    let tv = arc_value(c, pp);
                                    match state.color_mode {
                                        ColorMode::Hsl => set_hsl(brush, arc, tv),
                                        ColorMode::Rgb => brush.color[arc] = tv,
                                        ColorMode::Copic => {}
                                    }
                                }
                            }
                        }
                    }
                    // Al soltar, liberar la banda fijada.
                    if resp.drag_stopped() {
                        state.active_arc = None;
                    }
                }
            });
    }

    // ---------- Lista de opciones (abajo-izquierda) ----------
    egui::Area::new(egui::Id::new("options"))
        .anchor(Align2::LEFT_BOTTOM, egui::vec2(16.0, -16.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(230.0);
                if icon_text_row(ui, state.show_ps_panel, "Pinceles", |p, c| {
                    preview_wave(p, c, 9.0, 2.4, Color32::from_gray(70));
                }) {
                    state.show_ps_panel = !state.show_ps_panel;
                }
                if icon_text_row(ui, state.show_layers, "Capas", icon_layers) {
                    state.show_layers = !state.show_layers;
                }
                if icon_text_row(ui, false, "Precisión", icon_precision) {
                    state.show_settings = true;
                    state.settings_tab = SettingsTab::Workspace;
                }
                ui.separator();

                // Cuadricula: el check enciende/apaga la rejilla REAL (cfg.grid);
                // el nombre del tipo (a la derecha) abre el panel de ajustes.
                ui.horizontal(|ui| {
                    let mut on = cfg.grid != ink_core::GridKind::None;
                    if ui.checkbox(&mut on, "Cuadrícula").changed() {
                        cfg.grid = if on {
                            if state.last_grid != ink_core::GridKind::None {
                                state.last_grid
                            } else {
                                ink_core::GridKind::Squares
                            }
                        } else {
                            ink_core::GridKind::None
                        };
                    }
                    ui.label("·");
                    if ui
                        .link(egui::RichText::new(grid_name(cfg.grid)).color(Color32::from_rgb(40, 110, 210)))
                        .on_hover_text("Abrir opciones de papel y cuadrícula")
                        .clicked()
                    {
                        state.show_settings = true;
                        state.settings_tab = SettingsTab::Workspace;
                    }
                });

                ui.checkbox(&mut state.snap, "Ajustar  ·  Opciones");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.measure, "Medir");
                    ui.label("·");
                    if ui
                        .link(egui::RichText::new(format!("{:.0}:{:.0} {}", cfg.scale_from, cfg.scale_to, cfg.unit_abbrev())).color(Color32::from_rgb(40, 110, 210)))
                        .clicked()
                    {
                        state.show_settings = true;
                        state.settings_tab = SettingsTab::Workspace;
                    }
                });
                ui.add_enabled(false, egui::Checkbox::new(&mut false, "Guía  ·  Arco"));
                ui.add_enabled(false, egui::Checkbox::new(&mut false, "Reconocimiento  ·  Opciones"));
                ui.label(
                    egui::RichText::new(format!("{:.0} FPS  ·  {} trazos  ·  {:.2}x", stats.fps, stats.strokes, stats.zoom))
                        .weak()
                        .small(),
                );
            });
        });
    } // fin de `if state.open`

    // El panel de capas (estilo Photoshop), si esta abierto.
    layers_panel(ctx, state, doc, &mut actions);

    // El panel de ajustes (modal) se dibuja por ENCIMA de todo (incluida la rueda).
    settings_panel(ctx, state, cfg, brush);

    let _ = stats.verts;
    actions
}

/// Circulo relleno usando el painter del `ui` (helper corto).
fn p_circle(ui: &egui::Ui, c: Pos2, r: f32, fill: Color32) {
    ui.painter().circle_filled(c, r, fill);
}

/// Panel de ajuste dibujado a medida (estilo Concepts): presets en fila, slider de
/// linea con tirador redondo, titulo centrado y botones -/+ con icono. Edita `value`.
fn draw_popup(
    ui: &mut egui::Ui,
    title: &str,
    presets: &[(String, f32)],
    value: &mut f32,
    min: f32,
    max: f32,
    icon: fn(&egui::Painter, Pos2, f32, Color32),
) {
    let size = egui::vec2(348.0, 100.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let p = ui.painter_at(rect);
    let rr = egui::CornerRadius::same(12);
    p.rect_filled(rect, rr, Color32::from_gray(240));
    p.rect_stroke(rect, rr, Stroke::new(1.0, Color32::from_gray(208)), egui::StrokeKind::Inside);

    let pad = 18.0;
    let x0 = rect.left() + pad;
    let x1 = rect.right() - pad;
    let span = x1 - x0;
    let n = presets.len() as f32;

    // Fila de presets (el seleccionado en recuadro blanco).
    let py = rect.top() + 22.0;
    for (i, (lbl, v)) in presets.iter().enumerate() {
        let cx = x0 + span * (i as f32 + 0.5) / n;
        let sel = (*value - *v).abs() <= (max - min) * 0.012 + 0.001;
        if sel {
            let r = egui::Rect::from_center_size(egui::pos2(cx, py), egui::vec2(60.0, 24.0));
            p.rect_filled(r, egui::CornerRadius::same(6), Color32::WHITE);
        }
        p.text(
            egui::pos2(cx, py),
            Align2::CENTER_CENTER,
            lbl.as_str(),
            FontId::proportional(15.0),
            if sel { Color32::from_gray(20) } else { Color32::from_gray(95) },
        );
    }

    // Slider de linea con marcas y tirador redondo.
    let sy = rect.top() + 52.0;
    p.line_segment([egui::pos2(x0, sy), egui::pos2(x1, sy)], Stroke::new(2.0, Color32::from_gray(70)));
    for (_, v) in presets {
        let tx = x0 + span * ((*v - min) / (max - min));
        p.line_segment([egui::pos2(tx, sy - 4.0), egui::pos2(tx, sy + 4.0)], Stroke::new(1.5, Color32::from_gray(150)));
    }
    let hx = x0 + span * ((*value - min) / (max - min)).clamp(0.0, 1.0);
    p.circle_filled(egui::pos2(hx, sy), 8.0, Color32::WHITE);
    p.circle_stroke(egui::pos2(hx, sy), 8.0, Stroke::new(1.6, Color32::from_gray(110)));

    // Fila inferior: −icono ... TITULO ... +icono
    let by = rect.bottom() - 20.0;
    p.text(egui::pos2(rect.center().x, by), Align2::CENTER_CENTER, title, FontId::proportional(14.0), Color32::from_gray(70));
    icon(&p, egui::pos2(x0 + 6.0, by), 7.0, Color32::from_gray(60));
    p.text(egui::pos2(x0 + 22.0, by), Align2::CENTER_CENTER, "−", FontId::proportional(20.0), Color32::from_gray(50));
    icon(&p, egui::pos2(x1 - 22.0, by), 7.0, Color32::from_gray(60));
    p.text(egui::pos2(x1 - 6.0, by), Align2::CENTER_CENTER, "+", FontId::proportional(20.0), Color32::from_gray(50));

    // Interaccion: presets (arriba), slider (medio), −/+ (abajo).
    let step = (max - min) * 0.02;
    if resp.clicked() || resp.dragged() {
        if let Some(pp) = resp.interact_pointer_pos() {
            if pp.y < py + 14.0 {
                let i = (((pp.x - x0) / span) * n).floor().clamp(0.0, n - 1.0) as usize;
                *value = presets[i].1;
            } else if (pp.y - sy).abs() < 18.0 {
                *value = (min + (max - min) * ((pp.x - x0) / span)).clamp(min, max);
            } else if pp.y > by - 16.0 {
                if pp.x < rect.center().x {
                    *value = (*value - step).max(min);
                } else {
                    *value = (*value + step).min(max);
                }
            }
        }
    }
}
