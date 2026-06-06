// Incrusta el icono de la app en el .exe de Windows (lo que ve Explorer y la barra de tareas).
// En otras plataformas no hace nada.
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        let _ = res.compile();
    }
}
