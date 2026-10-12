//! Apertura y cierre de caja: con cuánto se empezó, qué se cobró y cuánto debe haber.
//! Hay una sola caja por computadora, compartida por todos los que venden en ella.

use chrono::NaiveDateTime;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::cobro::Metodo;
use crate::error::duplicado;
use crate::{Bd, Dinero, Error, Resultado, ahora};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Caja {
    pub id: i64,
    /// Quién la abrió.
    pub abrio: String,
    /// Quién hizo el corte final.
    pub cerro: Option<String>,
    pub abierta_en: NaiveDateTime,
    pub cerrada_en: Option<NaiveDateTime>,
    pub fondo_inicial: Dinero,
    pub efectivo_contado: Option<Dinero>,
}

impl Caja {
    fn de_fila(fila: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: fila.get("id")?,
            abrio: fila.get("abrio")?,
            cerro: fila.get("cerro")?,
            abierta_en: fila.get("abierta_en")?,
            cerrada_en: fila.get("cerrada_en")?,
            fondo_inicial: fila.get("fondo_inicial")?,
            efectivo_contado: fila.get("efectivo_contado")?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PorMetodo {
    pub metodo: Metodo,
    pub monto: Dinero,
}

/// Lo que vendió cada quien en la caja.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PorCajero {
    pub cajero: String,
    pub ventas: i64,
    pub total: Dinero,
}

/// El corte: lo que se vendió en la caja y cuánto efectivo debería haber en el cajón.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Corte {
    pub caja: Caja,
    pub ventas: i64,
    pub total: Dinero,
    pub por_metodo: Vec<PorMetodo>,
    pub por_cajero: Vec<PorCajero>,
    /// Lo que se regresó a clientes por devoluciones, por forma de reembolso.
    pub devoluciones: Vec<PorMetodo>,
    pub efectivo_esperado: Dinero,
    /// Contado menos esperado: negativo si falta dinero.
    pub diferencia: Option<Dinero>,
}

impl Bd {
    pub fn caja_abierta(&self) -> Resultado<Option<Caja>> {
        abierta(self.conn())?.map(|id| cargar(self.conn(), id)).transpose()
    }

    /// `usuario_id`: quien cuenta el fondo y abre.
    pub fn abrir_caja(&mut self, usuario_id: i64, fondo_inicial: Dinero) -> Resultado<Caja> {
        if fondo_inicial < Dinero::CERO {
            return Err(Error::invalido("El fondo inicial no puede ser negativo."));
        }
        self.conn()
            .execute(
                "INSERT INTO cajas (abrio_id, abierta_en, fondo_inicial) VALUES (?1, ?2, ?3)",
                params![usuario_id, ahora(), fondo_inicial],
            )
            .map_err(duplicado(|| "La caja ya está abierta.".into()))?;
        cargar(self.conn(), self.conn().last_insert_rowid())
    }

    pub fn corte(&self, caja_id: i64) -> Resultado<Corte> {
        corte(self.conn(), caja_id)
    }

    /// `usuario_id`: quien cuenta el efectivo y hace el corte.
    pub fn cerrar_caja(&mut self, caja_id: i64, usuario_id: i64, efectivo_contado: Dinero) -> Resultado<Corte> {
        let cerradas = self.conn().execute(
            "UPDATE cajas SET cerrada_en = ?2, efectivo_contado = ?3, cerro_id = ?4 WHERE id = ?1 AND cerrada_en IS NULL",
            params![caja_id, ahora(), efectivo_contado, usuario_id],
        )?;
        if cerradas == 0 {
            return Err(Error::invalido("La caja ya estaba cerrada."));
        }
        self.corte(caja_id)
    }

    /// Las cajas más recientes, para consultar cortes anteriores.
    pub fn cajas(&self, limite: usize) -> Resultado<Vec<Caja>> {
        let mut sentencia = self.conn().prepare(&format!("{SELECCION} ORDER BY c.abierta_en DESC LIMIT ?1"))?;
        let cajas = sentencia.query_map([limite as i64], Caja::de_fila)?.collect::<rusqlite::Result<_>>()?;
        Ok(cajas)
    }
}

const SELECCION: &str = "
    SELECT c.*, a.nombre AS abrio, z.nombre AS cerro
    FROM cajas c JOIN usuarios a ON a.id = c.abrio_id LEFT JOIN usuarios z ON z.id = c.cerro_id";

/// La caja abierta de esta computadora, si hay.
pub(crate) fn abierta(conn: &Connection) -> Resultado<Option<i64>> {
    Ok(conn.query_row("SELECT id FROM cajas WHERE cerrada_en IS NULL", [], |f| f.get(0)).optional()?)
}

fn cargar(conn: &Connection, id: i64) -> Resultado<Caja> {
    conn.query_row(&format!("{SELECCION} WHERE c.id = ?1"), [id], Caja::de_fila)
        .optional()?
        .ok_or(Error::NoEncontrado("Caja"))
}

fn corte(conn: &Connection, caja_id: i64) -> Resultado<Corte> {
    let caja = cargar(conn, caja_id)?;
    let (ventas, total) = conn.query_row(
        "SELECT count(*), coalesce(sum(total), 0) FROM ventas WHERE caja_id = ?1 AND estado = 'pagada'",
        [caja_id],
        |f| Ok((f.get(0)?, f.get(1)?)),
    )?;
    let por_metodo = montos_por_metodo(
        conn,
        "SELECT pg.metodo, sum(pg.monto) FROM pagos pg JOIN ventas v ON v.id = pg.venta_id
         WHERE v.caja_id = ?1 AND v.estado = 'pagada'
         GROUP BY pg.metodo ORDER BY sum(pg.monto) DESC",
        caja_id,
    )?;
    let mut sentencia = conn.prepare(
        "SELECT u.nombre, count(*), sum(v.total) FROM ventas v JOIN usuarios u ON u.id = v.cajero_id
         WHERE v.caja_id = ?1 AND v.estado = 'pagada'
         GROUP BY v.cajero_id ORDER BY sum(v.total) DESC",
    )?;
    let por_cajero = sentencia
        .query_map([caja_id], |f| Ok(PorCajero { cajero: f.get(0)?, ventas: f.get(1)?, total: f.get(2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    let devoluciones = montos_por_metodo(
        conn,
        "SELECT metodo, sum(reembolso) FROM devoluciones WHERE caja_id = ?1 GROUP BY metodo ORDER BY sum(reembolso) DESC",
        caja_id,
    )?;

    let efectivo = |montos: &[PorMetodo]| -> Dinero {
        montos.iter().filter(|p| p.metodo == Metodo::Efectivo).map(|p| p.monto).sum()
    };
    let efectivo_esperado = caja.fondo_inicial + efectivo(&por_metodo) - efectivo(&devoluciones);
    let diferencia = caja.efectivo_contado.map(|contado| contado - efectivo_esperado);
    Ok(Corte { caja, ventas, total, por_metodo, por_cajero, devoluciones, efectivo_esperado, diferencia })
}

fn montos_por_metodo(conn: &Connection, sql: &str, caja_id: i64) -> Resultado<Vec<PorMetodo>> {
    let mut sentencia = conn.prepare(sql)?;
    let montos = sentencia
        .query_map([caja_id], |f| Ok(PorMetodo { metodo: f.get(0)?, monto: f.get(1)? }))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(montos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cantidad;
    use crate::catalogo::tests::d;
    use crate::ventas::tests::Tienda;

    #[test]
    fn una_sola_caja_para_todos() {
        let mut t = Tienda::nueva(); // Carla abrió la caja con 500
        let admin = t.bd.usuarios().unwrap().into_iter().find(|u| u.es_admin()).unwrap().id;
        assert_eq!(t.bd.abrir_caja(admin, d("100")).unwrap_err().to_string(), "La caja ya está abierta.");

        t.vender(&[(&t.martillo.clone(), "1")]); // Carla: 116
        let id = t.bd.venta_en_curso(admin).unwrap().id; // el administrador vende en la misma caja
        t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        t.bd.agregar(id, t.tornillo.id, Cantidad::UNO, None).unwrap();
        t.bd.cobrar(id, &Tienda::efectivo("117"), None).unwrap();

        let caja = t.bd.caja_abierta().unwrap().unwrap();
        assert_eq!(caja.abrio, "Carla");
        let corte = t.bd.cerrar_caja(caja.id, admin, d("733")).unwrap();
        assert_eq!((corte.ventas, corte.efectivo_esperado, corte.diferencia), (2, d("733"), Some(Dinero::CERO)));
        let por_cajero: Vec<_> = corte.por_cajero.iter().map(|p| (p.cajero.as_str(), p.ventas, p.total)).collect();
        assert_eq!(por_cajero, [("Admin", 1, d("117")), ("Carla", 1, d("116"))]);
        assert_eq!(corte.caja.cerro.as_deref(), Some("Admin"));

        assert!(t.bd.caja_abierta().unwrap().is_none());
        assert!(t.bd.abrir_caja(admin, d("0")).is_ok()); // al otro día, se abre otra
    }
}
