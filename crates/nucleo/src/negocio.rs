//! Los datos del negocio que aparecen en el ticket, y la tasa de IVA.

use rusqlite::{Row, named_params};
use serde::{Deserialize, Serialize};

use crate::{Bd, Error, Porcentaje, Resultado};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Negocio {
    pub nombre: String,
    pub direccion: String,
    pub telefono: String,
    pub rfc: String,
    pub tasa_iva: Porcentaje,
    pub pie_ticket: String,
}

impl Negocio {
    fn de_fila(fila: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            nombre: fila.get("nombre")?,
            direccion: fila.get("direccion")?,
            telefono: fila.get("telefono")?,
            rfc: fila.get("rfc")?,
            tasa_iva: fila.get("tasa_iva")?,
            pie_ticket: fila.get("pie_ticket")?,
        })
    }
}

pub(crate) fn tasa_iva(conn: &rusqlite::Connection) -> rusqlite::Result<Porcentaje> {
    conn.query_row("SELECT tasa_iva FROM negocio", [], |fila| fila.get(0))
}

impl Bd {
    pub fn negocio(&self) -> Resultado<Negocio> {
        Ok(self.conn().query_row("SELECT * FROM negocio", [], Negocio::de_fila)?)
    }

    pub fn guardar_negocio(&mut self, negocio: &Negocio) -> Resultado<Negocio> {
        if negocio.nombre.trim().is_empty() {
            return Err(Error::invalido("El nombre del negocio no puede quedar vacío."));
        }
        if !(Porcentaje::CERO..=Porcentaje::entero(100)).contains(&negocio.tasa_iva) {
            return Err(Error::invalido("La tasa de IVA debe estar entre 0 y 100 %."));
        }
        self.conn().execute(
            "UPDATE negocio SET nombre = :nombre, direccion = :direccion, telefono = :telefono,
                                rfc = :rfc, tasa_iva = :tasa_iva, pie_ticket = :pie_ticket",
            named_params! {
                ":nombre": negocio.nombre.trim(),
                ":direccion": negocio.direccion.trim(),
                ":telefono": negocio.telefono.trim(),
                ":rfc": negocio.rfc.trim(),
                ":tasa_iva": negocio.tasa_iva,
                ":pie_ticket": negocio.pie_ticket.trim(),
            },
        )?;
        self.negocio()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarda_y_valida() {
        let mut bd = Bd::en_memoria().unwrap();
        let mut negocio = bd.negocio().unwrap();
        assert_eq!(negocio.tasa_iva, Porcentaje::entero(16));

        negocio.nombre = "  Ferretería La Tuerca ".into();
        assert_eq!(bd.guardar_negocio(&negocio).unwrap().nombre, "Ferretería La Tuerca");

        negocio.tasa_iva = Porcentaje::entero(101);
        assert!(bd.guardar_negocio(&negocio).is_err());
    }
}
