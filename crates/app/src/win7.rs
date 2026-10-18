//! Lo que hace falta para abrir en Windows 7.
//!
//! Las bibliotecas de Microsoft para Rust (`windows-core`, `windows-result`) importan unas cuantas funciones de
//! `combase.dll` y de `api-ms-win-core-winrt-error-l1-1-0.dll`, que llegaron con Windows 8. Si el .exe las pidiera
//! al arrancar, Windows 7 ni lo abriría. Por eso esas dos se cargan diferidas (`/DELAYLOAD`, ver `build.rs`): solo
//! cuando se llama una de sus funciones. En Windows 8 o posterior se cargan normal; en Windows 7, el ayudante de
//! carga diferida llama a [`__pfnDliFailureHook2`], que pone en su lugar:
//!
//! - para `combase.dll`, `ole32.dll`, que en Windows 7 tiene `CoTaskMemAlloc`, `CoTaskMemFree` y
//!   `CoCreateFreeThreadedMarshaler`;
//! - para lo que Windows 7 no tiene en ningún lado (cosas de WinRT, que esta app no usa), funciones que responden
//!   «no se puede» con el mismo número de argumentos, como pide `stdcall` en 32 bits.

use std::ffi::{CStr, c_char, c_void};

/// `DelayLoadProc`, de `delayimp.h`.
#[repr(C)]
struct Funcion {
    por_nombre: i32,
    /// `szProcName` si `por_nombre`, si no el ordinal.
    nombre_u_ordinal: *const c_char,
}

/// `DelayLoadInfo`, de `delayimp.h`.
#[repr(C)]
struct Carga {
    tamano: u32,
    descriptor: *const c_void,
    destino: *mut *const c_void,
    dll: *const c_char,
    funcion: Funcion,
    modulo: *mut c_void,
    resultado: *const c_void,
    ultimo_error: u32,
}

type Gancho = unsafe extern "system" fn(aviso: u32, carga: *const Carga) -> *const c_void;

const NO_SE_CARGO_LA_DLL: u32 = 3; // dliFailLoadLib
const NO_SE_ENCONTRO_LA_FUNCION: u32 = 4; // dliFailGetProc
const E_NOTIMPL: i32 = 0x8000_4001_u32 as i32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryA(nombre: *const c_char) -> *mut c_void;
}

/// El ayudante de `delayimp.lib` lo llama cuando no puede cargar una DLL diferida o una de sus funciones.
#[unsafe(no_mangle)]
#[used]
static __pfnDliFailureHook2: Gancho = sustituir;

unsafe extern "system" fn sustituir(aviso: u32, carga: *const Carga) -> *const c_void {
    // SAFETY: el ayudante siempre pasa una `DelayLoadInfo` válida, con el nombre de la DLL y, si es por nombre, el
    // de la función.
    let carga = unsafe { &*carga };
    let dll = unsafe { CStr::from_ptr(carga.dll) }.to_bytes();
    match aviso {
        NO_SE_CARGO_LA_DLL if dll.eq_ignore_ascii_case(b"combase.dll") => {
            // SAFETY: cadena terminada en cero.
            unsafe { LoadLibraryA(c"ole32.dll".as_ptr()) }.cast_const()
        }
        // No existe en Windows 7: cualquier módulo cargado sirve para seguir; sus funciones llegan como «no
        // encontradas» y se sustituyen abajo.
        NO_SE_CARGO_LA_DLL if dll.eq_ignore_ascii_case(b"api-ms-win-core-winrt-error-l1-1-0.dll") => {
            // SAFETY: cadena terminada en cero.
            unsafe { LoadLibraryA(c"kernel32.dll".as_ptr()) }.cast_const()
        }
        NO_SE_ENCONTRO_LA_FUNCION if carga.funcion.por_nombre != 0 => {
            // SAFETY: con `por_nombre`, es el nombre de la función, terminado en cero.
            let nombre = unsafe { CStr::from_ptr(carga.funcion.nombre_u_ordinal) }.to_bytes();
            sustituta(nombre).unwrap_or(std::ptr::null())
        }
        _ => std::ptr::null(), // lo de siempre: el ayudante lanza su excepción
    }
}

/// Las funciones de Windows 8 que Windows 7 no tiene, como «no se puede».
fn sustituta(nombre: &[u8]) -> Option<*const c_void> {
    Some(match nombre {
        b"CoIncrementMTAUsage" => co_increment_mta_usage as *const c_void,
        b"RoGetAgileReference" => ro_get_agile_reference as *const c_void,
        b"RoOriginateErrorW" => ro_originate_error_w as *const c_void,
        _ => return None,
    })
}

unsafe extern "system" fn co_increment_mta_usage(_galleta: *mut *mut c_void) -> i32 {
    E_NOTIMPL
}

unsafe extern "system" fn ro_get_agile_reference(
    _opciones: i32,
    _riid: *const c_void,
    _objeto: *mut c_void,
    referencia: *mut *mut c_void,
) -> i32 {
    if !referencia.is_null() {
        // SAFETY: quien llama pasa dónde dejar el resultado; en error debe quedar en nulo.
        unsafe { *referencia = std::ptr::null_mut() };
    }
    E_NOTIMPL
}

/// Solo agrega detalle al error para un depurador: sin él, el error sigue igual.
unsafe extern "system" fn ro_originate_error_w(_error: i32, _largo: u32, _mensaje: *const u16) -> i32 {
    0
}

/// `--probar-win7`: revisa el gancho sin necesitar Windows 7 (en CI, después de compilar).
pub fn probar() -> Result<(), String> {
    let carga = |dll: &CStr, funcion: Option<&CStr>| Carga {
        tamano: size_of::<Carga>() as u32,
        descriptor: std::ptr::null(),
        destino: std::ptr::null_mut(),
        dll: dll.as_ptr(),
        funcion: Funcion {
            por_nombre: funcion.is_some() as i32,
            nombre_u_ordinal: funcion.map_or(std::ptr::null(), CStr::as_ptr),
        },
        modulo: std::ptr::null_mut(),
        resultado: std::ptr::null(),
        ultimo_error: 0,
    };
    // Por el mismo puntero que lee el ayudante.
    let gancho = __pfnDliFailureHook2;
    // SAFETY: `Carga` armada como la arma el ayudante.
    let ole32 = unsafe { gancho(NO_SE_CARGO_LA_DLL, &carga(c"combase.dll", None)) };
    if ole32.is_null() {
        return Err("combase.dll no se sustituyó por ole32.dll".into());
    }
    for nombre in [c"CoIncrementMTAUsage", c"RoGetAgileReference", c"RoOriginateErrorW"] {
        let c = carga(c"combase.dll", Some(nombre));
        // SAFETY: igual que arriba.
        if unsafe { gancho(NO_SE_ENCONTRO_LA_FUNCION, &c) }.is_null() {
            return Err(format!("sin sustituta para {nombre:?}"));
        }
    }
    let otra = carga(c"combase.dll", Some(c"CoCreateInstance"));
    // SAFETY: igual que arriba.
    if !unsafe { gancho(NO_SE_ENCONTRO_LA_FUNCION, &otra) }.is_null() {
        return Err("una función desconocida no debe sustituirse".into());
    }
    Ok(())
}
