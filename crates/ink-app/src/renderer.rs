//! Estado de GPU (wgpu) y pipeline de render.
//!
//! Esta es la capa que toca la GPU. Es delgada a proposito: recibe geometria ya
//! calculada por `ink-core` (trazos confirmados y trazo activo) y la dibuja. Toda
//! la logica de tinta vive en el nucleo; aqui solo subimos buffers y dibujamos.

use std::sync::Arc;

use ink_core::Vertex;
use winit::window::Window;

/// Color de fondo (papel). Off-white suave.
const BG: wgpu::Color = wgpu::Color { r: 0.965, g: 0.965, b: 0.975, a: 1.0 };
/// Muestras de MSAA: 4x suaviza los bordes de los trazos sin costo notable.
const SAMPLE_COUNT: u32 = 4;

const VERTEX_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRS,
    }
}

/// Buffer de vertices que crece bajo demanda y se actualiza con `write_buffer`
/// (evita reasignar en cada frame mientras se dibuja).
struct DynBuffer {
    buf: Option<wgpu::Buffer>,
    capacity: u64,
    len: u32,
}

impl DynBuffer {
    fn new() -> Self {
        Self { buf: None, capacity: 0, len: 0 }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, verts: &[Vertex]) {
        self.len = verts.len() as u32;
        if verts.is_empty() {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(verts);
        let needed = bytes.len() as u64;
        if self.buf.is_none() || needed > self.capacity {
            let cap = needed.next_power_of_two().max(4096);
            self.buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ink dyn vbuf"),
                size: cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.capacity = cap;
        }
        queue.write_buffer(self.buf.as_ref().unwrap(), 0, bytes);
    }
}

pub struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    camera_buf: wgpu::Buffer,
    camera_bg: wgpu::BindGroup,
    msaa_view: wgpu::TextureView,
    grid: DynBuffer,
    committed: DynBuffer,
    active: DynBuffer,
    bg: wgpu::Color,
    pub supported_present_modes: Vec<wgpu::PresentMode>,
    egui_renderer: egui_wgpu::Renderer,
}

impl GpuState {
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window).expect("crear surface");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("no se encontro un adaptador de GPU compatible");

        log::info!("GPU: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("ink device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .expect("no se pudo crear el device");

        let caps = surface.get_capabilities(&adapter);
        // Formato NO sRGB => los colores se ven tal cual los definimos (WYSIWYG).
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let supported_present_modes = caps.present_modes.clone();
        let present_mode = pick_present_mode(&supported_present_modes);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        // Uniform de camara: una mat4x4<f32> = 64 bytes.
        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let camera_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ink shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ink pipeline layout"),
            bind_group_layouts: &[Some(&camera_bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ink pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: SAMPLE_COUNT,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        // Renderizador de egui (UI). Debe usar el MISMO numero de muestras MSAA
        // que nuestro render pass, porque dibuja en el mismo attachment.
        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            format,
            egui_wgpu::RendererOptions {
                msaa_samples: SAMPLE_COUNT,
                ..Default::default()
            },
        );

        let msaa_view = create_msaa(&device, &config);

        Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            camera_buf,
            camera_bg,
            msaa_view,
            grid: DynBuffer::new(),
            committed: DynBuffer::new(),
            active: DynBuffer::new(),
            bg: BG,
            supported_present_modes,
            egui_renderer,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.msaa_view = create_msaa(&self.device, &self.config);
    }

    pub fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.config);
        self.msaa_view = create_msaa(&self.device, &self.config);
    }

    pub fn set_present_mode(&mut self, mode: wgpu::PresentMode) {
        self.config.present_mode = mode;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.config.present_mode
    }

    pub fn width(&self) -> u32 {
        self.config.width
    }

    pub fn height(&self) -> u32 {
        self.config.height
    }

    pub fn update_camera(&mut self, view_proj: [[f32; 4]; 4]) {
        self.queue
            .write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&view_proj));
    }

    pub fn set_committed(&mut self, verts: &[Vertex]) {
        self.committed.upload(&self.device, &self.queue, verts);
    }

    pub fn set_grid(&mut self, verts: &[Vertex]) {
        self.grid.upload(&self.device, &self.queue, verts);
    }

    /// Cambia el color de fondo (papel). RGBA en 0..=1, lineal (sin sRGB).
    pub fn set_bg(&mut self, c: [f32; 4]) {
        self.bg = wgpu::Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: c[3] as f64 };
    }

    pub fn set_active(&mut self, verts: &[Vertex]) {
        self.active.upload(&self.device, &self.queue, verts);
    }

    pub fn render(
        &mut self,
        egui_primitives: &[egui::ClippedPrimitive],
        egui_textures_delta: &egui::TexturesDelta,
        screen: &egui_wgpu::ScreenDescriptor,
    ) {
        // En wgpu 29, get_current_texture() devuelve un enum, no un Result.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Occluded => {
                self.reconfigure();
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout => return,
            wgpu::CurrentSurfaceTexture::Validation => {
                log::warn!("surface: error de validacion");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // 1) Actualizar texturas de egui (atlas de fuentes, iconos, etc.).
        for (id, delta) in &egui_textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ink encoder") });

        // 2) Subir los buffers de egui (graba en el encoder; puede devolver command buffers).
        let egui_user_buffers = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            egui_primitives,
            screen,
        );

        {
            // forget_lifetime(): egui_wgpu::Renderer::render exige un RenderPass<'static>.
            let mut rpass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("ink pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.msaa_view,
                        depth_slice: None,
                        resolve_target: Some(&view),
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(self.bg),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();

            // --- Lienzo: rejilla (detras), luego nuestros trazos ---
            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, &self.camera_bg, &[]);
            if let Some(buf) = &self.grid.buf {
                if self.grid.len > 0 {
                    rpass.set_vertex_buffer(0, buf.slice(..));
                    rpass.draw(0..self.grid.len, 0..1);
                }
            }
            if let Some(buf) = &self.committed.buf {
                if self.committed.len > 0 {
                    rpass.set_vertex_buffer(0, buf.slice(..));
                    rpass.draw(0..self.committed.len, 0..1);
                }
            }
            if let Some(buf) = &self.active.buf {
                if self.active.len > 0 {
                    rpass.set_vertex_buffer(0, buf.slice(..));
                    rpass.draw(0..self.active.len, 0..1);
                }
            }

            // --- UI de egui encima ---
            self.egui_renderer
                .render(&mut rpass, egui_primitives, screen);
        }

        // 3) Liberar texturas que egui ya no usa.
        for id in &egui_textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        // Los command buffers de egui van ANTES que el nuestro.
        self.queue.submit(
            egui_user_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        frame.present();
    }
}

fn create_msaa(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("msaa target"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: SAMPLE_COUNT,
        dimension: wgpu::TextureDimension::D2,
        format: config.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn pick_present_mode(modes: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    // Mailbox: baja latencia y sin tearing. Fifo siempre esta disponible (vsync).
    if modes.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    }
}

pub fn present_mode_name(mode: wgpu::PresentMode) -> &'static str {
    match mode {
        wgpu::PresentMode::Mailbox => "Mailbox",
        wgpu::PresentMode::Immediate => "Immediate",
        wgpu::PresentMode::Fifo => "Fifo",
        wgpu::PresentMode::FifoRelaxed => "FifoRelaxed",
        wgpu::PresentMode::AutoVsync => "AutoVsync",
        wgpu::PresentMode::AutoNoVsync => "AutoNoVsync",
    }
}
