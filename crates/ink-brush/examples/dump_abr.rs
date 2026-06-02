//! Verificador del parser .abr: lee un archivo, imprime cuantas puntas trae y sus
//! tamanos, y vuelca las primeras N como PNG (escala de grises) para inspeccion.
//!
//! Uso:
//!   cargo run -p ink-brush --example dump_abr -- "<ruta .abr>" [carpeta_salida] [n]

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("uso: dump_abr <ruta.abr> [carpeta_salida] [n_max_png]");
        std::process::exit(2);
    }
    let path = &args[1];
    let out_dir = args.get(2).cloned().unwrap_or_else(|| "abr_dump".to_string());
    let n_max: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(16);

    let bytes = std::fs::read(path).expect("no se pudo leer el archivo");
    println!("Archivo: {} ({} bytes)", path, bytes.len());

    let brushes = match ink_brush::parse_abr(&bytes) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("ERROR al parsear: {e}");
            std::process::exit(1);
        }
    };

    println!("Puntas encontradas: {}", brushes.len());
    let mut min_d = u32::MAX;
    let mut max_d = 0u32;
    let mut empty = 0;
    for b in &brushes {
        min_d = min_d.min(b.width.min(b.height));
        max_d = max_d.max(b.width.max(b.height));
        // Cuantas puntas parecen "vacias" (todo 0) -> indicio de mal parseo.
        if b.alpha.iter().all(|&v| v == 0) {
            empty += 1;
        }
    }
    if !brushes.is_empty() {
        println!("Dimensiones: min lado={min_d}px, max lado={max_d}px");
        println!("Puntas totalmente vacias (sospechosas): {empty}/{}", brushes.len());
    }

    std::fs::create_dir_all(&out_dir).ok();
    for (i, b) in brushes.iter().take(n_max).enumerate() {
        let file = format!("{i:03}_{}x{}.png", b.width, b.height);
        let full = Path::new(&out_dir).join(&file);
        write_gray_png(&full, b.width, b.height, &b.alpha);
        let mn = b.alpha.iter().copied().min().unwrap_or(0);
        let mx = b.alpha.iter().copied().max().unwrap_or(0);
        println!("  [{i:03}] {}x{}  alfa[min={mn} max={mx}]  id={}", b.width, b.height, b.id);
    }
    println!("PNGs volcados en: {out_dir}");
}

fn write_gray_png(path: &Path, w: u32, h: u32, alpha: &[u8]) {
    let file = std::fs::File::create(path).expect("crear png");
    let w_buf = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(w_buf, w, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(alpha).expect("png data");
}
