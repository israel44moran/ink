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
}

impl PageData {
    pub fn empty() -> Self {
        Self {
            doc: ink_core::Document::new(),
            texts: Vec::new(),
            erase_strokes: Vec::new(),
            tick: 1.0,
        }
    }
}

/// Contenido serializable de un cuaderno (lo que se guarda en disco).
#[derive(Serialize, Deserialize)]
pub struct NotebookData {
    #[serde(default)]
    pub version: u32,
    pub name: String,
    /// `true` = lienzo infinito (1 pagina); `false` = con hojas (varias paginas A4).
    pub infinite: bool,
    /// Acabado/diseño de la "carta" en la biblioteca (0=mate, 1=holo, 2=galaxia, 3=oro,
    /// 4=prisma, 5=destellos).
    #[serde(default = "default_finish")]
    pub finish: u32,
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
    pub path: PathBuf,
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

/// Lista los cuadernos guardados (ordenados por nombre).
pub fn list() -> Vec<NotebookEntry> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(notebooks_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map_or(false, |x| x.eq_ignore_ascii_case("json")) {
                if let Some(nb) = load(&p) {
                    out.push(NotebookEntry { name: nb.name, infinite: nb.infinite, finish: nb.finish, path: p });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
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
