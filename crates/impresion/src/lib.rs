//! Todo lo que sale en papel: el ticket va directo a la impresora térmica (ESC/POS), y la cotización en
//! hoja carta (o cualquier ticket, si no hay impresora térmica) se genera como PDF.

pub mod documento;
pub mod escpos;
pub mod pdf;
pub mod sistema;

pub use documento::{Documento, corte, devolucion, prueba, ticket};
