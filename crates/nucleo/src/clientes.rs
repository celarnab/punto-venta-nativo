use rusqlite::{Connection, OptionalExtension, Row, named_params};
use serde::{Deserialize, Serialize};

use crate::texto::palabras_json;
use crate::{Bd, Error, Resultado};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Cliente {
    pub id: i64,
    #[serde(flatten)]
    pub datos: DatosCliente,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatosCliente {
    pub nombre: String,
    #[serde(default)]
    pub telefono: String,
    #[serde(default)]
    pub correo: String,
    /// RFC o identificación fiscal.
    #[serde(default)]
    pub rfc: String,
    #[serde(default)]
    pub notas: String,
}

impl Cliente {
    fn de_fila(fila: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: fila.get("id")?,
            datos: DatosCliente {
                nombre: fila.get("nombre")?,
                telefono: fila.get("telefono")?,
                correo: fila.get("correo")?,
                rfc: fila.get("rfc")?,
                notas: fila.get("notas")?,
            },
        })
    }
}

impl Bd {
    /// Por nombre, teléfono o RFC; cada palabra debe aparecer.
    pub fn buscar_clientes(&self, consulta: &str, limite: usize) -> Resultado<Vec<Cliente>> {
        let mut sentencia = self.conn().prepare(
            "SELECT * FROM clientes c
             WHERE NOT EXISTS (
                 SELECT 1 FROM json_each(:palabras) palabra
                 WHERE instr(normalizar(c.nombre || ' ' || c.telefono || ' ' || c.rfc), palabra.value) = 0
             )
             ORDER BY c.nombre LIMIT :limite",
        )?;
        let clientes = sentencia
            .query_map(
                named_params! { ":palabras": palabras_json(consulta), ":limite": limite as i64 },
                Cliente::de_fila,
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok(clientes)
    }

    pub fn cliente(&self, id: i64) -> Resultado<Cliente> {
        self.conn()
            .query_row("SELECT * FROM clientes WHERE id = ?1", [id], Cliente::de_fila)
            .optional()?
            .ok_or(Error::NoEncontrado("Cliente"))
    }

    pub fn crear_cliente(&mut self, datos: &DatosCliente) -> Resultado<Cliente> {
        guardar(
            self.conn(),
            "INSERT INTO clientes (id, nombre, telefono, correo, rfc, notas)
             VALUES (:id, :nombre, :telefono, :correo, :rfc, :notas)",
            None,
            datos,
        )?;
        self.cliente(self.conn().last_insert_rowid())
    }

    pub fn actualizar_cliente(&mut self, id: i64, datos: &DatosCliente) -> Resultado<Cliente> {
        guardar(
            self.conn(),
            "UPDATE clientes SET nombre = :nombre, telefono = :telefono, correo = :correo, rfc = :rfc, notas = :notas
             WHERE id = :id",
            Some(id),
            datos,
        )?;
        self.cliente(id)
    }
}

/// Inserta (`id` = `None`) o actualiza el cliente según `sql`.
fn guardar(conn: &Connection, sql: &str, id: Option<i64>, datos: &DatosCliente) -> Resultado<()> {
    if datos.nombre.trim().is_empty() {
        return Err(Error::invalido("Falta el nombre del cliente."));
    }
    conn.execute(
        sql,
        named_params! {
            ":id": id,
            ":nombre": datos.nombre.trim(),
            ":telefono": datos.telefono.trim(),
            ":correo": datos.correo.trim(),
            ":rfc": datos.rfc.trim(),
            ":notas": datos.notas.trim(),
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cliente(nombre: &str, telefono: &str) -> DatosCliente {
        DatosCliente { nombre: nombre.into(), telefono: telefono.into(), ..Default::default() }
    }

    #[test]
    fn busca_por_nombre_o_telefono() {
        let mut bd = Bd::en_memoria().unwrap();
        bd.crear_cliente(&cliente("José Pérez", "555-1234")).unwrap();
        bd.crear_cliente(&cliente("Construcciones Pérez", "")).unwrap();

        let encontrados =
            |q| bd.buscar_clientes(q, 10).unwrap().into_iter().map(|c| c.datos.nombre).collect::<Vec<_>>();
        assert_eq!(encontrados("perez"), ["Construcciones Pérez", "José Pérez"]);
        assert_eq!(encontrados("jose perez"), ["José Pérez"]);
        assert_eq!(encontrados("1234"), ["José Pérez"]);
        assert_eq!(encontrados("").len(), 2);
    }

    #[test]
    fn actualiza_y_valida() {
        let mut bd = Bd::en_memoria().unwrap();
        let jose = bd.crear_cliente(&cliente("José", "")).unwrap();
        let jose = bd.actualizar_cliente(jose.id, &cliente(" José Pérez ", "555")).unwrap();
        assert_eq!(jose.datos.nombre, "José Pérez");
        assert!(bd.crear_cliente(&cliente(" ", "")).is_err());
    }
}
