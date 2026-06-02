# Ink — app de notas con tinta (multiplataforma)

App de notas y dibujo a mano alzada que combina lo mejor de **GoodNotes** (libretas),
**Excalidraw** (lienzo infinito para brainstorm) y **Concepts** (pinceles con textura,
rueda de herramientas, color de todo el espectro). Objetivo: **gratis, ilimitada,
super optimizada**, en **Windows + Android + iPad**, con escritura fluida a **alto FPS y
baja latencia**.

## Estado actual — v0.2 (motor de tinta + panel, Windows)

Funcionando y probado en Windows (NVIDIA RTX 3050, backend Vulkan, >2000 FPS):

- Lienzo **infinito** con pan y zoom (zoom hacia el cursor).
- Trazos suaves con **One-Euro filter** (anti-temblor, baja latencia).
- **Ancho variable por presion** (presion real si el lapiz la envia; si no, pseudo-presion por velocidad).
- Teselado a triangulos con uniones y puntas redondeadas; **MSAA 4x** para bordes suaves.
- Render por GPU con **wgpu** y present mode de **baja latencia** (Mailbox / Immediate / Fifo).
- **Panel de herramientas estilo Concepts** (egui), flotante y **ocultable** para no estorbar:
  - **Selector de color de TODO el espectro** (rueda HSV + RGB/HSL).
  - Colores rapidos, grosor, deshacer / rehacer / limpiar y stats en vivo (FPS, trazos).

## Arquitectura

```
App escritura/
|- crates/
|  |- ink-core/   # NUCLEO portable (sin GPU ni plataforma): se reusa en las 3 plataformas
|  |  |- stroke.rs      # modelo de trazos + teselado de ancho variable
|  |  |- smoothing.rs   # filtro One-Euro (anti-jitter, baja latencia)
|  |  |- camera.rs      # camara del lienzo infinito (pan/zoom)
|  |  |- document.rs    # documento: trazos + malla horneada
|  |- ink-app/    # SHELL de escritorio (Windows): winit + wgpu + entrada del lapiz
|     |- main.rs       # ventana, captura de lapiz/raton, atajos
|     |- renderer.rs   # estado wgpu, pipeline, buffers, MSAA
|     |- shader.wgsl   # shader de trazos
```

**Filosofia "nativo por plataforma" sin reescribir 3 veces:** toda la logica de tinta vive
en `ink-core` (Rust puro). Cada plataforma solo aporta un cascaron fino para **capturar el
lapiz con minima latencia** y **presentar** via wgpu (que mapea a DX12 / Metal / Vulkan):

| Plataforma | Captura de lapiz | Presentado |
|---|---|---|
| Windows | Win32 `WM_POINTER` (presion/inclinacion) + `DelegatedInk` | wgpu → DX12/Vulkan |
| Android | `MotionEvent` + prediccion + `CanvasFrontBufferedRenderer` | wgpu → Vulkan |
| iPad/iOS | `UITouch` coalesced/predicted + Apple Pencil | wgpu → Metal |

## Compilar y ejecutar

Requiere Rust (toolchain `stable-msvc`) y MSVC Build Tools.

```powershell
# Debug (rapido de compilar)
cargo run -p ink-app

# Release (maximo rendimiento / FPS reales) -- recomendado para probar fluidez
cargo run -p ink-app --release

# Tests del nucleo
cargo test -p ink-core
```

### Controles

| Accion | Control |
|---|---|
| Dibujar | Boton izquierdo (o lapiz/tactil) |
| Mover lienzo (pan) | Boton central, o Espacio + arrastrar |
| Zoom (al cursor) | Rueda del raton |
| Color | Teclas `1`..`8`, o el selector del panel (espectro completo) |
| Grosor | `[` y `]`, o el slider del panel |
| Deshacer / Rehacer | `Z` / `Y` |
| Limpiar | `C` |
| Ocultar / mostrar panel | Boton "Ocultar panel ▶" / "☰ Herramientas" |
| Cambiar present mode | `V` |
| Salir | `Esc` |

El titulo de la ventana muestra FPS, numero de trazos, vertices, zoom y present mode.

## Roadmap

1. **Tinta base** — *(hecho en v0.1)*: One-Euro, ancho variable, MSAA, pan/zoom infinito.
2. **Lapiz real (Windows)** — `WM_POINTER` para presion/inclinacion/azimut reales,
   prediccion de trazo y `DelegatedInk` para el "trazo humedo" de minima latencia.
3. **Pinceles con textura** — estampado de sello con textura en la GPU; tipos pluma,
   lapiz, marcador, acuarela. (Lo que hace Concepts.)
4. **Lienzo infinito optimizado** — indice espacial por mosaicos (tiles) + cache de
   textura por mosaico + culling, para millones de trazos sin caer FPS.
5. **UI estilo Concepts** — *(panel ocultable + color de espectro completo: hecho en v0.2)*.
   Falta: rueda radial de herramientas al estilo exacto de Concepts, y **capas**.
6. **Documentos** — libretas infinitas, paginas, guardar/cargar (formato binario rapido,
   local-first), undo/redo robusto.
7. **Portar shells** — Android (NDK) e iPad (Metal + Apple Pencil) reusando `ink-core` por FFI.
8. **Extras** — exportar PDF/PNG, sincronizacion opcional.
