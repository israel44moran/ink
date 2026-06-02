//! Ajustes del lienzo e interaccion (panel estilo Concepts: "Area de trabajo" e
//! "Interaccion"). Aqui vive el ESTADO y la logica de cada opcion (colores de
//! papel, color de rejilla, conversion/format de medidas). La UI (en `ui.rs`)
//! solo lee/escribe estos campos; el shell (`main.rs`) los aplica al render.

use ink_core::GridKind;

// ----------------------------- Enums -----------------------------

/// Papel / color de fondo.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BgKind {
    Custom,
    MatteWhite,
    Transparent,
    Crumpled,
    Light,
    Heavy,
    Wavy,
    FlatBlue,
    BrownPaper,
    FlatDark,
}

/// Tamano de la mesa de trabajo (marco de referencia).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Artboard {
    Infinite,
    R1024x768,
    A4,
    R1080p,
}

/// Pestana del sistema de unidades.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UnitTab {
    Digital,
    Metric,
    Imperial,
}

/// Unidad concreta.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Px,
    Pts,
    Mm,
    Cm,
    M,
    In,
    Ft,
}

/// Paleta de herramientas preferida.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToolUi {
    Wheel,
    Bar,
}

/// Accion del dedo mientras el lapiz dibuja (tactil).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FingerAction {
    Nothing,
    ActiveTool,
    Pan,
    Select,
    Push,
    Segment,
    Zoom,
    Rotate,
}

/// Accion al mantener pulsado (tactil).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HoldAction {
    LastUsed,
    Nothing,
    Lasso,
    ElementPicker,
    ColorPicker,
}

/// Accion de un toque con varios dedos.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TapAction {
    Nothing,
    Undo,
    Redo,
    SelectLast,
    ShowLayers,
    ShowColors,
    ToolConfig,
    ShowObjects,
    ToggleRotation,
    ToggleZoom,
    SelectAll,
    ToggleInterface,
}

// ----------------------------- Estado -----------------------------

#[derive(Clone)]
pub struct Settings {
    // --- Lienzo ---
    pub bg: BgKind,
    pub bg_custom: [f32; 4],
    pub grid: GridKind,
    pub grid_size: f32, // separacion base de celda en unidades de mundo

    // --- Mesa de trabajo ---
    pub artboard: Artboard,

    // --- Medidas ---
    pub scale_from: f32,
    pub scale_to: f32,
    pub unit_tab: UnitTab,
    pub unit: Unit,
    pub name_full: bool, // Completo (true) vs Abreviado (false)
    pub tenths: bool,    // Decimas (1 decimal) vs Redondeado (0)
    pub show_stroke_length: bool,
    pub show_scale_statusbar: bool,

    // --- Ajuste de herramienta ---
    pub tool_ui: ToolUi,

    // --- Interaccion: raton/teclado ---
    pub shortcuts_enabled: bool,

    // --- Interaccion: tactil ---
    pub finger_action: FingerAction,
    pub two_finger_zoom: bool,
    pub two_finger_photo_zoom: bool,
    pub two_finger_rotate: bool,
    pub two_finger_photo_rotate: bool,
    pub hold_action: HoldAction,
    pub hold_time: f32,
    pub highlight_selection: bool,
    pub shape_recognition: bool,
    pub draw_hold_time: f32,
    pub two_finger_tap: TapAction,
    pub three_finger_tap: TapAction,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bg: BgKind::MatteWhite,
            bg_custom: [0.96, 0.96, 0.98, 1.0],
            grid: GridKind::None,
            grid_size: 24.0,
            artboard: Artboard::Infinite,
            scale_from: 1.0,
            scale_to: 1.0,
            unit_tab: UnitTab::Digital,
            unit: Unit::Pts,
            name_full: false,
            tenths: true,
            show_stroke_length: false,
            show_scale_statusbar: false,
            tool_ui: ToolUi::Wheel,
            shortcuts_enabled: true,
            finger_action: FingerAction::ActiveTool,
            two_finger_zoom: true,
            two_finger_photo_zoom: true,
            two_finger_rotate: true,
            two_finger_photo_rotate: true,
            hold_action: HoldAction::LastUsed,
            hold_time: 0.4,
            highlight_selection: true,
            shape_recognition: false,
            draw_hold_time: 0.8,
            two_finger_tap: TapAction::Undo,
            three_finger_tap: TapAction::Redo,
        }
    }
}

impl Settings {
    /// Color de fondo (papel) para limpiar el lienzo. RGBA lineal 0..=1.
    pub fn bg_color(&self) -> [f32; 4] {
        match self.bg {
            BgKind::Custom => self.bg_custom,
            BgKind::MatteWhite => [1.0, 1.0, 1.0, 1.0],
            BgKind::Transparent => [0.95, 0.95, 0.96, 1.0],
            BgKind::Crumpled => [0.95, 0.94, 0.91, 1.0],
            BgKind::Light => [0.97, 0.97, 0.97, 1.0],
            BgKind::Heavy => [0.92, 0.92, 0.92, 1.0],
            BgKind::Wavy => [0.94, 0.95, 0.96, 1.0],
            BgKind::FlatBlue => [0.16, 0.45, 0.70, 1.0],
            BgKind::BrownPaper => [0.58, 0.42, 0.26, 1.0],
            BgKind::FlatDark => [0.10, 0.11, 0.13, 1.0],
        }
    }

    /// Luminancia aproximada del fondo (para decidir colores contrastados).
    pub fn bg_luma(&self) -> f32 {
        let c = self.bg_color();
        0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
    }

    /// Color de las lineas de rejilla (claras sobre fondo oscuro y viceversa).
    pub fn grid_color(&self) -> [f32; 4] {
        if self.bg_luma() > 0.5 {
            [0.0, 0.0, 0.05, 0.16]
        } else {
            [1.0, 1.0, 1.0, 0.20]
        }
    }

    /// Color de tinta por defecto sugerido segun el fondo (oscuro sobre claro).
    pub fn default_ink(&self) -> [f32; 4] {
        if self.bg_luma() > 0.5 {
            [0.10, 0.10, 0.13, 1.0]
        } else {
            [0.95, 0.95, 0.97, 1.0]
        }
    }

    /// Factor para convertir 1 unidad de mundo (= 1 pt) a la unidad activa.
    fn unit_factor(&self) -> f32 {
        match self.unit {
            Unit::Px => 1.0,
            Unit::Pts => 1.0,
            Unit::Mm => 0.352_777_8, // 1 pt = 1/72 in = 0.35278 mm
            Unit::Cm => 0.035_277_78,
            Unit::M => 0.000_352_777_8,
            Unit::In => 1.0 / 72.0,
            Unit::Ft => 1.0 / (72.0 * 12.0),
        }
    }

    pub fn unit_abbrev(&self) -> &'static str {
        match self.unit {
            Unit::Px => "px",
            Unit::Pts => "pts",
            Unit::Mm => "mm",
            Unit::Cm => "cm",
            Unit::M => "m",
            Unit::In => "in",
            Unit::Ft => "ft",
        }
    }

    pub fn unit_full(&self) -> &'static str {
        match self.unit {
            Unit::Px => "pixeles",
            Unit::Pts => "puntos",
            Unit::Mm => "milimetros",
            Unit::Cm => "centimetros",
            Unit::M => "metros",
            Unit::In => "pulgadas",
            Unit::Ft => "pies",
        }
    }

    /// Formatea una medida (en pts) con una notacion concreta (para previsualizar
    /// las opciones de "Formato de visualizacion").
    pub fn format_with(&self, name_full: bool, tenths: bool, world: f32) -> String {
        let scale = if self.scale_from.abs() > 1e-6 { self.scale_to / self.scale_from } else { 1.0 };
        let v = world * self.unit_factor() * scale;
        let num = if tenths { format!("{:.1}", v) } else { format!("{:.0}", v) };
        let name = if name_full { self.unit_full() } else { self.unit_abbrev() };
        format!("{} {}", num, name)
    }

    /// Formatea una medida dada en unidades de mundo (pts) segun unidad, escala
    /// de dibujo y notacion preferida. Ej: "6.5 pts", "6 puntos".
    pub fn format_measure(&self, world: f32) -> String {
        self.format_with(self.name_full, self.tenths, world)
    }

    /// Tamano de la mesa de trabajo en unidades de mundo, o None si es infinita.
    pub fn artboard_size(&self) -> Option<(f32, f32)> {
        match self.artboard {
            Artboard::Infinite => None,
            Artboard::R1024x768 => Some((1024.0, 768.0)),
            Artboard::A4 => Some((595.0, 842.0)), // A4 en puntos (210x297 mm)
            Artboard::R1080p => Some((1920.0, 1080.0)),
        }
    }
}
