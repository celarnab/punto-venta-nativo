use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use nucleo::Bd;

const PREFIJO: &str = "punto-venta-";
const CONSERVAR: usize = 30;

/// Un respaldo por día en `carpeta`, conservando los últimos 30.
pub fn diario(bd: &Bd, carpeta: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(carpeta)?;
    let hoy = carpeta.join(format!("{PREFIJO}{}.sqlite3", chrono::Local::now().format("%Y-%m-%d")));
    if !hoy.exists() {
        bd.respaldar(&hoy)?;
    }

    let mut respaldos: Vec<PathBuf> = fs::read_dir(carpeta)?
        .filter_map(|entrada| Some(entrada.ok()?.path()))
        .filter(|ruta| ruta.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PREFIJO)))
        .collect();
    respaldos.sort(); // el nombre lleva la fecha: orden alfabético = cronológico
    for viejo in respaldos.iter().rev().skip(CONSERVAR) {
        fs::remove_file(viejo)?;
    }
    Ok(())
}
