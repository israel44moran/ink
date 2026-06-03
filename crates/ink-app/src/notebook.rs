//! Cuadernos de notas: persistencia en disco del dibujo (trazos, texto, borrados) y la
//! biblioteca (listar / crear / abrir / borrar). Cada cuaderno es un archivo JSON en
//! `Documentos/Cuadernos Ink/`. Las herramientas y pinceles son los mismos que en el
//! lienzo infinito; aqui solo se guarda/restaura el contenido.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

fn default_tick() -> f32 {
    1.0
}

/// Contenido serializable de un cuaderno (lo que se guarda en disco).
#[derive(Serialize, Deserialize)]
pub struct NotebookData {
    #[serde(default)]
    pub version: u32,
    pub name: String,
    /// `true` = lienzo infinito; `false` = con hojas (mesa de trabajo finita).
    pub infinite: bool,
    /// Dibujo (trazos vectoriales por capa).
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

impl NotebookData {
    /// Cuaderno nuevo y vacio.
    pub fn new(name: &str, infinite: bool) -> Self {
        Self {
            version: 1,
            name: name.to_string(),
            infinite,
            doc: ink_core::Document::new(),
            texts: Vec::new(),
            erase_strokes: Vec::new(),
            tick: 1.0,
        }
    }
}

/// Una entrada de la biblioteca (para listar sin cargar todo el dibujo).
pub struct NotebookEntry {
    pub name: String,
    pub infinite: bool,
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
                    out.push(NotebookEntry { name: nb.name, infinite: nb.infinite, path: p });
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

/// Carga un cuaderno desde `path` (None si falla).
pub fn load(path: &Path) -> Option<NotebookData> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

/// Borra el archivo del cuaderno.
pub fn delete(path: &Path) {
    let _ = std::fs::remove_file(path);
}
