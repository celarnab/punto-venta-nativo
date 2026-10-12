// Sin ventana de consola en Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Punto de venta de escritorio: abre la base de datos del negocio y la interfaz.

#[cfg(feature = "captura")]
mod captura;
mod configuracion;
mod controlador;
mod estado;
mod imprimir;
mod respaldos;
mod vista;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use nucleo::Bd;

slint::include_modules!();

/// Dónde vive la base de datos: la carpeta de datos de la app en cada sistema
/// (p. ej. `~/.local/share/punto-venta`, `%APPDATA%\punto-venta`, `~/Library/Application Support/…`).
fn carpeta_de_datos() -> PathBuf {
    directories::ProjectDirs::from("mx", "negocio", "punto-venta")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("datos"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "captura")]
    if let Some(carpeta) = std::env::args().skip_while(|a| a != "--captura").nth(1) {
        return captura::ejecutar(carpeta.into());
    }
    let carpeta = carpeta_de_datos();
    std::fs::create_dir_all(&carpeta)?;
    let bd = Bd::abrir(carpeta.join("punto-venta.sqlite3"))?;
    let estado = estado::Estado::new(bd, carpeta);
    controlador::respaldo_diario(&estado);

    let app = App::new()?;
    controlador::conectar(&app, Rc::new(RefCell::new(estado)));
    app.run()?;
    Ok(())
}
