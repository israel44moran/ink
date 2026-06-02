//! # ink-brush
//!
//! Parser de pinceles de Adobe Photoshop (`.abr`) y del estado de paleta (`.psp`
//! "Brushes"), formato moderno "8BIM" (versiones 6/9/10, subversiones 1 y 2).
//!
//! Extrae cada pincel MUESTREADO (sampled brush) como una mascara alfa en escala de
//! grises (la "punta" del pincel). La capa de aplicacion la sube como textura y la
//! estampa a lo largo del trazo (motor de stamps), igual que Photoshop.
//!
//! Es 100% portable (sin GPU): solo decodifica bytes -> [`SampledBrush`].
//!
//! Especificacion del formato verificada contra las implementaciones de GIMP
//! (`gimpbrush-load.c`), Krita (`kis_abr_brush_collection.cpp`) y la gramatica
//! Kaitai `ABR.ksy` de brush-viewer.

/// Una punta de pincel muestreada: mascara alfa de `width`x`height` (8 bits/pixel,
/// 255 = cobertura total). Es lo que Photoshop estampa a lo largo del trazo.
#[derive(Clone)]
pub struct SampledBrush {
    /// UUID interno del pincel (enlaza con su nombre en el bloque `desc`).
    pub id: String,
    /// Nombre legible, si se pudo leer del bloque `desc` (si no, `None`).
    pub name: Option<String>,
    pub width: u32,
    pub height: u32,
    /// Mascara alfa, `width * height` bytes en orden de filas (row-major).
    pub alpha: Vec<u8>,
}

impl std::fmt::Debug for SampledBrush {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SampledBrush")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("alpha_len", &self.alpha.len())
            .finish()
    }
}

#[derive(Debug)]
pub enum AbrError {
    Truncated,
    UnsupportedVersion(u16, u16),
    NoSampleSection,
}

impl std::fmt::Display for AbrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AbrError::Truncated => write!(f, "archivo .abr truncado o corrupto"),
            AbrError::UnsupportedVersion(v, s) => write!(f, "version .abr no soportada: {v}.{s}"),
            AbrError::NoSampleSection => write!(f, "no se encontro el bloque 'samp' de pinceles"),
        }
    }
}
impl std::error::Error for AbrError {}

/// Tamano maximo de punta aceptado (igual que GIMP), como guarda anti-corrupcion.
const MAX_DIM: i64 = 16384;

/// Lector big-endian con posicion y cortes seguros.
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], AbrError> {
        if self.pos + n > self.b.len() {
            return Err(AbrError::Truncated);
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, AbrError> {
        Ok(self.take(1)?[0])
    }
    fn i8(&mut self) -> Result<i8, AbrError> {
        Ok(self.u8()? as i8)
    }
    fn u16(&mut self) -> Result<u16, AbrError> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn i16(&mut self) -> Result<i16, AbrError> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, AbrError> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self) -> Result<i32, AbrError> {
        Ok(self.u32()? as i32)
    }
    fn skip(&mut self, n: usize) -> Result<(), AbrError> {
        if self.pos + n > self.b.len() {
            return Err(AbrError::Truncated);
        }
        self.pos += n;
        Ok(())
    }
}

/// Parsea un archivo `.abr` (o el `Brushes.psp` de la carpeta Settings, que usa el
/// mismo formato 8BIM) y devuelve todas las puntas muestreadas que pueda leer.
///
/// Es tolerante a fallos: si una punta concreta esta corrupta, la salta y sigue con
/// las demas en vez de abortar todo el archivo.
pub fn parse_abr(bytes: &[u8]) -> Result<Vec<SampledBrush>, AbrError> {
    let mut r = Reader::new(bytes);
    let version = r.u16()?;
    let subversion = r.u16()?;
    // Solo el formato moderno (6..=10). El antiguo (1/2) casi no se usa ya.
    if !(6..=10).contains(&version) || !(1..=2).contains(&subversion) {
        return Err(AbrError::UnsupportedVersion(version, subversion));
    }

    if !reach_8bim(&mut r, b"samp")? {
        return Err(AbrError::NoSampleSection);
    }
    let samp_size = r.u32()? as usize;
    let samp_end = (r.pos + samp_size).min(bytes.len());

    let mut out = Vec::new();
    while r.pos + 4 <= samp_end {
        match read_sample(&mut r, subversion, samp_end) {
            Ok(Some(b)) => out.push(b),
            Ok(None) => {}
            Err(_) => break, // muestra corrupta: detenemos este bloque con lo que haya
        }
    }
    Ok(out)
}

/// Avanza el lector hasta justo despues de la clave `8BIM<key>` buscada.
/// Devuelve `false` si llega a EOF sin encontrarla.
fn reach_8bim(r: &mut Reader, key: &[u8; 4]) -> Result<bool, AbrError> {
    loop {
        if r.remaining() < 8 {
            return Ok(false);
        }
        let tag = r.take(4)?;
        if tag != b"8BIM" {
            return Ok(false); // desincronizado: no hay mas secciones validas
        }
        let k = r.take(4)?;
        if k == key {
            return Ok(true);
        }
        // Saltar el cuerpo de la seccion (longitud BE), con padding a multiplo de 4.
        let len = r.u32()? as usize;
        let padded = (len + 3) & !3;
        if r.skip(padded).is_err() {
            return Ok(false);
        }
    }
}

/// Lee UNA muestra del bloque `samp`. Devuelve `Ok(None)` si la punta es valida pero
/// vacia/ignorable; salta siempre al inicio de la siguiente muestra (padding a 4).
fn read_sample(r: &mut Reader, subversion: u16, samp_end: usize) -> Result<Option<SampledBrush>, AbrError> {
    let sample_len = r.u32()? as usize;
    let body_start = r.pos;
    // Cada muestra esta rellenada a multiplo de 4 bytes.
    let next = (body_start + ((sample_len + 3) & !3)).min(samp_end);

    let result = read_sample_body(r, subversion);

    // Pase lo que pase, reposicionar al inicio de la siguiente muestra.
    r.pos = next;
    result
}

fn read_sample_body(r: &mut Reader, subversion: u16) -> Result<Option<SampledBrush>, AbrError> {
    let id_len = r.u8()? as usize;
    let id_bytes = r.take(id_len)?;
    let id = String::from_utf8_lossy(id_bytes).into_owned();

    if subversion == 1 {
        // v6.1: 10 bytes desconocidos, luego image_data directo.
        r.skip(10)?;
    } else {
        // v6.2: cabecera de descriptor + canales; avanzamos al primer canal con datos.
        r.skip(2 + 2)?; // meta_len u16 + meta_a u16
        r.skip(4 + 4)?; // version u32 + length u32
        r.skip(16)?; // rectangulo de bounds externo
        let num_channels = r.u32()?;
        let mut found = false;
        for _ in 0..num_channels.min(64) {
            let is_written = r.u32()?;
            if is_written > 0 {
                let clen = r.u32()?;
                if clen > 0 {
                    r.skip(4)?; // unused_depth u32 -> el cursor queda en image_data
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return Ok(None);
        }
    }

    // --- image_data ---
    let top = r.i32()? as i64;
    let left = r.i32()? as i64;
    let bottom = r.i32()? as i64;
    let right = r.i32()? as i64;
    let depth = r.i16()?;
    let compression = r.u8()?;

    let depth_bytes = (depth >> 3) as i64;
    let width = right - left;
    let height = bottom - top;

    // Validacion anti-corrupcion (reglas de GIMP).
    if width < 1 || height < 1 || width > MAX_DIM || height > MAX_DIM {
        return Ok(None);
    }
    if compression > 1 {
        return Ok(None);
    }
    if compression == 1 && depth_bytes != 1 {
        return Ok(None);
    }
    if compression == 0 && depth_bytes != 1 && depth_bytes != 2 {
        return Ok(None);
    }

    let w = width as u32;
    let h = height as u32;
    let count = (width * height) as usize;
    let mut alpha = vec![0u8; count];

    if compression == 0 {
        if depth_bytes == 1 {
            let data = r.take(count)?;
            alpha.copy_from_slice(data);
        } else {
            // 16 bits LE -> 8 bits.
            let data = r.take(count * 2)?;
            for (i, px) in alpha.iter_mut().enumerate() {
                let lo = data[i * 2] as u16;
                let hi = data[i * 2 + 1] as u16;
                let v = u16::from_le_bytes([lo as u8, hi as u8]);
                *px = (v >> 8) as u8;
            }
        }
    } else {
        rle_decode(r, &mut alpha, w as usize, h as usize)?;
    }

    Ok(Some(SampledBrush { id, name: None, width: w, height: h, alpha }))
}

/// Descompresion PackBits por scanline (RLE de Photoshop). Primero una tabla de
/// `height` longitudes (u16 BE) y luego los datos comprimidos de cada fila.
fn rle_decode(r: &mut Reader, out: &mut [u8], width: usize, height: usize) -> Result<(), AbrError> {
    let mut row_len = Vec::with_capacity(height);
    for _ in 0..height {
        row_len.push(r.u16()? as usize);
    }
    for (row, &rl) in row_len.iter().enumerate() {
        let row_end_in = r.pos + rl;
        if row_end_in > r.b.len() {
            return Err(AbrError::Truncated);
        }
        // Cada fila escribe en su propio tramo de la salida: out[row*width ..].
        let mut o = row * width;
        let row_target = (o + width).min(out.len());
        while r.pos < row_end_in && o < row_target {
            let n = r.i8()?;
            if n >= 0 {
                // Tramo literal: copiar n+1 bytes.
                let cnt = (n as usize) + 1;
                let writable = cnt.min(row_target - o);
                let data = r.take(writable)?;
                out[o..o + writable].copy_from_slice(data);
                o += writable;
                if cnt > writable {
                    r.skip(cnt - writable)?; // sobrante que no cabe en la fila
                }
            } else if n != -128 {
                // Tramo repetido: 1-n copias del siguiente byte.
                let cnt = (1 - n as i32) as usize;
                let b = r.u8()?;
                let writable = cnt.min(row_target - o);
                out[o..o + writable].iter_mut().for_each(|px| *px = b);
                o += writable;
            }
            // n == -128: no-op
        }
        // Alinear el cursor de entrada al final declarado de la fila.
        r.pos = row_end_in;
    }
    Ok(())
}

// ===================================================================
// Parser del bloque 'desc' (Action Descriptor de Photoshop): nombres,
// grupos y el UUID de la punta de cada PRESET de pincel.
// ===================================================================

/// Un preset de pincel del bloque 'desc': nombre legible + UUID de la punta + grupo.
#[derive(Clone, Debug)]
pub struct BrushPreset {
    pub name: String,
    pub uuid: String,
    pub group: String,
}

/// Valor de un Action Descriptor (subconjunto suficiente para pinceles).
enum OsValue {
    Descriptor(OsDesc),
    List(Vec<OsValue>),
    Text(String),
    Other,
}

struct OsDesc {
    items: Vec<(String, OsValue)>,
}
impl OsDesc {
    fn get(&self, key: &str) -> Option<&OsValue> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    fn text(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(OsValue::Text(s)) => Some(s.clone()),
            _ => None,
        }
    }
}

/// String Unicode de PS: u32 (numero de chars) + chars UTF-16BE (terminado en \0).
fn read_unicode(r: &mut Reader) -> Result<String, AbrError> {
    let n = r.u32()? as usize;
    if n > 200_000 {
        return Err(AbrError::Truncated);
    }
    let mut s = String::with_capacity(n);
    for _ in 0..n {
        let c = r.u16()?;
        if c != 0 {
            s.push(char::from_u32(c as u32).unwrap_or('\u{FFFD}'));
        }
    }
    Ok(s)
}

/// String "compacto" de PS: u32 len; si len==0 se leen 4 bytes (clave de 4 chars).
fn read_compact(r: &mut Reader) -> Result<String, AbrError> {
    let n = r.u32()? as usize;
    let len = if n == 0 { 4 } else { n };
    let b = r.take(len)?;
    Ok(String::from_utf8_lossy(b).trim_end_matches('\0').to_string())
}

fn read_descriptor(r: &mut Reader, depth: u32) -> Result<OsDesc, AbrError> {
    let _class_name = read_unicode(r)?;
    let _class_id = read_compact(r)?;
    let count = r.u32()? as usize;
    if count > 100_000 {
        return Err(AbrError::Truncated);
    }
    let mut items = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let key = read_compact(r)?;
        let val = read_value(r, depth)?;
        items.push((key, val));
    }
    Ok(OsDesc { items })
}

fn read_value(r: &mut Reader, depth: u32) -> Result<OsValue, AbrError> {
    if depth > 40 {
        return Err(AbrError::Truncated);
    }
    let t = r.take(4)?.to_vec();
    let v = match &t[..] {
        b"Objc" | b"GlbO" => OsValue::Descriptor(read_descriptor(r, depth + 1)?),
        b"VlLs" => {
            let n = r.u32()? as usize;
            if n > 200_000 {
                return Err(AbrError::Truncated);
            }
            let mut list = Vec::with_capacity(n.min(512));
            for _ in 0..n {
                list.push(read_value(r, depth + 1)?);
            }
            OsValue::List(list)
        }
        b"TEXT" => OsValue::Text(read_unicode(r)?),
        b"tdta" | b"alis" => {
            let n = r.u32()? as usize;
            r.skip(n)?;
            OsValue::Other
        }
        b"long" => {
            r.skip(4)?;
            OsValue::Other
        }
        b"doub" | b"comp" => {
            r.skip(8)?;
            OsValue::Other
        }
        b"bool" => {
            r.skip(1)?;
            OsValue::Other
        }
        b"UntF" => {
            r.skip(12)?; // unidad (4) + f64 (8)
            OsValue::Other
        }
        b"enum" => {
            let _ = read_compact(r)?;
            let _ = read_compact(r)?;
            OsValue::Other
        }
        b"type" | b"GlbC" => {
            let _ = read_unicode(r)?;
            let _ = read_compact(r)?;
            OsValue::Other
        }
        // Referencias y otros tipos no soportados: abortar esta rama de forma segura.
        _ => return Err(AbrError::Truncated),
    };
    Ok(v)
}

/// Busca recursivamente un valor TEXT bajo la clave `key`.
fn find_text(d: &OsDesc, key: &str, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    for (k, v) in &d.items {
        if k == key {
            if let OsValue::Text(s) = v {
                return Some(s.clone());
            }
        }
        if let OsValue::Descriptor(sub) = v {
            if let Some(s) = find_text(sub, key, depth + 1) {
                return Some(s);
            }
        }
    }
    None
}

fn walk_presets(list: &[OsValue], group: &str, out: &mut Vec<BrushPreset>) {
    for v in list {
        if let OsValue::Descriptor(d) = v {
            // Grupo si contiene una sub-LISTA "Brsh"; preset si su "Brsh" es un Objc.
            if let Some(OsValue::List(sub)) = d.get("Brsh") {
                let g = d.text("Nm  ").unwrap_or_else(|| group.to_string());
                walk_presets(sub, &g, out);
            } else {
                // El nombre suele venir como "$$$/Presets/Brushes/Clave=Nombre legible".
                let raw = d.text("Nm  ").unwrap_or_default();
                let clean = raw.rsplit('=').next().unwrap_or("").trim();
                let name = if clean.is_empty() { raw.clone() } else { clean.to_string() };
                if let Some(uuid) = find_text(d, "sampledData", 0) {
                    out.push(BrushPreset { name, uuid, group: group.to_string() });
                }
            }
        }
    }
}

fn find_sig(bytes: &[u8], sig: &[u8]) -> Option<usize> {
    bytes.windows(sig.len()).position(|w| w == sig)
}

/// Parsea los PRESETS de pincel (nombre + grupo + UUID de la punta) del bloque 'desc'.
/// Localiza la firma `8BIMdesc` directamente (robusto ante el relleno de secciones).
pub fn parse_presets(bytes: &[u8]) -> Vec<BrushPreset> {
    let Some(off) = find_sig(bytes, b"8BIMdesc") else {
        return Vec::new();
    };
    let mut r = Reader::new(bytes);
    r.pos = off + 8; // tras "8BIMdesc"
    if r.u32().is_err() || r.u32().is_err() {
        return Vec::new(); // length + version
    }
    let root = match read_descriptor(&mut r, 0) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Some(OsValue::List(list)) = root.get("Brsh") {
        walk_presets(list, "", &mut out);
    }
    out
}
