//! Lectura del BOTON DEL LAPIZ en Windows (igual que Photoshop).
//!
//! winit (en modo Windows Ink) entrega los botones del stylus como el boton primario del
//! puntero (clic izquierdo), asi que no se pueden distinguir de un trazo. Para detectarlos
//! de verdad, "subclasamos" la ventana (SetWindowLongPtrW) e interceptamos los mensajes
//! WM_POINTER* leyendo `POINTER_PEN_INFO.penFlags`, que indica si el boton "barrel"
//! (el de abajo) esta presionado. Detectamos el flanco de pulsacion y lo exponemos como
//! un contador que la app consume para abrir/cerrar el panel.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::Pointer::{GetPointerPenInfo, POINTER_PEN_INFO};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, SetWindowLongPtrW, GWLP_WNDPROC, PEN_FLAG_BARREL, WNDPROC,
};

const WM_POINTERUPDATE: u32 = 0x0245;
const WM_POINTERDOWN: u32 = 0x0246;
const WM_POINTERUP: u32 = 0x0247;

/// Puntero al WndProc original de winit (para encadenar).
static ORIG_WNDPROC: AtomicIsize = AtomicIsize::new(0);
/// Estado actual del boton barrel (para detectar flancos).
static BARREL_DOWN: AtomicBool = AtomicBool::new(false);
/// Flancos de pulsacion del boton barrel desde la ultima lectura.
static BARREL_CLICKS: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_POINTERUPDATE || msg == WM_POINTERDOWN || msg == WM_POINTERUP {
        let pointer_id = (wparam.0 & 0xFFFF) as u32;
        let mut info = POINTER_PEN_INFO::default();
        if unsafe { GetPointerPenInfo(pointer_id, &mut info) }.is_ok() {
            let barrel = (info.penFlags & PEN_FLAG_BARREL) != 0;
            let was = BARREL_DOWN.swap(barrel, Ordering::SeqCst);
            if barrel && !was {
                BARREL_CLICKS.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    let orig = ORIG_WNDPROC.load(Ordering::SeqCst);
    let prev: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(orig) };
    unsafe { CallWindowProcW(prev, hwnd, msg, wparam, lparam) }
}

/// Instala el subclass en la ventana (HWND como isize). Idempotente.
pub fn install(hwnd_ptr: isize) {
    if hwnd_ptr == 0 || ORIG_WNDPROC.load(Ordering::SeqCst) != 0 {
        return;
    }
    let proc_addr = subclass_proc as *const () as isize;
    unsafe {
        let hwnd = HWND(hwnd_ptr as *mut core::ffi::c_void);
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc_addr);
        ORIG_WNDPROC.store(prev, Ordering::SeqCst);
    }
}

/// Numero de pulsaciones del boton barrel desde la ultima llamada (y las resetea).
pub fn take_barrel_clicks() -> u32 {
    BARREL_CLICKS.swap(0, Ordering::SeqCst)
}
