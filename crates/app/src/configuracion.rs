//! Ajustes de esta computadora que no viajan con la base de datos (al restaurar un respaldo de otra
//! computadora, la impresora de aquí sigue siendo la de aquí).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Configuracion {
    /// La impresora térmica de tickets; `None` = los tickets salen en PDF.
    pub impresora: Option<String>,
    pub columnas: usize,
    pub abrir_cajon: bool,
    /// Imprimir el ticket en cuanto se cobra.
    pub automatico: bool,
    /// Generar los PDF sin abrirlos (para las capturas de pantalla).
    #[serde(skip)]
    pub sin_ventanas: bool,
}

impl Default for Configuracion {
    fn default() -> Self {
        Self { impresora: None, columnas: 42, abrir_cajon: false, automatico: false, sin_ventanas: false }
    }
}

impl Configuracion {
    fn ruta(carpeta: &Path) -> PathBuf {
        carpeta.join("configuracion.json")
    }

    /// Si no hay archivo (o está dañado), la de siempre.
    pub fn cargar(carpeta: &Path) -> Self {
        std::fs::read_to_string(Self::ruta(carpeta))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn guardar(&self, carpeta: &Path) -> std::io::Result<()> {
        std::fs::write(Self::ruta(carpeta), serde_json::to_string_pretty(self).expect("se serializa"))
    }
}
