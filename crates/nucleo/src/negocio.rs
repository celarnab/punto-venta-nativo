//! Los datos del negocio que aparecen en el ticket, la tasa de IVA y el tipo de negocio.

use rusqlite::{Row, named_params};
use serde::{Deserialize, Serialize};

use crate::{Bd, Error, Porcentaje, Resultado};

enum_texto! {
    /// El tipo de negocio. Una farmacia maneja lotes con caducidad; los demás venden como siempre.
    pub enum Giro {
        Abarrotes = "abarrotes", "Tienda de abarrotes";
        Ferreteria = "ferreteria", "Ferretería";
        Farmacia = "farmacia", "Farmacia";
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Negocio {
    pub nombre: String,
    pub direccion: String,
    pub telefono: String,
    pub rfc: String,
    pub tasa_iva: Porcentaje,
    pub pie_ticket: String,
    pub giro: Giro,
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
            giro: fila.get("giro")?,
        })
    }
}

/// ¿Maneja lotes y caducidades?
pub(crate) fn es_farmacia(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    conn.query_row("SELECT giro = 'farmacia' FROM negocio", [], |fila| fila.get(0))
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
                                rfc = :rfc, tasa_iva = :tasa_iva, pie_ticket = :pie_ticket, giro = :giro",
            named_params! {
                ":nombre": negocio.nombre.trim(),
                ":direccion": negocio.direccion.trim(),
                ":telefono": negocio.telefono.trim(),
                ":rfc": negocio.rfc.trim(),
                ":tasa_iva": negocio.tasa_iva,
                ":pie_ticket": negocio.pie_ticket.trim(),
                ":giro": negocio.giro,
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
        assert_eq!((negocio.tasa_iva, negocio.giro), (Porcentaje::entero(16), Giro::Abarrotes));

        negocio.nombre = "  Ferretería La Tuerca ".into();
        negocio.giro = Giro::Ferreteria;
        let guardado = bd.guardar_negocio(&negocio).unwrap();
        assert_eq!((guardado.nombre.as_str(), guardado.giro), ("Ferretería La Tuerca", Giro::Ferreteria));
        assert!(!es_farmacia(bd.conn()).unwrap());
        negocio.giro = Giro::Farmacia;
        bd.guardar_negocio(&negocio).unwrap();
        assert!(es_farmacia(bd.conn()).unwrap());

        negocio.tasa_iva = Porcentaje::entero(101);
        assert!(bd.guardar_negocio(&negocio).is_err());
    }
}
