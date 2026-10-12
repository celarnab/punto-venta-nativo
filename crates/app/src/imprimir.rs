//! Mandar algo a imprimir: a la térmica si está configurada; si no, como PDF que se abre para imprimir.

use std::path::Path;

use impresion::escpos::{self, Opciones};
use impresion::{Documento, pdf, sistema};

use crate::configuracion::Configuracion;

/// `cajon`: abrir el cajón de dinero (si así está configurado).
pub fn documento(
    configuracion: &Configuracion,
    carpeta: &Path,
    nombre: &str,
    doc: &Documento,
    cajon: bool,
) -> Result<(), String> {
    match &configuracion.impresora {
        Some(impresora) => {
            let opciones = Opciones {
                columnas: configuracion.columnas,
                abrir_cajon: cajon && configuracion.abrir_cajon,
                cortar: true,
            };
            sistema::imprimir_crudo(impresora, &escpos::codificar(doc, opciones))
        }
        None => abrir_pdf(configuracion, carpeta, nombre, &pdf::rollo(doc, configuracion.columnas)),
    }
}

/// Guarda el PDF en la carpeta de impresiones y lo abre con el visor del sistema.
pub fn abrir_pdf(configuracion: &Configuracion, carpeta: &Path, nombre: &str, bytes: &[u8]) -> Result<(), String> {
    let carpeta = carpeta.join("impresiones");
    std::fs::create_dir_all(&carpeta).map_err(|e| e.to_string())?;
    let ruta = carpeta.join(format!("{nombre}.pdf"));
    std::fs::write(&ruta, bytes).map_err(|e| format!("No se pudo guardar {}: {e}", ruta.display()))?;
    if configuracion.sin_ventanas { Ok(()) } else { sistema::abrir(&ruta) }
}
