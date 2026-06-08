//! Exportar cuadernos y notas a PDF.
//!
//! Genera el PDF "a mano" (sin dependencias externas) para no tocar el motor de render de baja
//! latencia. Es VECTORIAL: el texto del documento (Markdown simplificado) se escribe como texto
//! nativo del PDF (fuente Helvetica integrada, seleccionable) y los trazos de tinta se dibujan como
//! lineas vectoriales. Una hoja del cuaderno = una pagina del PDF.
//!
//! El flujo de contenido de un PDF son BYTES crudos (las cadenas usan WinAnsiEncoding, que NO es
//! UTF-8); por eso se construye en `Vec<u8>` y nunca se pasa por `String`/UTF-8.

use crate::notebook::{NotebookData, PageData};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

// ----------------------------------------------------------------------------
// Constructor minimo de PDF (objetos + tabla xref)
// ----------------------------------------------------------------------------

struct Pdf {
    objects: Vec<Vec<u8>>, // el objeto con id N esta en objects[N-1]
}

impl Pdf {
    fn new() -> Self {
        Pdf { objects: Vec::new() }
    }
    /// Reserva un id de objeto (cuerpo vacio que se rellena luego con `set`).
    fn alloc(&mut self) -> usize {
        self.objects.push(Vec::new());
        self.objects.len()
    }
    fn set(&mut self, id: usize, body: Vec<u8>) {
        self.objects[id - 1] = body;
    }
    /// Anade un objeto ya formado y devuelve su id.
    fn add(&mut self, body: Vec<u8>) -> usize {
        self.objects.push(body);
        self.objects.len()
    }
    /// Serializa el PDF completo con la tabla xref y el trailer.
    fn finish(self, root: usize) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
        let mut offsets = vec![0usize; self.objects.len()];
        for (i, obj) in self.objects.iter().enumerate() {
            offsets[i] = out.len();
            let _ = write!(out, "{} 0 obj\n", i + 1);
            out.extend_from_slice(obj);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_off = out.len();
        let n = self.objects.len() + 1;
        let _ = write!(out, "xref\n0 {}\n", n);
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            let _ = write!(out, "{:010} 00000 n \n", off);
        }
        let _ = write!(out, "trailer\n<< /Size {} /Root {} 0 R >>\nstartxref\n{}\n%%EOF\n", n, root, xref_off);
        out
    }
}

// ----------------------------------------------------------------------------
// Codificacion de texto (WinAnsi) y escape para cadenas literales del PDF
// ----------------------------------------------------------------------------

/// Convierte un caracter a su byte en WinAnsiEncoding (lo que entiende Helvetica integrada).
/// Latin-1 (acentos del espanol) coincide; algunos signos tipograficos se remapean; el resto -> '?'.
fn winansi(c: char) -> u8 {
    let u = c as u32;
    match u {
        0x20..=0x7E => u as u8,            // ASCII imprimible
        0xA0..=0xFF => u as u8,            // Latin-1 (a, e, i, o, u con tilde, n, signos ? !, dieresis)
        0x2018 => 0x91,                    // comilla simple izquierda
        0x2019 => 0x92,                    // comilla simple derecha / apostrofo
        0x201C => 0x93,                    // comilla doble izquierda
        0x201D => 0x94,                    // comilla doble derecha
        0x2022 => 0x95,                    // bullet
        0x2013 => 0x96,                    // guion corto (en dash)
        0x2014 => 0x97,                    // guion largo (em dash)
        0x2026 => 0x85,                    // puntos suspensivos
        0x09 => 0x20,                      // tab -> espacio
        0xFEFF => 0x20,                    // BOM -> espacio
        _ => b'?',
    }
}

/// Escribe una cadena como literal PDF `(...)` con WinAnsi y escapes de ( ) \\.
fn push_pdf_string(buf: &mut Vec<u8>, s: &str) {
    buf.push(b'(');
    for c in s.chars() {
        let b = winansi(c);
        if b == b'(' || b == b')' || b == b'\\' {
            buf.push(b'\\');
        }
        buf.push(b);
    }
    buf.push(b')');
}

/// Emite un renglon de texto: `BT /Fn size Tf r g b rg 1 0 0 1 x y Tm (texto) Tj ET`.
fn push_text_line(buf: &mut Vec<u8>, font: &str, size: f32, rgb: [f32; 3], x: f32, y: f32, text: &str) {
    let _ = write!(
        buf,
        "BT {} {:.2} Tf {:.3} {:.3} {:.3} rg 1 0 0 1 {:.2} {:.2} Tm ",
        font, size, rgb[0], rgb[1], rgb[2], x, y
    );
    push_pdf_string(buf, text);
    buf.extend_from_slice(b" Tj ET\n");
}

// ----------------------------------------------------------------------------
// Conversion de color lineal -> sRGB (la tinta se guarda en espacio lineal)
// ----------------------------------------------------------------------------

fn lin_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb3(c: [f32; 4]) -> [f32; 3] {
    [lin_to_srgb(c[0]), lin_to_srgb(c[1]), lin_to_srgb(c[2])]
}

// ----------------------------------------------------------------------------
// Ancho aproximado de texto en Helvetica (para el ajuste de linea)
// ----------------------------------------------------------------------------

fn char_em(c: char) -> f32 {
    match c {
        ' ' | 'i' | 'j' | 'l' | '.' | ',' | '\'' | '|' | '!' | ':' | ';' | '`' => 0.30,
        'f' | 't' | 'r' | 'I' | '(' | ')' | '[' | ']' | '/' | '-' => 0.36,
        'm' | 'M' | 'w' | 'W' | '@' => 0.85,
        'A'..='Z' => 0.70,
        _ => 0.52,
    }
}

fn text_width_pt(s: &str, size: f32) -> f32 {
    s.chars().map(char_em).sum::<f32>() * size
}

/// Parte una linea larga en varias que quepan en `max_w` puntos (corta por palabras).
fn wrap_line(text: &str, size: f32, max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if text_width_pt(&cand, size) <= max_w || cur.is_empty() {
            cur = cand;
        } else {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

// ----------------------------------------------------------------------------
// Markdown simplificado -> renglones con estilo
// ----------------------------------------------------------------------------

struct Renderable {
    text: String,
    size: f32,
    bold: bool,
    indent: f32,
    gap_before: f32,
}

/// Quita marcas en linea (**negrita**, *cursiva*, `codigo`, [texto](url)) dejando el texto.
fn strip_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' | '`' | '~' => {}
            '[' => {
                let mut txt = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    txt.push(d);
                }
                if chars.peek() == Some(&'(') {
                    chars.next();
                    for d in chars.by_ref() {
                        if d == ')' {
                            break;
                        }
                    }
                }
                out.push_str(&txt);
            }
            _ => out.push(c),
        }
    }
    out
}

fn bullet(s: &str) -> Option<String> {
    for p in ["- ", "* ", "+ "] {
        if let Some(r) = s.strip_prefix(p) {
            return Some(r.to_string());
        }
    }
    None
}

fn checkbox(s: &str) -> Option<String> {
    for p in ["- [ ] ", "* [ ] "] {
        if let Some(r) = s.strip_prefix(p) {
            return Some(format!("\u{2610} {}", strip_inline(r)));
        }
    }
    for p in ["- [x] ", "- [X] ", "* [x] "] {
        if let Some(r) = s.strip_prefix(p) {
            return Some(format!("[x] {}", strip_inline(r)));
        }
    }
    None
}

fn numbered(s: &str) -> Option<String> {
    let mut seen_digit = false;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            seen_digit = true;
        } else if c == '.' && seen_digit {
            if let Some(r) = s[i + 1..].strip_prefix(' ') {
                return Some(format!("{}. {}", &s[..i], r));
            }
            return None;
        } else {
            return None;
        }
    }
    None
}

fn parse_body(body: &str, base_size: f32) -> Vec<Renderable> {
    let mut items = Vec::new();
    for raw in body.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            items.push(Renderable { text: String::new(), size: base_size, bold: false, indent: 0.0, gap_before: base_size * 0.5 });
        } else if let Some(rest) = trimmed.strip_prefix("### ") {
            items.push(Renderable { text: strip_inline(rest), size: base_size * 1.15, bold: true, indent: 0.0, gap_before: base_size * 0.6 });
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            items.push(Renderable { text: strip_inline(rest), size: base_size * 1.35, bold: true, indent: 0.0, gap_before: base_size * 0.8 });
        } else if let Some(rest) = trimmed.strip_prefix("# ") {
            items.push(Renderable { text: strip_inline(rest), size: base_size * 1.7, bold: true, indent: 0.0, gap_before: base_size * 1.0 });
        } else if let Some(rest) = trimmed.strip_prefix("> ") {
            items.push(Renderable { text: strip_inline(rest), size: base_size, bold: false, indent: base_size * 1.2, gap_before: base_size * 0.2 });
        } else if let Some(rest) = checkbox(trimmed) {
            items.push(Renderable { text: rest, size: base_size, bold: false, indent: base_size * 1.0, gap_before: 0.0 });
        } else if let Some(rest) = bullet(trimmed) {
            items.push(Renderable { text: format!("\u{2022} {}", strip_inline(&rest)), size: base_size, bold: false, indent: base_size * 1.0, gap_before: 0.0 });
        } else if let Some(rest) = numbered(trimmed) {
            items.push(Renderable { text: strip_inline(&rest), size: base_size, bold: false, indent: base_size * 1.0, gap_before: 0.0 });
        } else {
            items.push(Renderable { text: strip_inline(trimmed), size: base_size, bold: false, indent: 0.0, gap_before: 0.0 });
        }
    }
    items
}

// ----------------------------------------------------------------------------
// Construccion del flujo de contenido de una pagina
// ----------------------------------------------------------------------------

/// Mapea un punto de mundo (pagina centrada en el origen, Y hacia abajo) a coordenadas PDF
/// (origen abajo-izquierda, Y hacia arriba), en puntos.
fn world_to_pdf(x: f32, y: f32, w_pt: f32, h_pt: f32) -> (f32, f32) {
    (x + w_pt * 0.5, h_pt * 0.5 - y)
}

/// Texto del cuerpo (Markdown) en una hoja A4 con sus margenes.
fn body_content(buf: &mut Vec<u8>, body: &str, w_pt: f32, h_pt: f32, margins: [f32; 4], base_size: f32) {
    let left = margins[3];
    let top = margins[0];
    let right = margins[1];
    let bottom = margins[2];
    let max_w = (w_pt - left - right).max(20.0);
    let mut y = h_pt - top;
    for item in parse_body(body, base_size) {
        y -= item.gap_before;
        let leading = item.size * 1.35;
        let wrapped = if item.text.is_empty() {
            vec![String::new()]
        } else {
            wrap_line(&item.text, item.size, max_w - item.indent)
        };
        for ln in wrapped {
            if y < bottom {
                break; // esta version no pagina el texto: lo que no cabe se omite
            }
            if !ln.trim().is_empty() {
                let font = if item.bold { "/F2" } else { "/F1" };
                push_text_line(buf, font, item.size, [0.0, 0.0, 0.0], left + item.indent, y, &ln);
            }
            y -= leading;
        }
    }
}

/// Trazos de tinta de una pagina como lineas vectoriales. `off` traslada coordenadas (notas
/// infinitas); para hojas A4 es (0,0) y se usa el mapeo centrado.
fn ink_content(buf: &mut Vec<u8>, doc: &ink_core::Document, w_pt: f32, h_pt: f32, off: Option<(f32, f32)>) {
    buf.extend_from_slice(b"1 J 1 j\n"); // extremos y uniones redondeados
    for layer in &doc.layers {
        if !layer.visible || layer.opacity <= 0.001 {
            continue;
        }
        for stroke in &layer.strokes {
            if stroke.samples.len() < 2 {
                continue;
            }
            let c = srgb3(stroke.brush.color);
            let _ = write!(buf, "{:.3} {:.3} {:.3} RG {:.2} w\n", c[0], c[1], c[2], stroke.brush.width.max(0.3));
            let mut first = true;
            for smp in &stroke.samples {
                let (px, py) = match off {
                    Some((ox, oy)) => (smp.pos.x - ox, h_pt - (smp.pos.y - oy)),
                    None => world_to_pdf(smp.pos.x, smp.pos.y, w_pt, h_pt),
                };
                if first {
                    let _ = write!(buf, "{:.2} {:.2} m ", px, py);
                    first = false;
                } else {
                    let _ = write!(buf, "{:.2} {:.2} l ", px, py);
                }
            }
            buf.extend_from_slice(b"S\n");
        }
    }
}

/// Etiquetas de texto (herramienta de texto) de una pagina.
fn text_items_content(buf: &mut Vec<u8>, page: &PageData, w_pt: f32, h_pt: f32, off: Option<(f32, f32)>) {
    for t in &page.texts {
        if t.content.trim().is_empty() {
            continue;
        }
        let (px, mut py) = match off {
            Some((ox, oy)) => (t.pos.x - ox, h_pt - (t.pos.y - oy)),
            None => world_to_pdf(t.pos.x, t.pos.y, w_pt, h_pt),
        };
        let size = t.size.max(4.0);
        py -= size; // el ancla del TextItem es la esquina superior; el texto PDF va en la linea base
        let rgb = srgb3(t.color);
        for ln in t.content.split('\n') {
            push_text_line(buf, "/F1", size, rgb, px, py, ln);
            py -= size * 1.3;
        }
    }
}

// ----------------------------------------------------------------------------
// Punto de entrada
// ----------------------------------------------------------------------------

/// Exporta un cuaderno completo a PDF. `page_pt` es el tamano de hoja en puntos (None = nota de
/// lienzo infinito: se calcula del contenido). `margins`/`base_size` vienen del DocLayout.
pub fn export_notebook_pdf(
    nb: &NotebookData,
    page_pt: Option<(f32, f32)>,
    margins: [f32; 4],
    base_size: f32,
    dest: &Path,
) -> std::io::Result<()> {
    let mut pdf = Pdf::new();
    let catalog = pdf.alloc();
    let pages_tree = pdf.alloc();
    let font_reg = pdf.add(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec());
    let font_bold = pdf.add(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".to_vec());

    let mut page_ids: Vec<usize> = Vec::new();

    for page in &nb.pages {
        let mut content: Vec<u8> = Vec::new();
        let (w_pt, h_pt) = match page_pt {
            Some((w, h)) => {
                // Hoja A4: cuerpo de texto + tinta + etiquetas (pagina centrada en el origen).
                body_content(&mut content, &page.body, w, h, margins, base_size);
                ink_content(&mut content, &page.doc, w, h, None);
                text_items_content(&mut content, page, w, h, None);
                (w, h)
            }
            None => {
                // Nota de lienzo infinito: solo tinta + etiquetas, recentradas al bbox del contenido.
                let (min_x, min_y, w, h) = content_box(page);
                let off = Some((min_x, min_y));
                ink_content(&mut content, &page.doc, w, h, off);
                text_items_content(&mut content, page, w, h, off);
                (w, h)
            }
        };

        let mut content_obj = Vec::new();
        let _ = write!(content_obj, "<< /Length {} >>\nstream\n", content.len());
        content_obj.extend_from_slice(&content);
        content_obj.extend_from_slice(b"\nendstream");
        let content_id = pdf.add(content_obj);

        let page_id = pdf.alloc();
        let mut page_obj = Vec::new();
        let _ = write!(
            page_obj,
            "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << /Font << /F1 {} 0 R /F2 {} 0 R >> >> /Contents {} 0 R >>",
            pages_tree, w_pt.max(1.0), h_pt.max(1.0), font_reg, font_bold, content_id
        );
        pdf.set(page_id, page_obj);
        page_ids.push(page_id);
    }

    if page_ids.is_empty() {
        let (w_pt, h_pt) = page_pt.unwrap_or((595.0, 842.0));
        let content_id = pdf.add(b"<< /Length 0 >>\nstream\n\nendstream".to_vec());
        let page_id = pdf.alloc();
        let mut page_obj = Vec::new();
        let _ = write!(
            page_obj,
            "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << /Font << /F1 {} 0 R /F2 {} 0 R >> >> /Contents {} 0 R >>",
            pages_tree, w_pt, h_pt, font_reg, font_bold, content_id
        );
        pdf.set(page_id, page_obj);
        page_ids.push(page_id);
    }

    let mut kids = String::new();
    for id in &page_ids {
        let _ = write!(kids, "{} 0 R ", id);
    }
    pdf.set(pages_tree, format!("<< /Type /Pages /Kids [ {}] /Count {} >>", kids, page_ids.len()).into_bytes());
    pdf.set(catalog, format!("<< /Type /Catalog /Pages {} 0 R >>", pages_tree).into_bytes());

    let bytes = pdf.finish(catalog);
    let mut f = std::fs::File::create(dest)?;
    f.write_all(&bytes)?;
    Ok(())
}

/// Caja del contenido de una pagina infinita: (min_x, min_y, ancho, alto) en puntos, con margen.
fn content_box(page: &PageData) -> (f32, f32, f32, f32) {
    let pad = 40.0;
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    let mut any = false;
    for layer in &page.doc.layers {
        for stroke in &layer.strokes {
            for smp in &stroke.samples {
                any = true;
                min.0 = min.0.min(smp.pos.x);
                min.1 = min.1.min(smp.pos.y);
                max.0 = max.0.max(smp.pos.x);
                max.1 = max.1.max(smp.pos.y);
            }
        }
    }
    for t in &page.texts {
        any = true;
        min.0 = min.0.min(t.pos.x);
        min.1 = min.1.min(t.pos.y);
        max.0 = max.0.max(t.pos.x + t.size * 8.0);
        max.1 = max.1.max(t.pos.y + t.size);
    }
    if !any {
        return (0.0, 0.0, 595.0, 842.0);
    }
    let w = (max.0 - min.0 + pad * 2.0).max(60.0);
    let h = (max.1 - min.1 + pad * 2.0).max(60.0);
    (min.0 - pad, min.1 - pad, w, h)
}
