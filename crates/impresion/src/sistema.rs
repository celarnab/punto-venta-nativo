//! Las impresoras de la computadora: cuáles hay y cómo mandarles bytes tal cual (ESC/POS), sin que el
//! controlador los convierta. En Linux y macOS se usa CUPS (`lpstat`, `lp`); en Windows, la cola de
//! impresión (winspool) con el tipo de datos «RAW».

use std::path::Path;

/// Abre un archivo (p. ej. un PDF) con el programa que el sistema tenga para él.
pub fn abrir(ruta: &Path) -> Result<(), String> {
    open::that_detached(ruta).map_err(|e| format!("No se pudo abrir {}: {e}", ruta.display()))
}

pub use plataforma::{impresoras, imprimir_crudo};

#[cfg(unix)]
mod plataforma {
    use std::io::Write;
    use std::process::{Command, Stdio};

    pub fn impresoras() -> Vec<String> {
        let Ok(salida) = Command::new("lpstat").arg("-e").output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&salida.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }

    pub fn imprimir_crudo(impresora: &str, datos: &[u8]) -> Result<(), String> {
        let mut lp = Command::new("lp")
            .args(["-d", impresora, "-o", "raw"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("No se pudo usar la impresora: {e}"))?;
        lp.stdin.take().expect("stdin entubado").write_all(datos).map_err(|e| e.to_string())?;
        let salida = lp.wait_with_output().map_err(|e| e.to_string())?;
        if salida.status.success() {
            Ok(())
        } else {
            Err(format!("La impresora respondió: {}", String::from_utf8_lossy(&salida.stderr).trim()))
        }
    }
}

#[cfg(windows)]
mod plataforma {
    use std::ptr;

    use windows_sys::Win32::Graphics::Printing::{
        ClosePrinter, DOC_INFO_1W, EndDocPrinter, EndPagePrinter, EnumPrintersW, OpenPrinterW,
        PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W, StartDocPrinterW,
        StartPagePrinter, WritePrinter,
    };

    fn ancho(texto: &str) -> Vec<u16> {
        texto.encode_utf16().chain([0]).collect()
    }

    pub fn impresoras() -> Vec<String> {
        let banderas = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let (mut necesarios, mut cuantas) = (0u32, 0u32);
        // SAFETY: primera llamada solo para saber el tamaño del búfer; la segunda llena uno de ese tamaño.
        unsafe {
            EnumPrintersW(banderas, ptr::null(), 4, ptr::null_mut(), 0, &mut necesarios, &mut cuantas);
            if necesarios == 0 {
                return Vec::new();
            }
            let mut bufer = vec![0u8; necesarios as usize];
            if EnumPrintersW(banderas, ptr::null(), 4, bufer.as_mut_ptr(), necesarios, &mut necesarios, &mut cuantas)
                == 0
            {
                return Vec::new();
            }
            let info = std::slice::from_raw_parts(bufer.as_ptr() as *const PRINTER_INFO_4W, cuantas as usize);
            info.iter()
                .map(|i| {
                    let largo = (0..).take_while(|&n| *i.pPrinterName.add(n) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(i.pPrinterName, largo))
                })
                .collect()
        }
    }

    pub fn imprimir_crudo(impresora: &str, datos: &[u8]) -> Result<(), String> {
        let nombre = ancho(impresora);
        let mut documento = ancho("Ticket");
        let mut tipo = ancho("RAW");
        let info = DOC_INFO_1W {
            pDocName: documento.as_mut_ptr(),
            pOutputFile: ptr::null_mut(),
            pDatatype: tipo.as_mut_ptr(),
        };
        let mut manija = PRINTER_HANDLE::default();
        // SAFETY: cadenas terminadas en cero que viven durante toda la llamada; la manija se cierra al final.
        unsafe {
            if OpenPrinterW(nombre.as_ptr(), &mut manija, ptr::null()) == 0 {
                return Err(format!("No se encontró la impresora «{impresora}»."));
            }
            let mut escritos = 0u32;
            let bien = StartDocPrinterW(manija, 1, &info) != 0
                && StartPagePrinter(manija) != 0
                && WritePrinter(manija, datos.as_ptr().cast(), datos.len() as u32, &mut escritos) != 0
                && EndPagePrinter(manija) != 0
                && EndDocPrinter(manija) != 0;
            ClosePrinter(manija);
            if bien && escritos as usize == datos.len() {
                Ok(())
            } else {
                Err(format!("La impresora «{impresora}» no aceptó el trabajo."))
            }
        }
    }
}
