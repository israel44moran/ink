# Texturas de material

Fotos de materiales **CC0 (dominio público)** descargadas de [Poly Haven](https://polyhaven.com)
(licencia CC0 — no requiere atribución; se acredita por cortesía). 1K JPG, mapa de color (albedo).

| Archivo | Material | Asset Poly Haven |
|---|---|---|
| cuero.jpg | Cuero marrón | `brown_leather` |
| madera.jpg | Madera (roble) | `oak_veneer_01` |
| lino.jpg | Tela / lino | `fabric_pattern_05` |
| denim.jpg | Denim | `denim_fabric` |
| lana.jpg | Lana bouclé | `wool_boucle` |
| cuerorojo.jpg | Cuero rojo | `leather_red_02` |
| libro.jpg | Tela de libro | `book_pattern` |
| plywood.jpg | Contrachapado | `plywood` |
| azulejo.jpg | Azulejo | `blue_floor_tiles_01` |
| gema.jpg | Piedras preciosas (mármol) | `marble_01` |

Además, **Diamante** (acolchado capitoné) es PROCEDURAL (en `card.wgsl`, no es una imagen).

Se incrustan en el binario con `include_bytes!` (ver `MATERIAL_JPGS` en `renderer.rs`) y se suben a
la GPU como un array de texturas con mipmaps. Para añadir más: descarga el JPG 1K de color de un
asset CC0, ponlo aquí, añádelo a `MATERIAL_JPGS` y a `TEXTURE_NAMES` (mismo orden).
