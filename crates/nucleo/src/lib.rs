//! El corazón del punto de venta: reglas de negocio y base de datos, sin interfaz.
//!
//! Todo se hace a través de [`Bd`]; cada módulo le agrega sus operaciones.

#[macro_use]
mod macros;

pub mod caja;
pub mod catalogo;
pub mod clientes;
pub mod cobro;
pub mod cotizaciones;
mod db;
pub mod devoluciones;
pub mod dinero;
mod error;
pub mod exportacion;
pub mod importacion;
pub mod negocio;
pub mod reportes;
pub mod texto;
pub mod usuarios;
pub mod ventas;

pub use db::Bd;
pub use dinero::{Cantidad, Dinero, Porcentaje};
pub use error::{Error, Resultado};

/// Fecha y hora local en que ocurre algo: el negocio vive en su propio huso horario.
fn ahora() -> chrono::NaiveDateTime {
    chrono::Local::now().naive_local()
}
