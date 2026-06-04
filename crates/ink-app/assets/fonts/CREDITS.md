# Fuentes

**Inter** (Inter-Regular.ttf, Inter-SemiBold.ttf) — © The Inter Project Authors.
Licencia **SIL Open Font License 1.1** (OFL). https://rsms.me/inter/ ·
https://github.com/rsms/inter — subconjunto latino (TTF estático) vía Fontsource.

Se incrustan en el binario con `include_bytes!` (ver `setup_fonts` en `main.rs`) como fuente
principal de la interfaz; el sistema (Segoe UI / DejaVu) queda como respaldo para glifos que falten.
