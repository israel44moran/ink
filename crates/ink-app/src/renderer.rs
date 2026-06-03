//! Estado de GPU (wgpu) y pipeline de render.
//!
//! Esta es la capa que toca la GPU. Es delgada a proposito: recibe geometria ya
//! calculada por `ink-core` (trazos confirmados y trazo activo) y la dibuja. Toda
//! la logica de tinta vive en el nucleo; aqui solo subimos buffers y dibujamos.

use std::collections::HashMap;
use std::sync::Arc;

use ink_core::{StampVertex, Vertex};
use winit::window::Window;

/// Color de fondo (papel). Off-white suave.
const BG: wgpu::Color = wgpu::Color { r: 0.965, g: 0.965, b: 0.975, a: 1.0 };
/// Muestras de MSAA: 4x suaviza los bordes de los trazos sin costo notable.
const SAMPLE_COUNT: u32 = 4;

/// Mascara de borrado (goma raster). Textura R8 en espacio de mundo: 1 = visible,
/// 0 = borrado. Los shaders de contenido multiplican el alfa por esta mascara, asi la
/// goma borra a nivel de pixel. Cubre `MASK_WORLD` unidades de mundo centradas en el
/// origen a `MASK_RES` px (1 texel = 1 unidad). R8 => barata en VRAM.
const MASK_RES: u32 = 8192;
const MASK_WORLD: f32 = 8192.0;

const VERTEX_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRS,
    }
}

const STAMP_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32];

fn stamp_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<StampVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &STAMP_ATTRS,
    }
}

// Instancia de disco de borrado: [center.x, center.y, radio, fuerza] = 16 bytes.
const ERASE_INST_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

fn erase_inst_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: 16,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ERASE_INST_ATTRS,
    }
}

/// Instancia de una "carta" de la biblioteca (12 floats = 48 bytes):
/// [center.x, center.y, half.x, half.y, rotX, rotY, pointer.x, pointer.y, hover, baseR, baseG, baseB].
pub type CardInstance = [f32; 12];

const CARD_INST_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32, 5 => Float32x3
];

fn card_inst_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: 48,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &CARD_INST_ATTRS,
    }
}

/// Una punta de pincel ya subida a GPU: bind group con su textura + aspecto (w/h).
struct TipGpu {
    bind_group: wgpu::BindGroup,
    aspect: f32,
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

    fn upload<T: bytemuck::Pod>(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, verts: &[T]) {
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

    /// Anexa SOLO los vertices nuevos al final del buffer (teselado incremental del
    /// trazo en vivo: O(1) por punto en vez de re-subir todo). Devuelve `false` si no
    /// caben (hay que crecer); en ese caso el llamador debe hacer `upload` completo.
    fn append<T: bytemuck::Pod>(&mut self, queue: &wgpu::Queue, new_verts: &[T]) -> bool {
        if new_verts.is_empty() {
            return true;
        }
        let stride = std::mem::size_of::<T>() as u64;
        let offset = self.len as u64 * stride;
        let bytes: &[u8] = bytemuck::cast_slice(new_verts);
        let needed = offset + bytes.len() as u64;
        if self.buf.is_none() || needed > self.capacity {
            return false; // no cabe: el llamador re-sube el mesh completo (crece el buffer)
        }
        queue.write_buffer(self.buf.as_ref().unwrap(), offset, bytes);
        self.len += new_verts.len() as u32;
        true
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
    /// Recorte (scissor) del CONTENIDO en pixeles fisicos: cuando el cuaderno es de hojas,
    /// limita el dibujo/rejilla a la hoja. `None` = sin recorte (lienzo infinito).
    content_clip: Option<(u32, u32, u32, u32)>,
    pub supported_present_modes: Vec<wgpu::PresentMode>,
    egui_renderer: egui_wgpu::Renderer,

    // --- Pinceles texturizados estilo Photoshop (estampados) ---
    stamp_pipeline: wgpu::RenderPipeline,
    tip_bgl: wgpu::BindGroupLayout,
    tip_sampler: wgpu::Sampler,
    /// Puntas subidas a GPU, por id.
    tips: HashMap<u32, TipGpu>,
    /// Estampados confirmados, agrupados por punta (cada punta = una textura).
    committed_stamps: HashMap<u32, DynBuffer>,
    /// Estampados del trazo en curso + su punta.
    active_stamps: DynBuffer,
    active_stamp_tip: Option<u32>,

    // --- Mascara de borrado (goma raster) ---
    /// Textura R8 (M) en espacio de mundo: el alfa del contenido se multiplica por ella.
    mask_view: wgpu::TextureView,
    /// Bind group para MUESTREAR M en los shaders de contenido (uniform + tex + sampler).
    mask_sample_bg: wgpu::BindGroup,
    /// Igual pero con una textura 1x1 blanca (M=1): para grid/trazo en curso (sin borrar).
    white_sample_bg: wgpu::BindGroup,
    /// Bind group para ESCRIBIR en M (uniform de parametros + de borrado).
    mask_render_bg: wgpu::BindGroup,
    /// Buffer de parametros de la mascara (origen.xy, inv_size.xy). El origen se MUEVE
    /// para que la ventana de borrado siga al contenido (lienzo infinito).
    mask_params_buf: wgpu::Buffer,
    /// Buffer de INSTANCIAS de discos de borrado (se reusa entre llamadas). Permite borrar
    /// muchos discos en un solo draw (sin un submit por disco).
    erase_inst: DynBuffer,
    /// Pipeline que escribe el tiempo de borrado de cada disco de la goma en M.
    mask_erase_pipeline: wgpu::RenderPipeline,
    /// Pipeline que ESTAMPA la forma de un pincel en M (goma con textura suave): dibuja
    /// quads con la textura de la punta y escribe (tiempo, cobertura).
    mask_erase_stamp_pipeline: wgpu::RenderPipeline,
    /// Buffer de vertices del estampado de goma con forma (se reusa entre llamadas).
    erase_stamp_buf: DynBuffer,

    // --- Cartas hologr aficas de la biblioteca (Home) ---
    /// Pipeline que dibuja cada cuaderno como una "carta" con inclinacion 3D + holografico.
    card_pipeline: wgpu::RenderPipeline,
    /// Instancias de cartas a dibujar este frame (vacio = no se dibujan, p.ej. en Canvas).
    card_inst: DynBuffer,
    /// Uniform con el viewport (px) y la focal para la perspectiva del tilt.
    card_view_buf: wgpu::Buffer,
    card_view_bg: wgpu::BindGroup,
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

        // ---------------- Mascara de borrado (goma raster) ----------------
        let mask_min = -MASK_WORLD * 0.5;
        let mask_params: [f32; 4] = [mask_min, mask_min, 1.0 / MASK_WORLD, 1.0 / MASK_WORLD];
        let mask_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mask params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&mask_params_buf, 0, bytemuck::cast_slice(&mask_params));

        let mask_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("erase mask"),
            size: wgpu::Extent3d { width: MASK_RES, height: MASK_RES, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Rg32Float: R = TIEMPO del ultimo borrado (0 = nunca), G = FUERZA (0..1) para la
            // goma con textura suave (borrado parcial).
            format: wgpu::TextureFormat::Rg32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mask_view = mask_tex.create_view(&wgpu::TextureViewDescriptor::default());
        // Limpiar M a 0 (ningun borrado todavia).
        {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("clear mask") });
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear mask pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &mask_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            queue.submit(std::iter::once(enc.finish()));
        }
        // Textura 1x1 con tiempo -1 (y fuerza 0) para grid/trazo en curso: cualquier
        // `time >= 0` es mayor que -1, asi que SIEMPRE se ven (no se borran nunca).
        let white_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("never-erased mask"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&[-1.0f32, 0.0f32]),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let white_view = white_tex.create_view(&wgpu::TextureViewDescriptor::default());
        // R32Float no es filtrable: muestreo NEAREST (la comparacion de tiempo es binaria).
        let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mask sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // BGL para MUESTREAR M (en los shaders de contenido): uniform + textura + sampler.
        let mask_sample_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask sample bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let mask_sample_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask sample bg"),
            layout: &mask_sample_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: mask_params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&mask_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&mask_sampler) },
            ],
        });
        let white_sample_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("white sample bg"),
            layout: &mask_sample_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: mask_params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&white_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&mask_sampler) },
            ],
        });

        // BGL para ESCRIBIR en M: solo los parametros de mascara (origen + inv_size). Los
        // discos de borrado llegan como INSTANCIAS (vertex buffer), no por uniform.
        let mask_render_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask render bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let mask_render_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask render bg"),
            layout: &mask_render_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: mask_params_buf.as_entire_binding() }],
        });

        // Pipeline que escribe el TIEMPO de borrado en M (R32Float). Los discos llegan
        // como instancias [cx, cy, radio, tiempo]. Sin blend (replace): los borrados se
        // aplican en orden de tiempo creciente, asi que el ultimo (mayor) gana donde solapan.
        let mask_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mask shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mask.wgsl").into()),
        });
        let mask_render_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask render layout"),
            bind_group_layouts: &[Some(&mask_render_bgl)],
            immediate_size: 0,
        });
        let mask_erase_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mask erase pipeline"),
            layout: Some(&mask_render_layout),
            vertex: wgpu::VertexState { module: &mask_shader, entry_point: Some("vs_erase"), buffers: &[erase_inst_layout()], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &mask_shader,
                entry_point: Some("fs_erase"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rg32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: 1, mask: !0, alpha_to_coverage_enabled: false },
            multiview_mask: None,
            cache: None,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ink shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ink pipeline layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&mask_sample_bgl)],
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

        // --- Pipeline de ESTAMPADOS texturizados (pinceles estilo Photoshop) ---
        let tip_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tip bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let tip_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tip sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let stamp_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stamp shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("stamp.wgsl").into()),
        });
        let stamp_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stamp pipeline layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&tip_bgl), Some(&mask_sample_bgl)],
            immediate_size: 0,
        });
        let stamp_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stamp pipeline"),
            layout: Some(&stamp_layout),
            vertex: wgpu::VertexState {
                module: &stamp_shader,
                entry_point: Some("vs_main"),
                buffers: &[stamp_vertex_layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &stamp_shader,
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

        // Pipeline de la GOMA CON FORMA: estampa la textura de la punta (group 1) en M
        // escribiendo (tiempo, cobertura). Usa el modulo de mask.wgsl y el layout de StampVertex.
        let mask_erase_stamp_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask erase stamp layout"),
            bind_group_layouts: &[Some(&mask_render_bgl), Some(&tip_bgl)],
            immediate_size: 0,
        });
        let mask_erase_stamp_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mask erase stamp pipeline"),
            layout: Some(&mask_erase_stamp_layout),
            vertex: wgpu::VertexState { module: &mask_shader, entry_point: Some("vs_erase_stamp"), buffers: &[stamp_vertex_layout()], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &mask_shader,
                entry_point: Some("fs_erase_stamp"),
                targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::Rg32Float, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: 1, mask: !0, alpha_to_coverage_enabled: false },
            multiview_mask: None,
            cache: None,
        });

        // --- Pipeline de CARTAS de la biblioteca (inclinacion 3D en perspectiva + holografico) ---
        let card_view_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("card view buf"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let card_view_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("card view bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let card_view_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("card view bg"),
            layout: &card_view_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: card_view_buf.as_entire_binding() }],
        });
        let card_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("card shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("card.wgsl").into()),
        });
        let card_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("card layout"),
            bind_group_layouts: &[Some(&card_view_bgl)],
            immediate_size: 0,
        });
        let card_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("card pipeline"),
            layout: Some(&card_layout),
            vertex: wgpu::VertexState { module: &card_shader, entry_point: Some("vs_main"), buffers: &[card_inst_layout()], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &card_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: SAMPLE_COUNT, mask: !0, alpha_to_coverage_enabled: false },
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
            content_clip: None,
            supported_present_modes,
            egui_renderer,
            stamp_pipeline,
            tip_bgl,
            tip_sampler,
            tips: HashMap::new(),
            committed_stamps: HashMap::new(),
            active_stamps: DynBuffer::new(),
            active_stamp_tip: None,
            mask_view,
            mask_sample_bg,
            white_sample_bg,
            mask_render_bg,
            mask_params_buf,
            erase_inst: DynBuffer::new(),
            mask_erase_pipeline,
            mask_erase_stamp_pipeline,
            erase_stamp_buf: DynBuffer::new(),
            card_pipeline,
            card_inst: DynBuffer::new(),
            card_view_buf,
            card_view_bg,
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

    /// Define las CARTAS de la biblioteca a dibujar este frame (vacio = ninguna, p.ej. en el
    /// lienzo). Sube las instancias y actualiza el uniform de viewport/focal para el tilt 3D.
    /// `vw`/`vh` deben estar en las MISMAS unidades que las posiciones de las cartas (las del
    /// cursor / `camera.viewport`), para que coincidan con el hit-test del hover/clic.
    pub fn set_cards(&mut self, cards: &[CardInstance], vw: f32, vh: f32) {
        self.card_inst.upload(&self.device, &self.queue, cards);
        let view: [f32; 4] = [vw.max(1.0), vh.max(1.0), 900.0, 0.0];
        self.queue.write_buffer(&self.card_view_buf, 0, bytemuck::cast_slice(&view));
    }

    /// Anexa SOLO los vertices de un trazo recien confirmado al buffer committed (sin
    /// re-subir toda la malla). Devuelve `false` si el buffer tuvo que crecer y el
    /// llamador debe re-subir el mesh completo con `set_committed`.
    pub fn append_committed(&mut self, new_verts: &[Vertex]) -> bool {
        self.committed.append(&self.queue, new_verts)
    }

    pub fn set_grid(&mut self, verts: &[Vertex]) {
        self.grid.upload(&self.device, &self.queue, verts);
    }

    /// Cambia el color de fondo (papel). RGBA en 0..=1, lineal (sin sRGB).
    pub fn set_bg(&mut self, c: [f32; 4]) {
        self.bg = wgpu::Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: c[3] as f64 };
    }

    /// Recorta el dibujo y la rejilla a un rectangulo en pixeles fisicos (la hoja de un
    /// cuaderno). `None` quita el recorte (lienzo infinito).
    pub fn set_content_clip(&mut self, rect: Option<(u32, u32, u32, u32)>) {
        self.content_clip = rect;
    }

    pub fn set_active(&mut self, verts: &[Vertex]) {
        self.active.upload(&self.device, &self.queue, verts);
    }

    /// Anexa vertices al buffer activo (teselado incremental). Devuelve `false` si el
    /// buffer tuvo que crecer y el llamador debe re-subir el mesh activo completo.
    pub fn append_active(&mut self, new_verts: &[Vertex]) -> bool {
        self.active.append(&self.queue, new_verts)
    }

    // ---------------- Pinceles texturizados (estampados) ----------------

    /// Aspecto (ancho/alto) de una punta ya subida; 1.0 si no existe.
    pub fn tip_aspect(&self, id: u32) -> f32 {
        self.tips.get(&id).map_or(1.0, |t| t.aspect)
    }

    /// Sube una punta (mascara alfa en escala de grises, `w*h` bytes) como textura R8.
    pub fn upload_tip(&mut self, id: u32, w: u32, h: u32, alpha: &[u8]) {
        let need = (w as usize) * (h as usize);
        if w == 0 || h == 0 || alpha.len() < need {
            return;
        }
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tip tex"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &alpha[..need],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tip bg"),
            layout: &self.tip_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.tip_sampler) },
            ],
        });
        self.tips.insert(id, TipGpu { bind_group, aspect: w as f32 / h as f32 });
    }

    /// Reemplaza los estampados del trazo en curso (de la punta `tip`).
    pub fn set_active_stamps(&mut self, tip: u32, verts: &[StampVertex]) {
        self.active_stamp_tip = Some(tip);
        self.active_stamps.upload(&self.device, &self.queue, verts);
    }

    /// Anexa estampados al trazo en curso. `false` si hay que re-subir completo.
    pub fn append_active_stamps(&mut self, new: &[StampVertex]) -> bool {
        self.active_stamps.append(&self.queue, new)
    }

    pub fn clear_active_stamps(&mut self) {
        self.active_stamps.len = 0;
        self.active_stamp_tip = None;
    }

    /// Reemplaza los estampados CONFIRMADOS de la punta `tip`.
    pub fn set_committed_stamps(&mut self, tip: u32, verts: &[StampVertex]) {
        let buf = self.committed_stamps.entry(tip).or_insert_with(DynBuffer::new);
        buf.upload(&self.device, &self.queue, verts);
    }

    // ---------------- Mascara de borrado (goma raster) ----------------

    /// Lado de la ventana de mascara en unidades de mundo (region cubierta a la vez).
    pub fn mask_world(&self) -> f32 {
        MASK_WORLD
    }

    /// Mueve el ORIGEN (esquina inferior) de la ventana de mascara en espacio de mundo.
    /// Permite que la ventana de borrado siga al contenido (lienzo infinito). Tras moverla
    /// hay que reconstruir M (el llamador lo hace con los trazos de goma guardados).
    pub fn set_mask_origin(&mut self, origin: [f32; 2]) {
        let params: [f32; 4] = [origin[0], origin[1], 1.0 / MASK_WORLD, 1.0 / MASK_WORLD];
        self.queue.write_buffer(&self.mask_params_buf, 0, bytemuck::cast_slice(&params));
    }

    /// Escribe el TIEMPO de borrado de una lista de DISCOS en la mascara (coords de MUNDO).
    /// Cada disco es `[center.x, center.y, radio, tiempo]`. Todos en UN solo draw (una
    /// instancia por disco). Los pixeles bajo cada disco quedan marcados con ese tiempo;
    /// un trazo se vera solo si su `time` es mayor (se dibujo despues del borrado).
    pub fn erase_mask(&mut self, discs: &[[f32; 4]]) {
        if discs.is_empty() {
            return;
        }
        self.erase_inst.upload(&self.device, &self.queue, discs);
        let Some(inst_buf) = self.erase_inst.buf.as_ref() else { return };
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("erase mask enc") });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("erase mask pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.mask_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(&self.mask_erase_pipeline);
            rp.set_bind_group(0, &self.mask_render_bg, &[]);
            rp.set_vertex_buffer(0, inst_buf.slice(..));
            rp.draw(0..6, 0..discs.len() as u32);
        }
        self.queue.submit(std::iter::once(enc.finish()));
    }

    /// ESTAMPA la forma de un pincel (textura de la punta `tip`) en M: cada vertice lleva su
    /// UV y `time`; el fragment escribe (tiempo, cobertura de la punta). Goma con textura
    /// suave (borrado parcial). Si la punta no esta subida, no hace nada.
    pub fn erase_mask_stamps(&mut self, tip: u32, verts: &[StampVertex]) {
        if verts.is_empty() {
            return;
        }
        let Some(t) = self.tips.get(&tip) else { return };
        let tip_bg = &t.bind_group;
        self.erase_stamp_buf.upload(&self.device, &self.queue, verts);
        let Some(vbuf) = self.erase_stamp_buf.buf.as_ref() else { return };
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("erase stamp enc") });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("erase stamp pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.mask_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(&self.mask_erase_stamp_pipeline);
            rp.set_bind_group(0, &self.mask_render_bg, &[]);
            rp.set_bind_group(1, tip_bg, &[]);
            rp.set_vertex_buffer(0, vbuf.slice(..));
            rp.draw(0..verts.len() as u32, 0..1);
        }
        self.queue.submit(std::iter::once(enc.finish()));
    }

    /// Restablece la mascara a 0 (ningun borrado): quita todos los borrados.
    pub fn clear_mask(&mut self) {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("clear mask enc") });
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear mask pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.mask_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.queue.submit(std::iter::once(enc.finish()));
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

            // Recorte del contenido a la hoja (cuadernos de hojas). El fondo (clear) ya
            // cubrio todo; lo que sigue (rejilla + dibujo) queda dentro de la hoja.
            if let Some((x, y, w, h)) = self.content_clip {
                if w > 0 && h > 0 {
                    rpass.set_scissor_rect(x, y, w, h);
                }
            }

            // --- Lienzo: rejilla (detras), luego nuestros trazos ---
            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, &self.camera_bg, &[]);
            // Rejilla: sin mascara de borrado (M=1).
            rpass.set_bind_group(1, &self.white_sample_bg, &[]);
            if let Some(buf) = &self.grid.buf {
                if self.grid.len > 0 {
                    rpass.set_vertex_buffer(0, buf.slice(..));
                    rpass.draw(0..self.grid.len, 0..1);
                }
            }
            // Trazos confirmados: SE aplican los borrados (mascara real).
            rpass.set_bind_group(1, &self.mask_sample_bg, &[]);
            if let Some(buf) = &self.committed.buf {
                if self.committed.len > 0 {
                    rpass.set_vertex_buffer(0, buf.slice(..));
                    rpass.draw(0..self.committed.len, 0..1);
                }
            }
            // Trazo en curso: sin mascara (se dibuja nitido mientras se traza).
            rpass.set_bind_group(1, &self.white_sample_bg, &[]);
            if let Some(buf) = &self.active.buf {
                if self.active.len > 0 {
                    rpass.set_vertex_buffer(0, buf.slice(..));
                    rpass.draw(0..self.active.len, 0..1);
                }
            }

            // --- Estampados texturizados (pinceles estilo Photoshop) ---
            rpass.set_pipeline(&self.stamp_pipeline);
            rpass.set_bind_group(0, &self.camera_bg, &[]);
            // Estampados confirmados: con mascara de borrado.
            rpass.set_bind_group(2, &self.mask_sample_bg, &[]);
            for (tip_id, sbuf) in &self.committed_stamps {
                if sbuf.len > 0 {
                    if let (Some(t), Some(b)) = (self.tips.get(tip_id), &sbuf.buf) {
                        rpass.set_bind_group(1, &t.bind_group, &[]);
                        rpass.set_vertex_buffer(0, b.slice(..));
                        rpass.draw(0..sbuf.len, 0..1);
                    }
                }
            }
            // Estampados en curso: sin mascara.
            rpass.set_bind_group(2, &self.white_sample_bg, &[]);
            if let (Some(tip_id), Some(b)) = (self.active_stamp_tip, &self.active_stamps.buf) {
                if self.active_stamps.len > 0 {
                    if let Some(t) = self.tips.get(&tip_id) {
                        rpass.set_bind_group(1, &t.bind_group, &[]);
                        rpass.set_vertex_buffer(0, b.slice(..));
                        rpass.draw(0..self.active_stamps.len, 0..1);
                    }
                }
            }

            // Quitar el recorte antes de la UI (egui usa su propio scissor por elemento).
            if self.content_clip.is_some() {
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            }

            // --- CARTAS de la biblioteca (Home): bajo la UI de egui (titulo/nombres) ---
            if self.card_inst.len > 0 {
                if let Some(b) = self.card_inst.buf.as_ref() {
                    rpass.set_pipeline(&self.card_pipeline);
                    rpass.set_bind_group(0, &self.card_view_bg, &[]);
                    rpass.set_vertex_buffer(0, b.slice(..));
                    rpass.draw(0..6, 0..self.card_inst.len);
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
