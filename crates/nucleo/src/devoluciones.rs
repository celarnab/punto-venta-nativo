//! Devoluciones: el cliente regresa todo o parte de lo que compró. La mercancía vuelve al inventario
//! y el dinero sale de la caja abierta de quien la atiende.

use chrono::NaiveDateTime;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::catalogo::{self, TipoMovimiento};
use crate::cobro::Metodo;
use crate::negocio::Negocio;
use crate::usuarios::Autorizacion;
use crate::ventas::{self, Estado, Partida, Venta};
use crate::{Bd, Cantidad, Dinero, Error, Resultado, ahora, caja};

/// Cuánto se regresa de una partida de la venta.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct ADevolver {
    pub partida_id: i64,
    pub cantidad: Cantidad,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PartidaDevuelta {
    pub partida_id: i64,
    pub descripcion: String,
    pub unidad: String,
    pub cantidad: Cantidad,
    pub precio: Dinero,
    pub importe: Dinero,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Devolucion {
    pub id: i64,
    pub venta_id: i64,
    pub folio_venta: i64,
    pub creada: NaiveDateTime,
    pub cajero: String,
    pub autorizo: String,
    pub metodo: Metodo,
    pub motivo: String,
    /// Lo que se le regresó al cliente: el importe de lo devuelto menos su parte del descuento de la venta.
    pub reembolso: Dinero,
    pub partidas: Vec<PartidaDevuelta>,
}

/// Lo que se imprime para el cliente.
#[derive(Clone, Debug, Serialize)]
pub struct ComprobanteDevolucion {
    pub negocio: Negocio,
    pub devolucion: Devolucion,
}

impl Bd {
    /// Registra la devolución de `lineas` de una venta cobrada; un administrador la autoriza.
    pub fn devolver(
        &mut self,
        venta_id: i64,
        cajero_id: i64,
        lineas: &[ADevolver],
        metodo: Metodo,
        motivo: &str,
        autorizacion: Autorizacion,
    ) -> Resultado<Devolucion> {
        if lineas.iter().any(|l| l.cantidad < Cantidad::CERO) {
            return Err(Error::invalido("Las cantidades a devolver no pueden ser negativas."));
        }
        let lineas: Vec<ADevolver> = lineas.iter().copied().filter(|l| l.cantidad.es_positivo()).collect();
        if lineas.is_empty() {
            return Err(Error::invalido("Indica qué productos regresa el cliente."));
        }
        self.transaccion(|tx| {
            let venta = ventas::cargar(tx, venta_id)?;
            ventas::venta_en(&venta, Estado::Pagada)?;
            let caja_id = caja::abierta(tx)?.ok_or(Error::invalido("Abre la caja para hacer la devolución."))?;

            let mut devueltas = Vec::with_capacity(lineas.len());
            for linea in &lineas {
                let partida = venta
                    .partidas
                    .iter()
                    .find(|p| p.id == linea.partida_id)
                    .ok_or(Error::NoEncontrado("Producto de la venta"))?;
                let pendiente = partida.cantidad - partida.devuelto;
                if linea.cantidad > pendiente {
                    return Err(Error::invalido(format!(
                        "De {} solo quedan {pendiente} {} por devolver.",
                        partida.descripcion, partida.unidad
                    )));
                }
                devueltas.push((partida, linea.cantidad));
            }

            let importe: Dinero = devueltas.iter().map(|&(p, cantidad)| cantidad * p.precio).sum();
            let ya_reembolsado: Dinero = tx.query_row(
                "SELECT coalesce(sum(reembolso), 0) FROM devoluciones WHERE venta_id = ?1",
                [venta_id],
                |f| f.get(0),
            )?;
            let reembolso = reembolso(&venta, importe, ya_reembolsado, queda_completa(&venta, &devueltas));

            tx.execute(
                "INSERT INTO devoluciones (venta_id, creada, cajero_id, caja_id, autorizo_id, metodo, motivo, reembolso)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![venta_id, ahora(), cajero_id, caja_id, autorizacion.admin_id, metodo, motivo.trim(), reembolso],
            )?;
            let id = tx.last_insert_rowid();
            let referencia = format!("Devolución de venta {}", venta.folio.unwrap_or_default());
            for (partida, cantidad) in devueltas {
                tx.execute(
                    "INSERT INTO partidas_devueltas (devolucion_id, partida_id, cantidad, importe) VALUES (?1, ?2, ?3, ?4)",
                    params![id, partida.id, cantidad, cantidad * partida.precio],
                )?;
                catalogo::mover(tx, partida.producto_id, cantidad, TipoMovimiento::Devolucion, &referencia)?;
            }
            cargar(tx, id)
        })
    }

    pub fn devoluciones_de(&self, venta_id: i64) -> Resultado<Vec<Devolucion>> {
        let mut sentencia = self.conn().prepare("SELECT id FROM devoluciones WHERE venta_id = ?1 ORDER BY id")?;
        let ids: Vec<i64> = sentencia.query_map([venta_id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
        ids.into_iter().map(|id| cargar(self.conn(), id)).collect()
    }

    pub fn comprobante_devolucion(&self, id: i64) -> Resultado<ComprobanteDevolucion> {
        Ok(ComprobanteDevolucion { negocio: self.negocio()?, devolucion: cargar(self.conn(), id)? })
    }
}

/// ¿Con esto el cliente regresa todo lo que le quedaba?
fn queda_completa(venta: &Venta, devueltas: &[(&Partida, Cantidad)]) -> bool {
    venta.partidas.iter().all(|p| {
        let esta_vez: Cantidad = devueltas.iter().filter(|(d, _)| d.id == p.id).map(|&(_, c)| c).sum();
        p.devuelto + esta_vez == p.cantidad
    })
}

/// Lo devuelto pierde la misma proporción de descuento que tuvo la venta. La devolución que completa
/// la venta regresa exactamente lo que falta, para que los centavos cuadren con lo cobrado.
fn reembolso(venta: &Venta, importe: Dinero, ya_reembolsado: Dinero, completa: bool) -> Dinero {
    if completa {
        return venta.totales.total - ya_reembolsado;
    }
    let bruto: Dinero = venta.partidas.iter().map(|p| p.importe).sum();
    if bruto.es_positivo() { venta.totales.total.proporcion(importe, bruto) } else { Dinero::CERO }
}

fn cargar(conn: &Connection, id: i64) -> Resultado<Devolucion> {
    let mut devolucion = conn
        .query_row(
            "SELECT d.*, v.folio, c.nombre AS cajero, a.nombre AS autorizo
             FROM devoluciones d
             JOIN ventas v ON v.id = d.venta_id
             JOIN usuarios c ON c.id = d.cajero_id
             JOIN usuarios a ON a.id = d.autorizo_id
             WHERE d.id = ?1",
            [id],
            |f| {
                Ok(Devolucion {
                    id: f.get("id")?,
                    venta_id: f.get("venta_id")?,
                    folio_venta: f.get("folio")?,
                    creada: f.get("creada")?,
                    cajero: f.get("cajero")?,
                    autorizo: f.get("autorizo")?,
                    metodo: f.get("metodo")?,
                    motivo: f.get("motivo")?,
                    reembolso: f.get("reembolso")?,
                    partidas: Vec::new(),
                })
            },
        )
        .optional()?
        .ok_or(Error::NoEncontrado("Devolución"))?;

    let mut partidas = conn.prepare_cached(
        "SELECT pd.partida_id, pa.descripcion, pa.unidad, pd.cantidad, pa.precio, pd.importe
         FROM partidas_devueltas pd JOIN partidas pa ON pa.id = pd.partida_id
         WHERE pd.devolucion_id = ?1 ORDER BY pd.id",
    )?;
    devolucion.partidas = partidas
        .query_map([id], |f| {
            Ok(PartidaDevuelta {
                partida_id: f.get(0)?,
                descripcion: f.get(1)?,
                unidad: f.get(2)?,
                cantidad: f.get(3)?,
                precio: f.get(4)?,
                importe: f.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(devolucion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogo::tests::{c, d};
    use crate::ventas::tests::Tienda;

    fn devolver(partida: &Partida, cantidad: &str) -> ADevolver {
        ADevolver { partida_id: partida.id, cantidad: c(cantidad) }
    }

    #[test]
    fn devolucion_parcial_y_luego_la_completa() {
        let mut t = Tienda::nueva();
        let venta = t.vender(&[(&t.martillo.clone(), "3"), (&t.tornillo.clone(), "5")]); // 348 + 5
        let [martillos, tornillos] = [&venta.partidas[0], &venta.partidas[1]];
        let permiso = t.bd.autorizar("1111").unwrap();

        let primera =
            t.bd.devolver(venta.id, t.cajera, &[devolver(martillos, "1")], Metodo::Efectivo, "No le quedó", permiso)
                .unwrap();
        assert_eq!((primera.reembolso, primera.folio_venta, primera.autorizo.as_str()), (d("116"), 1, "Admin"));
        assert_eq!(t.bd.producto(t.martillo.id).unwrap().existencia, c("98"));
        assert_eq!(t.bd.venta(venta.id).unwrap().partidas[0].devuelto, c("1"));

        let error =
            t.bd.devolver(venta.id, t.cajera, &[devolver(martillos, "3")], Metodo::Efectivo, "", permiso).unwrap_err();
        assert_eq!(error.to_string(), "De Martillo solo quedan 2 pza por devolver.");

        let resto = [devolver(martillos, "2"), devolver(tornillos, "5")];
        let segunda = t.bd.devolver(venta.id, t.cajera, &resto, Metodo::Efectivo, "", permiso).unwrap();
        assert_eq!(segunda.reembolso, d("237"));
        assert_eq!(t.bd.devoluciones_de(venta.id).unwrap().len(), 2);
        assert!(t.bd.cancelar(venta.id, permiso).is_err()); // ya tiene devoluciones
    }

    #[test]
    fn con_descuento_regresa_la_parte_proporcional() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, c("3"), None).unwrap();
        t.bd.aplicar_descuento(id, d("48")).unwrap(); // 348 − 48 = 300: 100 por martillo
        let venta = t.bd.cobrar(id, &Tienda::efectivo("300"), None).unwrap();
        let permiso = t.bd.autorizar("1111").unwrap();
        let martillos = &venta.partidas[0];

        let una = t.bd.devolver(id, t.cajera, &[devolver(martillos, "1")], Metodo::Efectivo, "", permiso).unwrap();
        assert_eq!(una.reembolso, d("100"));
        let resto = t.bd.devolver(id, t.cajera, &[devolver(martillos, "2")], Metodo::Tarjeta, "", permiso).unwrap();
        assert_eq!(resto.reembolso, d("200"));
    }

    #[test]
    fn a_granel_regresa_kilos() {
        let mut t = Tienda::nueva();
        let clavo = crate::catalogo::DatosProducto {
            granel: true,
            ..crate::catalogo::tests::datos("CLA", "Clavo", "48", "30")
        };
        let clavo = t.bd.crear_producto(&clavo).unwrap();
        let venta = t.vender(&[(&clavo, "1")]);
        let permiso = t.bd.autorizar("1111").unwrap();
        t.bd.devolver(venta.id, t.cajera, &[devolver(&venta.partidas[0], "0.25")], Metodo::Efectivo, "", permiso)
            .unwrap();
        assert_eq!(t.bd.producto(clavo.id).unwrap().existencia, c("-0.75")); // 0 − 1 + 0.25
    }

    #[test]
    fn el_corte_descuenta_el_efectivo_regresado() {
        let mut t = Tienda::nueva();
        let venta = t.vender(&[(&t.martillo.clone(), "2")]); // 232 en efectivo
        let permiso = t.bd.autorizar("1111").unwrap();
        t.bd.devolver(venta.id, t.cajera, &[devolver(&venta.partidas[0], "1")], Metodo::Efectivo, "", permiso).unwrap();

        let caja = t.bd.caja_abierta().unwrap().unwrap();
        let corte = t.bd.corte(caja.id).unwrap();
        assert_eq!(corte.devoluciones, [crate::caja::PorMetodo { metodo: Metodo::Efectivo, monto: d("116") }]);
        assert_eq!(corte.efectivo_esperado, d("616")); // 500 + 232 − 116
    }

    #[test]
    fn validaciones() {
        let mut t = Tienda::nueva();
        let venta = t.vender(&[(&t.martillo.clone(), "1")]);
        let permiso = t.bd.autorizar("1111").unwrap();
        let martillo = &venta.partidas[0];
        assert!(t.bd.devolver(venta.id, t.cajera, &[], Metodo::Efectivo, "", permiso).is_err());
        assert!(t.bd.devolver(venta.id, t.cajera, &[devolver(martillo, "-1")], Metodo::Efectivo, "", permiso).is_err());
        let ajena = ADevolver { partida_id: 999, cantidad: c("1") };
        assert!(t.bd.devolver(venta.id, t.cajera, &[ajena], Metodo::Efectivo, "", permiso).is_err());

        let abierta = t.bd.venta_en_curso(t.cajera).unwrap();
        assert!(
            t.bd.devolver(abierta.id, t.cajera, &[devolver(martillo, "1")], Metodo::Efectivo, "", permiso).is_err()
        );

        let caja = t.bd.caja_abierta().unwrap().unwrap();
        t.bd.cerrar_caja(caja.id, t.cajera, d("616")).unwrap();
        let error = t.bd.devolver(venta.id, t.cajera, &[devolver(martillo, "1")], Metodo::Efectivo, "", permiso);
        assert_eq!(error.unwrap_err().to_string(), "Abre la caja para hacer la devolución.");
    }
}
