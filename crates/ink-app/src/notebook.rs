//! Cuadernos de notas: persistencia en disco del dibujo (trazos, texto, borrados) y la
//! biblioteca (listar / crear / abrir / borrar). Cada cuaderno es un archivo JSON en
//! `Documentos/Cuadernos Ink/`. Las herramientas y pinceles son los mismos que en el
//! lienzo infinito; aqui solo se guarda/restaura el contenido.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

fn default_tick() -> f32 {
    1.0
}

fn default_finish() -> u32 {
    1 // holografico
}

fn default_intensity() -> f32 {
    1.0
}

fn default_one() -> f32 {
    1.0
}

fn default_white3() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

/// Una pagina (hoja) del cuaderno: su propio dibujo, texto y borrados. Un cuaderno
/// infinito tiene UNA pagina (el espacio infinito); uno de hojas tiene varias.
#[derive(Clone, Serialize, Deserialize)]
pub struct PageData {
    pub doc: ink_core::Document,
    #[serde(default)]
    pub texts: Vec<ink_core::TextItem>,
    /// Trazos de goma (cada uno = discos [cx, cy, radio, tiempo]).
    #[serde(default)]
    pub erase_strokes: Vec<Vec<[f32; 4]>>,
    /// Reloj logico (para la goma por timestamps).
    #[serde(default = "default_tick")]
    pub tick: f32,
    /// Cuerpo de TEXTO de la hoja (modo escritura/documento), en Markdown. La tinta se dibuja
    /// encima. Vacio = hoja sin texto.
    #[serde(default)]
    pub body: String,
    /// Alineacion del texto de la hoja: 0 izquierda, 1 centro, 2 derecha, 3 justificado.
    #[serde(default)]
    pub align: u32,
}

impl PageData {
    pub fn empty() -> Self {
        Self {
            doc: ink_core::Document::new(),
            texts: Vec::new(),
            erase_strokes: Vec::new(),
            tick: 1.0,
            body: String::new(),
            align: 0,
        }
    }
}

/// Ajustes de DISEÑO de pagina del modo escritura (documento): se aplican a todo el cuaderno.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct DocLayout {
    /// Margenes en PUNTOS: [arriba, derecha, abajo, izquierda].
    pub margins: [f32; 4],
    /// Interlineado (multiplicador: 1.0, 1.15, 1.5, 2.0...).
    pub line_spacing: f32,
    /// Espacio despues de cada parrafo, en puntos.
    pub para_spacing: f32,
    /// Tamano de fuente base, en puntos.
    pub font_size: f32,
    /// Familia: 0=Sans (Hanken), 1=Serif (Spectral), 2=Mono (JetBrains), 3=Lora, 4=Merriweather,
    /// 5=Garamond, 6=Atkinson, 7=Source Sans, 8=Montserrat, 9=Roboto, 10=Inter, 11=Josefin, 12=Nunito.
    pub font: u32,
    /// Ancho de columna BASE para las tablas NUEVAS (0.0..1.0). Cada tabla guarda el suyo al
    /// crearse (en su directiva), asi que cambiar esto NO afecta a las tablas ya puestas.
    #[serde(default = "default_table_scale")]
    pub table_scale: f32,
    /// Alto de fila para las tablas NUEVAS (multiplicador, 0.6..2.5). Igual: por-tabla.
    #[serde(default = "default_table_row")]
    pub table_row: f32,
}

fn default_table_scale() -> f32 {
    0.5
}
fn default_table_row() -> f32 {
    1.0
}

impl Default for DocLayout {
    fn default() -> Self {
        Self {
            margins: [64.0, 56.0, 64.0, 56.0],
            line_spacing: 1.5,
            para_spacing: 8.0,
            font_size: 16.0,
            font: 0,
            table_scale: 0.5,
            table_row: 1.0,
        }
    }
}

fn default_doc_layout() -> DocLayout {
    DocLayout::default()
}

/// Contenido serializable de un cuaderno (lo que se guarda en disco).
#[derive(Serialize, Deserialize)]
pub struct NotebookData {
    #[serde(default)]
    pub version: u32,
    pub name: String,
    /// `true` = lienzo infinito (1 pagina); `false` = con hojas (varias paginas A4).
    pub infinite: bool,
    /// Diseño BASE de la "carta" en la biblioteca. Foils: 0=mate, 1=holo, 2=galaxia, 3=oro,
    /// 4=prisma, 5=destellos, 6=aurora, 7=neon, 8=esmeralda, 9=rubi, 10=cromo, 11=atardecer.
    /// Cargadores organicos animados: >= 100 (100+indice).
    #[serde(default = "default_finish")]
    pub finish: u32,
    /// Capas/efectos COMBINABLES encima del diseño base (mascara de bits):
    /// 1=destellos, 2=brillo animado (barrido), 4=resplandor (latido).
    #[serde(default)]
    pub fx: u32,
    /// Intensidad de las capas/efectos (0..1).
    #[serde(default = "default_intensity")]
    pub fx_intensity: f32,
    /// Acento de color (indice de paleta; 0 = por defecto / blanco y negro en los cargadores).
    #[serde(default)]
    pub accent: u32,
    /// FORMA del cuaderno (0=Actual, 1=Tapa dura, 2=Moleskine, 3=Espiral, 4=Minimalista).
    #[serde(default)]
    pub shape: u32,
    /// Multiplicador de grosor (1.0 = el de la forma).
    #[serde(default = "default_one")]
    pub thickness: f32,
    /// Multiplicador de la ceja de tapa (1.0 = la de la forma).
    #[serde(default = "default_one")]
    pub overhang: f32,
    /// Textura del material (0=ninguna, 1=cuero, 2=tela, 3=madera, 4=kraft, 5=carbono, 6=cuadros).
    /// Se aplica sobre la portada (foils) y sobre las figuras 3D.
    #[serde(default)]
    pub texture: u32,
    /// Color de TODO el cuaderno cuando el diseño es color solido (finish 800) o degradado (801):
    /// `cover_a` = color principal (solido / inicio del degradado), `cover_b` = fin del degradado.
    #[serde(default = "default_white3")]
    pub cover_a: [f32; 3],
    #[serde(default = "default_white3")]
    pub cover_b: [f32; 3],
    /// Archivero (carpeta) al que pertenece; "" = sin archivero (aparece solo en "Todos").
    #[serde(default)]
    pub archivero: String,
    /// Diseño de pagina del modo escritura (margenes, interlineado, fuente...). Todo el cuaderno.
    #[serde(default = "default_doc_layout")]
    pub doc_layout: DocLayout,
    /// Las paginas del cuaderno.
    #[serde(default)]
    pub pages: Vec<PageData>,

    // --- Compatibilidad con el formato anterior (cuaderno de un solo lienzo) ---
    #[serde(default, skip_serializing)]
    doc: Option<ink_core::Document>,
    #[serde(default, skip_serializing)]
    texts: Vec<ink_core::TextItem>,
    #[serde(default, skip_serializing)]
    erase_strokes: Vec<Vec<[f32; 4]>>,
    #[serde(default, skip_serializing)]
    tick: Option<f32>,
}

impl NotebookData {
    /// Cuaderno nuevo con una pagina vacia.
    pub fn new(name: &str, infinite: bool, finish: u32) -> Self {
        Self {
            version: 2,
            name: name.to_string(),
            infinite,
            finish,
            fx: 0,
            fx_intensity: 1.0,
            accent: 0,
            shape: 0,
            thickness: 1.0,
            overhang: 1.0,
            texture: 0,
            cover_a: [1.0, 1.0, 1.0],
            cover_b: [1.0, 1.0, 1.0],
            archivero: String::new(),
            doc_layout: DocLayout::default(),
            pages: vec![PageData::empty()],
            doc: None,
            texts: Vec::new(),
            erase_strokes: Vec::new(),
            tick: None,
        }
    }

    /// Garantiza que haya al menos una pagina, migrando el formato anterior si hace falta.
    fn normalize(&mut self) {
        if self.pages.is_empty() {
            if let Some(doc) = self.doc.take() {
                self.pages.push(PageData {
                    doc,
                    texts: std::mem::take(&mut self.texts),
                    erase_strokes: std::mem::take(&mut self.erase_strokes),
                    tick: self.tick.unwrap_or(1.0),
                    body: String::new(),
                    align: 0,
                });
            } else {
                self.pages.push(PageData::empty());
            }
        }
    }
}

/// Una entrada de la biblioteca (para listar sin cargar todo el dibujo).
pub struct NotebookEntry {
    pub name: String,
    pub infinite: bool,
    pub finish: u32,
    pub fx: u32,
    pub fx_intensity: f32,
    pub accent: u32,
    pub shape: u32,
    pub thickness: f32,
    pub overhang: f32,
    pub texture: u32,
    pub cover_a: [f32; 3],
    pub cover_b: [f32; 3],
    pub archivero: String,
    pub path: PathBuf,
}

/// Ajustes GLOBALES de la biblioteca (panel "Tweaks"): pose en el estante e interaccion.
#[derive(Clone, Serialize, Deserialize)]
pub struct LibTweaks {
    /// Giro (lomo) en grados (cuanto gira para ver el lomo).
    #[serde(default = "tw_giro")]
    pub giro: f32,
    /// Inclinacion en grados (cuanto se mira desde arriba).
    #[serde(default = "tw_incl")]
    pub inclinacion: f32,
    /// Modo de hover: 0=levantar, 1=abrir, 2=girar, 3=sutil.
    #[serde(default)]
    pub hover: u32,
    /// Animar las portadas (si no, quedan estaticas).
    #[serde(default = "tw_true")]
    pub animate: bool,
    /// Tema de la interfaz (chrome): 0 = Tinta (galeria oscura), 1 = Cuaderno (papel rayado).
    #[serde(default)]
    pub theme: u32,
}
fn tw_giro() -> f32 { 18.0 }
fn tw_incl() -> f32 { 9.0 }
fn tw_true() -> bool { true }
impl Default for LibTweaks {
    fn default() -> Self {
        Self { giro: 18.0, inclinacion: 9.0, hover: 0, animate: true, theme: 0 }
    }
}

fn tweaks_path() -> PathBuf {
    notebooks_dir().join("_tweaks.json")
}
pub fn load_tweaks() -> LibTweaks {
    std::fs::read_to_string(tweaks_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
pub fn save_tweaks(t: &LibTweaks) {
    let _ = ensure_dir();
    if let Ok(s) = serde_json::to_string(t) {
        let _ = std::fs::write(tweaks_path(), s);
    }
}

/// Carpeta donde viven los cuadernos: `%USERPROFILE%/Documents/Cuadernos Ink`.
pub fn notebooks_dir() -> PathBuf {
    let base = std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("Documents").join("Cuadernos Ink")
}

/// Crea la carpeta de cuadernos si no existe y la devuelve.
pub fn ensure_dir() -> PathBuf {
    let d = notebooks_dir();
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Ruta de archivo para un cuaderno con ese nombre (sanitizando caracteres invalidos).
pub fn path_for(name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let safe = safe.trim();
    let safe = if safe.is_empty() { "cuaderno" } else { safe };
    ensure_dir().join(format!("{safe}.json"))
}

/// Archivo con el ORDEN manual de la biblioteca (lista de nombres de archivo).
fn order_path() -> PathBuf {
    notebooks_dir().join("_order.json")
}

/// Lee el orden manual guardado (nombres de archivo, p.ej. "cscs.json"); vacio si no hay.
pub fn load_order() -> Vec<String> {
    std::fs::read_to_string(order_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Guarda el orden manual de la biblioteca (lista de nombres de archivo).
pub fn save_order(files: &[String]) {
    let _ = ensure_dir();
    if let Ok(s) = serde_json::to_string(files) {
        let _ = std::fs::write(order_path(), s);
    }
}

/// Lista de ARCHIVEROS (carpetas) definidos por el usuario; persiste aunque esten vacios.
fn archiveros_path() -> PathBuf {
    notebooks_dir().join("_archiveros.json")
}
pub fn load_archiveros() -> Vec<String> {
    std::fs::read_to_string(archiveros_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
pub fn save_archiveros(list: &[String]) {
    let _ = ensure_dir();
    if let Ok(s) = serde_json::to_string(list) {
        let _ = std::fs::write(archiveros_path(), s);
    }
}

/// Lista los cuadernos guardados, en el ORDEN manual (`_order.json`); los que no esten en el
/// orden (cuadernos nuevos) van al final, alfabeticamente.
pub fn list() -> Vec<NotebookEntry> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(notebooks_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            // Saltar archivos internos (orden / tweaks), no son cuadernos.
            if p.file_name().map_or(false, |n| n == "_order.json" || n == "_tweaks.json" || n == "_archiveros.json") {
                continue;
            }
            if p.extension().map_or(false, |x| x.eq_ignore_ascii_case("json")) {
                if let Some(nb) = load(&p) {
                    out.push(NotebookEntry {
                        name: nb.name,
                        infinite: nb.infinite,
                        finish: nb.finish,
                        fx: nb.fx,
                        fx_intensity: nb.fx_intensity,
                        accent: nb.accent,
                        shape: nb.shape,
                        thickness: nb.thickness,
                        overhang: nb.overhang,
                        texture: nb.texture,
                        cover_a: nb.cover_a,
                        cover_b: nb.cover_b,
                        archivero: nb.archivero,
                        path: p,
                    });
                }
            }
        }
    }
    let order = load_order();
    let pos = |e: &NotebookEntry| -> Option<usize> {
        let fname = e.path.file_name().and_then(|s| s.to_str())?;
        order.iter().position(|o| o == fname)
    };
    out.sort_by(|a, b| match (pos(a), pos(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    out
}

/// Guarda un cuaderno en `path`.
pub fn save(nb: &NotebookData, path: &Path) -> std::io::Result<()> {
    let s = serde_json::to_string(nb)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, s)
}

/// Carga un cuaderno desde `path` (None si falla). Migra el formato anterior.
pub fn load(path: &Path) -> Option<NotebookData> {
    let s = std::fs::read_to_string(path).ok()?;
    let mut nb: NotebookData = serde_json::from_str(&s).ok()?;
    nb.normalize();
    Some(nb)
}

/// Borra el archivo del cuaderno.
pub fn delete(path: &Path) {
    let _ = std::fs::remove_file(path);
}
