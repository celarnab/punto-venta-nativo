//! Cotizaciones: cuánto costaría una venta, sin cobrarla ni tocar el inventario.
//! Se arman en la pantalla de venta y después se pueden convertir en venta.

use chrono::Days;
use rusqlite::{OptionalExtension, params};

use crate::catalogo;
use crate::ventas::{self, Estado, Venta};
use crate::{Bd, Error, Resultado, ahora};

impl Bd {
    /// Guarda la venta en curso como cotización, válida `dias_vigencia` días; el cajero sigue con una venta nueva.
    pub fn cotizar(&mut self, venta_id: i64, dias_vigencia: u32, nota: &str) -> Resultado<Venta> {
        self.transaccion(|tx| {
            let venta = ventas::cargar(tx, venta_id)?;
            ventas::venta_en(&venta, Estado::Abierta)?;
            if venta.partidas.is_empty() {
                return Err(Error::invalido("La venta está vacía: no hay nada que cotizar."));
            }
            let folio: i64 =
                tx.query_row("SELECT coalesce(max(folio_cotizacion), 0) + 1 FROM ventas", [], |f| f.get(0))?;
            let vigencia = ahora().date() + Days::new(dias_vigencia.into());
            tx.execute(
                "UPDATE ventas SET estado = 'cotizacion', folio_cotizacion = ?2, vigencia = ?3, nota = ?4, creada = ?5
                 WHERE id = ?1",
                params![venta_id, folio, vigencia, nota.trim(), ahora()],
            )?;
            ventas::cargar(tx, venta_id)
        })
    }

    /// Las más recientes primero.
    pub fn cotizaciones(&self, limite: usize) -> Resultado<Vec<Venta>> {
        ventas::cargar_varias(
            self.conn(),
            "SELECT id FROM ventas WHERE estado = 'cotizacion' ORDER BY folio_cotizacion DESC LIMIT ?1",
            [limite as i64],
        )
    }

    pub fn cotizacion_por_folio(&self, folio: i64) -> Resultado<Venta> {
        let id: i64 = self
            .conn()
            .query_row("SELECT id FROM ventas WHERE folio_cotizacion = ?1 AND estado = 'cotizacion'", [folio], |f| {
                f.get(0)
            })
            .optional()?
            .ok_or(Error::NoEncontrado("Cotización"))?;
        self.venta(id)
    }

    /// Lleva la cotización a la pantalla de venta del cajero (lo que tenía ahí queda en pausa).
    /// Mientras esté vigente se respetan sus precios; ya vencida, se cobra a precios de hoy.
    pub fn convertir_cotizacion(&mut self, cotizacion_id: i64, cajero_id: i64) -> Resultado<Venta> {
        self.transaccion(|tx| {
            let cotizacion = ventas::cargar(tx, cotizacion_id)?;
            ventas::venta_en(&cotizacion, Estado::Cotizacion)?;
            ventas::apartar_en_curso(tx, cajero_id)?;

            let folio = cotizacion.folio_cotizacion.unwrap_or_default();
            tx.execute(
                "INSERT INTO ventas (cajero_id, cliente_id, descuento, nota, cotizacion_id, creada)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    cajero_id,
                    cotizacion.cliente_id,
                    cotizacion.totales.descuento,
                    format!("Cotización {folio}"),
                    cotizacion_id,
                    ahora()
                ],
            )?;
            let venta_id = tx.last_insert_rowid();

            let respetar_precios = cotizacion.vigente(ahora().date());
            for p in &cotizacion.partidas {
                // Lo que se borró del catálogo después de cotizar ya no se puede vender.
                if catalogo::cargar(tx, p.producto_id).is_err() {
                    continue;
                }
                let partida_id = ventas::poner_cantidad(tx, venta_id, p.producto_id, p.cantidad)?;
                if let Some(partida_id) = partida_id.filter(|_| respetar_precios) {
                    ventas::fijar_precio(tx, partida_id, p.precio, p.cantidad)?;
                }
            }
            ventas::recalcular(tx, venta_id)?;
            ventas::cargar(tx, venta_id)
        })
    }

    pub fn borrar_cotizacion(&mut self, id: i64) -> Resultado<()> {
        match self.conn().execute("DELETE FROM ventas WHERE id = ?1 AND estado = 'cotizacion'", [id])? {
            0 => Err(Error::NoEncontrado("Cotización")),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cantidad;
    use crate::catalogo::tests::{c, d, datos};
    use crate::ventas::tests::Tienda;

    /// Una cotización de 2 martillos ($232) y 5 tornillos ($5).
    fn cotizada(t: &mut Tienda) -> Venta {
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, c("2"), None).unwrap();
        t.bd.agregar(id, t.tornillo.id, c("5"), None).unwrap();
        t.bd.cotizar(id, 15, "Obra de la calle 5").unwrap()
    }

    #[test]
    fn cotizar_no_toca_inventario_y_libera_la_pantalla() {
        let mut t = Tienda::nueva();
        let cotizacion = cotizada(&mut t);
        assert_eq!(
            (cotizacion.estado, cotizacion.folio_cotizacion, cotizacion.folio),
            (Estado::Cotizacion, Some(1), None)
        );
        assert_eq!(cotizacion.totales.total, d("237"));
        assert_eq!(cotizacion.vigencia, Some(ahora().date() + Days::new(15)));
        assert_eq!(t.bd.producto(t.martillo.id).unwrap().existencia, c("100"));

        let nueva = t.bd.venta_en_curso(t.cajera).unwrap();
        assert!(nueva.partidas.is_empty());
        assert_eq!(cotizada(&mut t).folio_cotizacion, Some(2));
        assert_eq!(t.bd.cotizaciones(10).unwrap().len(), 2);
        assert!(t.bd.cotizar(nueva.id, 15, "").is_err()); // ya no existe: fue cotizada
    }

    #[test]
    fn convertir_respeta_los_precios_vigentes() {
        let mut t = Tienda::nueva();
        let cotizacion = cotizada(&mut t);
        let en_pantalla = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(en_pantalla, t.martillo.id, Cantidad::UNO, None).unwrap();

        // El martillo sube de precio después de cotizar.
        t.bd.actualizar_producto(t.martillo.id, &datos("MAR", "Martillo", "150", "70")).unwrap();

        let venta = t.bd.convertir_cotizacion(cotizacion.id, t.cajera).unwrap();
        assert_eq!((venta.estado, venta.cotizacion_id), (Estado::Abierta, Some(cotizacion.id)));
        assert_eq!(venta.totales.total, d("237")); // al precio cotizado
        assert!(venta.partidas.iter().all(|p| p.precio_fijo));
        assert_eq!(t.bd.ventas_pausadas().unwrap()[0].id, en_pantalla); // lo de la pantalla esperó en pausa
        assert_eq!(t.bd.venta(cotizacion.id).unwrap().estado, Estado::Cotizacion); // la cotización se conserva
    }

    #[test]
    fn vencida_se_cobra_a_precios_de_hoy() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        let cotizacion = t.bd.cotizar(id, 0, "").unwrap();
        t.bd.conn()
            .execute("UPDATE ventas SET vigencia = date(vigencia, '-1 day') WHERE id = ?1", [cotizacion.id])
            .unwrap();
        t.bd.actualizar_producto(t.martillo.id, &datos("MAR", "Martillo", "150", "70")).unwrap();

        let venta = t.bd.convertir_cotizacion(cotizacion.id, t.cajera).unwrap();
        assert_eq!(venta.totales.total, d("150"));
    }

    #[test]
    fn se_puede_borrar() {
        let mut t = Tienda::nueva();
        let cotizacion = cotizada(&mut t);
        assert_eq!(t.bd.cotizacion_por_folio(1).unwrap().id, cotizacion.id);
        t.bd.borrar_cotizacion(cotizacion.id).unwrap();
        assert!(t.bd.cotizaciones(10).unwrap().is_empty());
        assert!(t.bd.borrar_cotizacion(cotizacion.id).is_err());
    }
}
