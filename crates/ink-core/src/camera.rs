//! Camara 2D del lienzo infinito.
//!
//! El mundo usa Y hacia abajo (igual que la pantalla), asi nada se ve "volteado".
//! La proyeccion a clip-space invierte Y porque clip-space tiene Y hacia arriba.

use glam::{Mat4, Vec2, Vec4};

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// Punto de mundo que se muestra en el centro del viewport.
    pub center: Vec2,
    /// Pixeles de pantalla por unidad de mundo (escala). Mayor = mas acercado.
    pub zoom: f32,
    /// Tamano del viewport en pixeles fisicos.
    pub viewport: Vec2,
}

impl Camera {
    pub fn new(viewport: Vec2) -> Self {
        Self { center: Vec2::ZERO, zoom: 1.0, viewport }
    }

    /// Convierte un punto de pantalla (pixeles fisicos) a coordenadas de mundo.
    #[inline]
    pub fn screen_to_world(&self, p: Vec2) -> Vec2 {
        self.center + (p - self.viewport * 0.5) / self.zoom
    }

    /// Convierte un punto de mundo a pixeles de pantalla (inverso de `screen_to_world`).
    #[inline]
    pub fn world_to_screen(&self, w: Vec2) -> Vec2 {
        (w - self.center) * self.zoom + self.viewport * 0.5
    }

    /// Desplaza la camara segun un arrastre en pixeles de pantalla.
    #[inline]
    pub fn pan_pixels(&mut self, delta_screen: Vec2) {
        self.center -= delta_screen / self.zoom;
    }

    /// Hace zoom manteniendo fijo el punto de mundo bajo `screen_point` (zoom al cursor).
    pub fn zoom_at(&mut self, screen_point: Vec2, factor: f32) {
        let before = self.screen_to_world(screen_point);
        self.zoom = (self.zoom * factor).clamp(0.02, 200.0);
        let after = self.screen_to_world(screen_point);
        self.center += before - after;
    }

    /// Matriz vista-proyeccion (mundo -> clip), lista para subir a la GPU.
    pub fn view_proj(&self) -> [[f32; 4]; 4] {
        let vw = self.viewport.x.max(1.0);
        let vh = self.viewport.y.max(1.0);
        let sx = 2.0 * self.zoom / vw;
        let sy = 2.0 * self.zoom / vh;
        let cx = self.center.x;
        let cy = self.center.y;
        // Columnas (glam es column-major, igual que WGSL).
        Mat4::from_cols(
            Vec4::new(sx, 0.0, 0.0, 0.0),
            Vec4::new(0.0, -sy, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(-sx * cx, sy * cy, 0.0, 1.0),
        )
        .to_cols_array_2d()
    }
}
