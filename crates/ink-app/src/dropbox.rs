//! Sincronizacion de los cuadernos con Dropbox.
//!
//! La app esta registrada como "App folder" (Scoped App): solo ve su propia carpeta
//! `Aplicaciones/Ink-Notas-Israel` en el Dropbox del usuario, nada mas.
//!
//! Login: OAuth 2.0 con PKCE en flujo de "codigo manual" (sin servidor local): Ink abre el
//! navegador, el usuario autoriza, Dropbox le muestra un codigo que pega en Ink, e Ink lo
//! cambia por un refresh_token de larga duracion. No se usa App secret (PKCE no lo necesita).

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

/// Clave PUBLICA de la app en Dropbox (va dentro del programa; con PKCE no hay secreto que ocultar).
const APP_KEY: &str = "g16l9u6q2twqgf8";

const AUTH_URL: &str = "https://www.dropbox.com/oauth2/authorize";
const TOKEN_URL: &str = "https://api.dropboxapi.com/oauth2/token";
const ACCOUNT_URL: &str = "https://api.dropboxapi.com/2/users/get_current_account";
const LIST_URL: &str = "https://api.dropboxapi.com/2/files/list_folder";
const UPLOAD_URL: &str = "https://content.dropboxapi.com/2/files/upload";
const DOWNLOAD_URL: &str = "https://content.dropboxapi.com/2/files/download";

/// Credenciales guardadas en disco para no re-logear cada vez (solo el refresh_token; el
/// access_token se renueva al vuelo en cada sincronizacion).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Creds {
    pub refresh_token: String,
    /// Correo o nombre de la cuenta, solo para mostrar "Conectado como ...".
    #[serde(default)]
    pub cuenta: String,
}

// --------------------------- Almacenamiento del token ---------------------------

fn creds_path() -> PathBuf {
    crate::notebook::notebooks_dir().join("_dropbox.json")
}

/// Carga las credenciales guardadas (None si no hay sesion).
pub fn load_creds() -> Option<Creds> {
    let txt = std::fs::read_to_string(creds_path()).ok()?;
    let c: Creds = serde_json::from_str(&txt).ok()?;
    if c.refresh_token.is_empty() {
        None
    } else {
        Some(c)
    }
}

/// Guarda las credenciales (el refresh_token). NOTA: por ahora en texto plano; mas adelante se
/// puede cifrar con el almacen de credenciales de Windows (DPAPI).
pub fn save_creds(c: &Creds) {
    let _ = crate::notebook::ensure_dir();
    if let Ok(s) = serde_json::to_string(c) {
        let _ = std::fs::write(creds_path(), s);
    }
}

/// Cierra la sesion: borra el token guardado.
pub fn forget() {
    let _ = std::fs::remove_file(creds_path());
}

// --------------------------- Utilidades ---------------------------

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn sha256(data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Convierte el Result de ureq en Result<_, String>, leyendo el mensaje de error de Dropbox.
fn check(r: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, String> {
    match r {
        Ok(resp) => Ok(resp),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let body: String = body.chars().take(400).collect();
            Err(format!("Dropbox respondio HTTP {code}: {body}"))
        }
        Err(e) => Err(format!("Error de red: {e}")),
    }
}

fn json_of(resp: ureq::Response) -> Result<serde_json::Value, String> {
    let body = resp.into_string().map_err(|e| format!("no se pudo leer la respuesta: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("respuesta JSON invalida: {e}"))
}

/// Construye el valor del header `Dropbox-API-Arg` para una ruta `/{nombre}`. El header DEBE ser
/// ASCII puro: los caracteres no-ASCII (acentos, n con virgulilla...) van escapados como \uXXXX.
fn arg_path(nombre: &str) -> String {
    let mut s = String::from("/");
    for c in nombre.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let cp = c as u32;
                if cp > 0xFFFF {
                    let v = cp - 0x10000;
                    s.push_str(&format!("\\u{:04x}\\u{:04x}", 0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)));
                } else {
                    s.push_str(&format!("\\u{:04x}", cp));
                }
            }
            c => s.push(c),
        }
    }
    s
}

// --------------------------- Login (OAuth PKCE) ---------------------------

/// Devuelve (url_de_autorizacion, code_verifier). El usuario abre la URL, autoriza, y Dropbox le
/// muestra un codigo. Hay que conservar el code_verifier hasta que el usuario pegue ese codigo.
pub fn auth_url() -> (String, String) {
    let mut rnd = [0u8; 32];
    let _ = getrandom::getrandom(&mut rnd);
    let verifier = b64url(&rnd);
    let challenge = b64url(&sha256(verifier.as_bytes()));
    let url = format!(
        "{AUTH_URL}?client_id={APP_KEY}&response_type=code&code_challenge={challenge}\
         &code_challenge_method=S256&token_access_type=offline"
    );
    (url, verifier)
}

/// Cambia el codigo de autorizacion (pegado por el usuario) por las credenciales y las guarda.
pub fn connect(code: &str, verifier: &str) -> Result<Creds, String> {
    let resp = check(ureq::post(TOKEN_URL).send_form(&[
        ("code", code.trim()),
        ("grant_type", "authorization_code"),
        ("client_id", APP_KEY),
        ("code_verifier", verifier),
    ]))?;
    let j = json_of(resp)?;
    let refresh = j["refresh_token"].as_str().unwrap_or_default().to_string();
    if refresh.is_empty() {
        return Err("Dropbox no devolvio un refresh_token (revisa el codigo).".into());
    }
    let access = j["access_token"].as_str().unwrap_or_default().to_string();
    let cuenta = account_name(&access).unwrap_or_default();
    let creds = Creds { refresh_token: refresh, cuenta };
    save_creds(&creds);
    Ok(creds)
}

/// Renueva el access_token (corto, ~4 h) a partir del refresh_token (largo).
pub fn access_token(creds: &Creds) -> Result<String, String> {
    let resp = check(ureq::post(TOKEN_URL).send_form(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", creds.refresh_token.as_str()),
        ("client_id", APP_KEY),
    ]))?;
    let j = json_of(resp)?;
    j["access_token"].as_str().map(str::to_string).ok_or_else(|| "Dropbox no devolvio access_token".into())
}

/// Correo (o nombre) de la cuenta conectada, para mostrarlo en la UI.
fn account_name(access: &str) -> Option<String> {
    let resp = check(
        ureq::post(ACCOUNT_URL)
            .set("Authorization", &format!("Bearer {access}"))
            .call(),
    )
    .ok()?;
    let j = json_of(resp).ok()?;
    let email = j["email"].as_str().unwrap_or("");
    let name = j["name"]["display_name"].as_str().unwrap_or("");
    Some(if !email.is_empty() { email.to_string() } else { name.to_string() })
}

// --------------------------- API de archivos ---------------------------

/// Un archivo en la carpeta de la app en Dropbox.
pub struct RemoteFile {
    pub name: String,
    /// Hash propio de Dropbox; sirve para saber si cambio sin tener que descargarlo.
    pub content_hash: String,
}

/// Lista los archivos de la carpeta de la app.
pub fn list(access: &str) -> Result<Vec<RemoteFile>, String> {
    let resp = check(
        ureq::post(LIST_URL)
            .set("Authorization", &format!("Bearer {access}"))
            .set("Content-Type", "application/json")
            .send_string("{\"path\":\"\",\"recursive\":false}"),
    )?;
    let j = json_of(resp)?;
    let mut out = Vec::new();
    if let Some(entries) = j["entries"].as_array() {
        for e in entries {
            if e[".tag"].as_str() == Some("file") {
                out.push(RemoteFile {
                    name: e["name"].as_str().unwrap_or_default().to_string(),
                    content_hash: e["content_hash"].as_str().unwrap_or_default().to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// Sube `datos` a `/{nombre}` (sobrescribe).
pub fn upload(access: &str, nombre: &str, datos: &[u8]) -> Result<(), String> {
    let arg = format!("{{\"path\":\"{}\",\"mode\":\"overwrite\",\"mute\":true}}", arg_path(nombre));
    check(
        ureq::post(UPLOAD_URL)
            .set("Authorization", &format!("Bearer {access}"))
            .set("Dropbox-API-Arg", &arg)
            .set("Content-Type", "application/octet-stream")
            .send_bytes(datos),
    )?;
    Ok(())
}

/// Descarga `/{nombre}`.
pub fn download(access: &str, nombre: &str) -> Result<Vec<u8>, String> {
    let arg = format!("{{\"path\":\"{}\"}}", arg_path(nombre));
    let resp = check(
        ureq::post(DOWNLOAD_URL)
            .set("Authorization", &format!("Bearer {access}"))
            .set("Dropbox-API-Arg", &arg)
            .call(),
    )?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf).map_err(|e| format!("error al descargar: {e}"))?;
    Ok(buf)
}

/// Reproduce el `content_hash` de Dropbox para unos bytes locales (SHA256 por bloques de 4 MiB,
/// concatenados, y SHA256 final en hexadecimal). Permite comparar local vs remoto sin descargar.
pub fn content_hash(data: &[u8]) -> String {
    let mut concat = Vec::new();
    for chunk in data.chunks(4 * 1024 * 1024) {
        concat.extend_from_slice(&sha256(chunk));
    }
    hex(&sha256(&concat))
}

// --------------------------- Sincronizacion ---------------------------

use std::collections::{HashMap, HashSet};

/// Resumen de lo que hizo una sincronizacion.
#[derive(Default, Clone)]
pub struct SyncReport {
    pub subidos: usize,
    pub bajados: usize,
    /// Nombres de cuadernos con conflicto (editados en ambas PCs); se guardo una copia local.
    pub conflictos: Vec<String>,
}

/// Estado de la ultima sincronizacion: hash conocido de cada archivo. Sirve para saber QUIEN
/// cambio (local, remoto o ambos) y asi no pisar cambios ni perder datos.
fn sync_state_path() -> PathBuf {
    crate::notebook::notebooks_dir().join("_sync_state.json")
}
fn load_sync_state() -> HashMap<String, String> {
    std::fs::read_to_string(sync_state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
fn save_sync_state(m: &HashMap<String, String>) {
    if let Ok(s) = serde_json::to_string(m) {
        let _ = std::fs::write(sync_state_path(), s);
    }
}

/// Archivos locales a sincronizar: cuadernos y metadatos (orden, archiveros, tweaks), EXCEPTO los
/// internos de la sesion de Dropbox y del propio estado de sync.
fn local_files() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(crate::notebook::notebooks_dir()) {
        for e in rd.flatten() {
            if let Some(name) = e.file_name().to_str() {
                if name.ends_with(".json") && name != "_dropbox.json" && name != "_sync_state.json" {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// "MiCuaderno.json" -> "MiCuaderno (conflicto).json".
fn conflict_name(nombre: &str) -> String {
    match nombre.strip_suffix(".json") {
        Some(stem) => format!("{stem} (conflicto).json"),
        None => format!("{nombre} (conflicto)"),
    }
}

/// Sincroniza la carpeta de cuadernos con Dropbox (subir/bajar segun lo que haya cambiado en cada
/// lado). Reglas: si solo cambio un lado, gana ese; si cambiaron los dos (conflicto), se sube el
/// local y se guarda el remoto como copia "(conflicto)" para no perder nada. Por seguridad, los
/// BORRADOS no se propagan todavia (un archivo borrado en una PC se vuelve a copiar).
pub fn sync_all(creds: &Creds) -> Result<SyncReport, String> {
    let access = access_token(creds)?;
    let dir = crate::notebook::notebooks_dir();
    let mut state = load_sync_state();
    let mut report = SyncReport::default();

    let remotos: HashMap<String, String> =
        list(&access)?.into_iter().map(|f| (f.name, f.content_hash)).collect();
    let locales = local_files();

    let mut nombres: HashSet<String> = locales.into_iter().collect();
    nombres.extend(remotos.keys().cloned());

    for nombre in nombres {
        let local_path = dir.join(&nombre);
        let local_bytes = std::fs::read(&local_path).ok();
        let local_h = local_bytes.as_ref().map(|b| content_hash(b));
        let remote_h = remotos.get(&nombre).cloned();
        let base_h = state.get(&nombre).cloned();

        match (&local_h, &remote_h) {
            (Some(lh), Some(rh)) if lh == rh => {
                state.insert(nombre, lh.clone());
            }
            (Some(lh), Some(rh)) => {
                let local_cambio = base_h.as_deref() != Some(lh.as_str());
                let remote_cambio = base_h.as_deref() != Some(rh.as_str());
                if remote_cambio && !local_cambio {
                    let data = download(&access, &nombre)?;
                    std::fs::write(&local_path, &data).map_err(|e| e.to_string())?;
                    state.insert(nombre, rh.clone());
                    report.bajados += 1;
                } else if local_cambio && !remote_cambio {
                    upload(&access, &nombre, local_bytes.as_ref().unwrap())?;
                    state.insert(nombre, lh.clone());
                    report.subidos += 1;
                } else {
                    // Ambos cambiaron: el local gana, el remoto se guarda como copia local.
                    if let Ok(data) = download(&access, &nombre) {
                        let _ = std::fs::write(dir.join(conflict_name(&nombre)), &data);
                    }
                    upload(&access, &nombre, local_bytes.as_ref().unwrap())?;
                    report.conflictos.push(nombre.clone());
                    state.insert(nombre, lh.clone());
                    report.subidos += 1;
                }
            }
            (Some(lh), None) => {
                upload(&access, &nombre, local_bytes.as_ref().unwrap())?;
                state.insert(nombre, lh.clone());
                report.subidos += 1;
            }
            (None, Some(rh)) => {
                let data = download(&access, &nombre)?;
                std::fs::write(&local_path, &data).map_err(|e| e.to_string())?;
                state.insert(nombre, rh.clone());
                report.bajados += 1;
            }
            (None, None) => {}
        }
    }

    save_sync_state(&state);
    Ok(report)
}
