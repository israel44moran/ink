//! Rejilla del lienzo ("papel"): genera la geometria de lineas visible para el
//! tipo de cuadricula activo. Es portable (sin GPU) y se dibuja DETRAS de la
//! tinta, en coordenadas de mundo, asi que paneA y hace zoom junto con el dibujo.
//!
//! Para no generar millones de lineas al alejar el zoom, la separacion se adapta
//! por niveles (LOD): cada celda mantiene un tamano minimo en pantalla.

use crate::camera::Camera;
use crate::stroke::Vertex;
use glam::Vec2;

/// Tipo de cuadricula (igual que las opciones del panel "Area de trabajo").
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GridKind {
    None,     // sin cuadricula
    Dots,     // cuadricula de puntos
    Squares,  // papel milimetrado (lineas en cruz)
    Lines,    // papel rayado (solo horizontales)
    Iso,      // isometrica (lineas a ±30°)
    Triangle, // triangular (tres direcciones)
    P1,       // perspectiva de 1 punto
    P2,       // perspectiva de 2 puntos (Pro)
    P3,       // perspectiva de 3 puntos (Pro)
}

impl Default for GridKind {
    fn default() -> Self {
        GridKind::None
    }
}

#[inline]
fn push_tri(out: &mut Vec<Vertex>, a: Vec2, b: Vec2, c: Vec2, col: [f32; 4]) {
    // time = 0: la rejilla nunca se borra (se dibuja con la mascara "nunca-borrada").
    out.push(Vertex { pos: [a.x, a.y], color: col, time: 0.0 });
    out.push(Vertex { pos: [b.x, b.y], color: col, time: 0.0 });
    out.push(Vertex { pos: [c.x, c.y], color: col, time: 0.0 });
}

/// Anexa un segmento grueso (quad) de `a` a `b` con medio-ancho `half`.
fn push_line(out: &mut Vec<Vertex>, a: Vec2, b: Vec2, half: f32, col: [f32; 4]) {
    let d = b - a;
    let len = d.length();
    if len < 1e-6 {
        return;
    }
    let n = Vec2::new(-d.y, d.x) / len * half;
    let (p0, p1, p2, p3) = (a + n, b + n, b - n, a - n);
    push_tri(out, p0, p1, p2, col);
    push_tri(out, p0, p2, p3, col);
}

/// Anexa un cuadrado centrado en `c` de lado `2*half` (para los puntos).
fn push_dot(out: &mut Vec<Vertex>, c: Vec2, half: f32, col: [f32; 4]) {
    let a = c + Vec2::new(-half, -half);
    let b = c + Vec2::new(half, -half);
    let d = c + Vec2::new(half, half);
    let e = c + Vec2::new(-half, half);
    push_tri(out, a, b, d, col);
    push_tri(out, a, d, e, col);
}

/// Region visible del mundo (esquinas) segun la camara.
fn visible_rect(cam: &Camera) -> (Vec2, Vec2) {
    let a = cam.screen_to_world(Vec2::ZERO);
    let b = cam.screen_to_world(cam.viewport);
    (a.min(b), a.max(b))
}

/// Familia de lineas paralelas con direccion `ang_deg`, separadas `spacing` en su
/// perpendicular, que cubren el rectangulo [min,max]. Robusto para cualquier angulo.
fn parallel_lines(
    out: &mut Vec<Vertex>,
    ang_deg: f32,
    spacing: f32,
    min: Vec2,
    max: Vec2,
    half: f32,
    col: [f32; 4],
) {
    if spacing <= 1e-4 {
        return;
    }
    let a = ang_deg.to_radians();
    let u = Vec2::new(a.cos(), a.sin()); // direccion de la linea
    let nrm = Vec2::new(-u.y, u.x); // perpendicular (offset entre lineas)
    // Proyectar las 4 esquinas sobre la normal para saber el rango de offsets.
    let corners = [min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)];
    let mut omin = f32::MAX;
    let mut omax = f32::MIN;
    let mut tmin = f32::MAX;
    let mut tmax = f32::MIN;
    for c in corners {
        let o = c.dot(nrm);
        let t = c.dot(u);
        omin = omin.min(o);
        omax = omax.max(o);
        tmin = tmin.min(t);
        tmax = tmax.max(t);
    }
    let pad = spacing;
    let k0 = ((omin - pad) / spacing).floor() as i64;
    let k1 = ((omax + pad) / spacing).ceil() as i64;
    // Tope de seguridad por si el LOD fallara.
    if k1 - k0 > 4000 {
        return;
    }
    let ext = (tmax - tmin) * 0.5 + spacing;
    let mid_t = (tmin + tmax) * 0.5;
    for k in k0..=k1 {
        let o = k as f32 * spacing;
        let base = nrm * o + u * mid_t;
        push_line(out, base - u * ext, base + u * ext, half, col);
    }
}

/// Construye la rejilla visible y la anexa a `out`.
///
/// - `base`      = espaciado entre lineas PRINCIPALES (unidades de mundo).
/// - `divisions` = subdivisiones entre principales (1 = solo principales).
/// - `line_width`= grosor de linea (pts).
/// - `color`     = color de las lineas principales (las menores usan menos alfa).
pub fn build_grid(out: &mut Vec<Vertex>, kind: GridKind, cam: &Camera, base: f32, divisions: u32, line_width: f32, limit: Option<(Vec2, Vec2)>, color: [f32; 4]) {
    use GridKind::*;
    if matches!(kind, None) || base <= 0.0 {
        return;
    }
    let (mut min, mut max) = visible_rect(cam);
    // "Limitar a la mesa de trabajo": recortar la region a la mesa.
    if let Some((lmin, lmax)) = limit {
        min = min.max(lmin);
        max = max.min(lmax);
        if min.x >= max.x || min.y >= max.y {
            return;
        }
    }
    // LOD: agrandar la celda hasta que mida >= ~14 px en pantalla.
    let mut step = base.max(0.5);
    let mut guard = 0;
    while step * cam.zoom < 14.0 && guard < 40 {
        step *= 2.0;
        guard += 1;
    }
    let half = (line_width.max(0.2) * 0.55 / cam.zoom).max(0.0004);
    let minor = [color[0], color[1], color[2], color[3] * 0.5];
    // Espaciado de las subdivisiones (si caben en pantalla).
    let sub = if divisions > 1 { step / divisions as f32 } else { 0.0 };
    let draw_sub = sub > 0.0 && sub * cam.zoom >= 6.0;

    match kind {
        None => {}
        Squares => {
            if draw_sub {
                parallel_lines(out, 0.0, sub, min, max, half * 0.6, minor);
                parallel_lines(out, 90.0, sub, min, max, half * 0.6, minor);
            }
            parallel_lines(out, 0.0, step, min, max, half, color);
            parallel_lines(out, 90.0, step, min, max, half, color);
        }
        Lines => {
            if draw_sub {
                parallel_lines(out, 0.0, sub, min, max, half * 0.6, minor);
            }
            parallel_lines(out, 0.0, step, min, max, half, color);
        }
        Dots => {
            // Puntos en las intersecciones de la malla.
            let dot = (1.3 / cam.zoom).max(0.001);
            let kx0 = (min.x / step).floor() as i64;
            let kx1 = (max.x / step).ceil() as i64;
            let ky0 = (min.y / step).floor() as i64;
            let ky1 = (max.y / step).ceil() as i64;
            if (kx1 - kx0).max(0) * (ky1 - ky0).max(0) <= 200_000 {
                let mut x = kx0;
                while x <= kx1 {
                    let mut y = ky0;
                    while y <= ky1 {
                        push_dot(out, Vec2::new(x as f32 * step, y as f32 * step), dot, color);
                        y += 1;
                    }
                    x += 1;
                }
            }
        }
        Iso => {
            parallel_lines(out, 30.0, step, min, max, half, minor);
            parallel_lines(out, -30.0, step, min, max, half, minor);
            parallel_lines(out, 90.0, step, min, max, half, minor);
        }
        Triangle => {
            parallel_lines(out, 0.0, step, min, max, half, minor);
            parallel_lines(out, 60.0, step, min, max, half, minor);
            parallel_lines(out, -60.0, step, min, max, half, minor);
        }
        P1 => {
            // Perspectiva simple de 1 punto: horizonte + rejilla tenue + radiales
            // hacia un punto de fuga en el centro de la vista.
            parallel_lines(out, 0.0, step * 2.0, min, max, half, minor);
            parallel_lines(out, 90.0, step * 2.0, min, max, half, minor);
            let vp = cam.screen_to_world(cam.viewport * 0.5);
            let r = (max - min).length();
            for i in 0..24 {
                let a = (i as f32 / 24.0) * std::f32::consts::TAU;
                let dirv = Vec2::new(a.cos(), a.sin());
                push_line(out, vp, vp + dirv * r, half, color);
            }
            // Linea de horizonte.
            push_line(out, Vec2::new(min.x, vp.y), Vec2::new(max.x, vp.y), half * 1.6, color);
        }
        P2 => {
            // Perspectiva de 2 puntos: dos fugas en el horizonte (izquierda y derecha)
            // + verticales. Rayos en abanico desde cada punto de fuga.
            let vp_y = cam.screen_to_world(cam.viewport * 0.5).y;
            let w = max.x - min.x;
            let r = (max - min).length() * 2.0;
            let vps = [Vec2::new(min.x - w * 0.35, vp_y), Vec2::new(max.x + w * 0.35, vp_y)];
            for vp in vps {
                for i in 0..22 {
                    let a = (i as f32 / 22.0) * std::f32::consts::TAU;
                    push_line(out, vp, vp + Vec2::new(a.cos(), a.sin()) * r, half, minor);
                }
            }
            parallel_lines(out, 90.0, step * 1.5, min, max, half, minor);
            push_line(out, Vec2::new(min.x, vp_y), Vec2::new(max.x, vp_y), half * 1.6, color);
        }
        P3 => {
            // Perspectiva de 3 puntos: dos fugas en el horizonte + una cenital (arriba).
            let mid = cam.screen_to_world(cam.viewport * 0.5);
            let w = max.x - min.x;
            let h = max.y - min.y;
            let r = (max - min).length() * 2.2;
            let vps = [
                Vec2::new(min.x - w * 0.35, mid.y),
                Vec2::new(max.x + w * 0.35, mid.y),
                Vec2::new(mid.x, min.y - h * 0.55),
            ];
            for vp in vps {
                for i in 0..22 {
                    let a = (i as f32 / 22.0) * std::f32::consts::TAU;
                    push_line(out, vp, vp + Vec2::new(a.cos(), a.sin()) * r, half, minor);
                }
            }
            push_line(out, Vec2::new(min.x, mid.y), Vec2::new(max.x, mid.y), half * 1.4, color);
        }
    }
}
