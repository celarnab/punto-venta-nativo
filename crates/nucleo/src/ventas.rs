//! La venta: se arma partida por partida, puede esperar en pausa, y al cobrarse
//! recibe folio y descuenta inventario.

use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};
use serde::Serialize;

use crate::catalogo::{self, TipoMovimiento};
use crate::cobro::{Aplicado, Cobro, repartir};
use crate::negocio::{Negocio, es_farmacia, tasa_iva};
use crate::usuarios::Autorizacion;
use crate::{Bd, Cantidad, Dinero, Error, Porcentaje, Resultado, ahora, caja, lotes};

enum_texto! {
    pub enum Estado {
        Abierta = "abierta", "Abierta";
        Pausada = "pausada", "En pausa";
        Pagada = "pagada", "Pagada";
        Cancelada = "cancelada", "Cancelada";
        Cotizacion = "cotizacion", "Cotización";
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Partida {
    pub id: i64,
    pub producto_id: i64,
    pub codigo: String,
    pub descripcion: String,
    /// En la que se vendió: «pza», «kg», «m»…
    pub unidad: String,
    pub cantidad: Cantidad,
    pub precio: Dinero,
    /// Elegido a mano: ya no cambia con la cantidad.
    pub precio_fijo: bool,
    pub costo: Dinero,
    pub importe: Dinero,
    /// Lo que el cliente ya regresó.
    pub devuelto: Cantidad,
    /// Un precio de mayoreo que la cantidad ya alcanza y que no es el que lleva (solo en ventas abiertas).
    pub sugerencia: Option<Sugerencia>,
    /// El lote del que sale (farmacias).
    pub lote_id: Option<i64>,
    pub lote: Option<String>,
    pub caducidad: Option<NaiveDate>,
    /// En una farmacia, el producto tiene lotes y la partida no lleva ninguno (solo en ventas abiertas).
    pub pide_lote: bool,
    /// El producto solo se vende con receta (cuenta en farmacias).
    pub con_receta: bool,
}

/// Precio que se le sugiere al cajero; él decide si lo usa.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sugerencia {
    pub precio_id: i64,
    pub nombre: String,
    pub precio: Dinero,
}

impl Partida {
    pub fn bajo_costo(&self) -> bool {
        self.precio < self.costo
    }

    fn de_fila(fila: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: fila.get("id")?,
            producto_id: fila.get("producto_id")?,
            codigo: fila.get("codigo")?,
            descripcion: fila.get("descripcion")?,
            unidad: fila.get("unidad")?,
            cantidad: fila.get("cantidad")?,
            precio: fila.get("precio")?,
            precio_fijo: fila.get("precio_fijo")?,
            costo: fila.get("costo")?,
            importe: fila.get("importe")?,
            devuelto: fila.get("devuelto")?,
            sugerencia: None,
            lote_id: fila.get("lote_id")?,
            lote: fila.get("lote")?,
            caducidad: fila.get("caducidad")?,
            pide_lote: false,
            con_receta: fila.get("con_receta")?,
        })
    }
}

/// Cómo se llega del importe de las partidas al total. El IVA va incluido en los precios.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Totales {
    pub descuento: Dinero,
    pub subtotal: Dinero,
    pub iva: Dinero,
    pub total: Dinero,
}

impl Totales {
    /// El descuento nunca deja el total por debajo de cero.
    pub fn calcular(importe: Dinero, descuento: Dinero, tasa_iva: Porcentaje) -> Self {
        let descuento = descuento.clamp(Dinero::CERO, importe.max(Dinero::CERO));
        let total = importe - descuento;
        let subtotal = total.sin_iva(tasa_iva);
        Self { descuento, subtotal, iva: total - subtotal, total }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Venta {
    pub id: i64,
    pub folio: Option<i64>,
    pub estado: Estado,
    pub cajero_id: i64,
    pub cajero: String,
    pub cliente_id: Option<i64>,
    pub cliente: Option<String>,
    /// Para reconocer una venta en pausa.
    pub nota: String,
    pub creada: NaiveDateTime,
    /// La última vez que se puso en pausa.
    pub pausada_en: Option<NaiveDateTime>,
    pub pagada_en: Option<NaiveDateTime>,
    pub folio_cotizacion: Option<i64>,
    /// Último día en que se respetan los precios de una cotización.
    pub vigencia: Option<NaiveDate>,
    /// La cotización de la que salió esta venta.
    pub cotizacion_id: Option<i64>,
    #[serde(flatten)]
    pub totales: Totales,
    pub partidas: Vec<Partida>,
    pub pagos: Vec<Aplicado>,
}

impl Venta {
    pub fn cambio(&self) -> Dinero {
        self.pagos.iter().map(Aplicado::cambio).sum()
    }

    /// Una cotización respeta sus precios hasta el día de su vigencia, inclusive.
    pub fn vigente(&self, hoy: NaiveDate) -> bool {
        self.vigencia.is_some_and(|ultimo| hoy <= ultimo)
    }

    fn de_fila(fila: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: fila.get("id")?,
            folio: fila.get("folio")?,
            estado: fila.get("estado")?,
            cajero_id: fila.get("cajero_id")?,
            cajero: fila.get("cajero")?,
            cliente_id: fila.get("cliente_id")?,
            cliente: fila.get("cliente")?,
            nota: fila.get("nota")?,
            creada: fila.get("creada")?,
            pausada_en: fila.get("pausada_en")?,
            pagada_en: fila.get("pagada_en")?,
            folio_cotizacion: fila.get("folio_cotizacion")?,
            vigencia: fila.get("vigencia")?,
            cotizacion_id: fila.get("cotizacion_id")?,
            totales: Totales {
                descuento: fila.get("descuento")?,
                subtotal: fila.get("subtotal")?,
                iva: fila.get("iva")?,
                total: fila.get("total")?,
            },
            partidas: Vec::new(),
            pagos: Vec::new(),
        })
    }
}

/// Lo que se imprime: la venta y los datos del negocio.
#[derive(Clone, Debug, Serialize)]
pub struct Ticket {
    pub negocio: Negocio,
    pub venta: Venta,
    pub cambio: Dinero,
}

impl Bd {
    // — Armado —

    /// La venta que el cajero tiene en pantalla; si no hay, empieza una.
    pub fn venta_en_curso(&mut self, cajero_id: i64) -> Resultado<Venta> {
        self.transaccion(|tx| {
            let id = match en_curso(tx, cajero_id)? {
                Some(id) => id,
                None => {
                    tx.execute("INSERT INTO ventas (cajero_id, creada) VALUES (?1, ?2)", params![cajero_id, ahora()])?;
                    tx.last_insert_rowid()
                }
            };
            cargar(tx, id)
        })
    }

    pub fn venta(&self, id: i64) -> Resultado<Venta> {
        cargar(self.conn(), id)
    }

    /// Suma (o resta, si es negativa) `cantidad` del producto; la partida desaparece al llegar a cero.
    ///
    /// `precio_elegido`: uno de los otros precios del producto. `None` = el normal, o el que la partida
    /// ya tuviera. El mayoreo nunca se aplica solo: solo se sugiere (ver [`Partida::sugerencia`]).
    pub fn agregar(
        &mut self,
        venta_id: i64,
        producto_id: i64,
        cantidad: Cantidad,
        precio_elegido: Option<i64>,
    ) -> Resultado<Venta> {
        self.agregar_de_lote(venta_id, producto_id, None, cantidad, precio_elegido)
    }

    /// Como [`Bd::agregar`], del lote `lote_id` (farmacias): cada lote del producto va en su propia partida.
    pub fn agregar_de_lote(
        &mut self,
        venta_id: i64,
        producto_id: i64,
        lote_id: Option<i64>,
        cantidad: Cantidad,
        precio_elegido: Option<i64>,
    ) -> Resultado<Venta> {
        self.editar(venta_id, |tx| {
            let actual = cantidad_en(tx, venta_id, producto_id, lote_id)?;
            let nueva = actual.unwrap_or_default() + cantidad;
            let partida_id = poner_cantidad(tx, venta_id, producto_id, lote_id, nueva)?;
            if let (Some(partida_id), Some(precio_id)) = (partida_id, precio_elegido) {
                let precio = otro_precio(&catalogo::cargar(tx, producto_id)?, precio_id)?;
                fijar_precio(tx, partida_id, precio, nueva)?;
            }
            Ok(())
        })
    }

    pub fn cambiar_cantidad(&mut self, venta_id: i64, partida_id: i64, cantidad: Cantidad) -> Resultado<Venta> {
        self.editar(venta_id, |tx| {
            let partida = partida(tx, venta_id, partida_id)?;
            poner_cantidad(tx, venta_id, partida.producto_id, partida.lote_id, cantidad).map(drop)
        })
    }

    /// Cambia el lote de una partida (`None` = sin lote). Si la venta ya lleva ese lote del producto, se juntan.
    pub fn elegir_lote(&mut self, venta_id: i64, partida_id: i64, lote_id: Option<i64>) -> Resultado<Venta> {
        self.editar(venta_id, |tx| {
            let partida = partida(tx, venta_id, partida_id)?;
            if partida.lote_id == lote_id {
                return Ok(());
            }
            match cantidad_en(tx, venta_id, partida.producto_id, lote_id)? {
                Some(ya) => {
                    poner_cantidad(tx, venta_id, partida.producto_id, lote_id, ya + partida.cantidad)?;
                    tx.execute("DELETE FROM partidas WHERE id = ?1", [partida_id])?;
                }
                None => {
                    if let Some(lote_id) = lote_id {
                        lotes::revisar_venta(tx, partida.producto_id, lote_id, partida.cantidad)?;
                    }
                    tx.execute("UPDATE partidas SET lote_id = ?2 WHERE id = ?1", params![partida_id, lote_id])?;
                }
            }
            Ok(())
        })
    }

    pub fn quitar(&mut self, venta_id: i64, partida_id: i64) -> Resultado<Venta> {
        self.cambiar_cantidad(venta_id, partida_id, Cantidad::CERO)
    }

    /// Cambia el precio de una partida a uno de los del producto (`None` = el normal) y lo deja fijo.
    pub fn elegir_precio(&mut self, venta_id: i64, partida_id: i64, precio_id: Option<i64>) -> Resultado<Venta> {
        self.editar(venta_id, |tx| {
            let partida = partida(tx, venta_id, partida_id)?;
            let producto = catalogo::cargar(tx, partida.producto_id)?;
            let precio = match precio_id {
                None => producto.precio,
                Some(id) => otro_precio(&producto, id)?,
            };
            fijar_precio(tx, partida_id, precio, partida.cantidad)
        })
    }

    pub fn aplicar_descuento(&mut self, venta_id: i64, descuento: Dinero) -> Resultado<Venta> {
        if descuento < Dinero::CERO {
            return Err(Error::invalido("El descuento no puede ser negativo."));
        }
        self.editar(venta_id, |tx| {
            tx.execute("UPDATE ventas SET descuento = ?2 WHERE id = ?1", params![venta_id, descuento])?;
            Ok(())
        })
    }

    pub fn asignar_cliente(&mut self, venta_id: i64, cliente_id: Option<i64>) -> Resultado<Venta> {
        self.editar(venta_id, |tx| {
            tx.execute("UPDATE ventas SET cliente_id = ?2 WHERE id = ?1", params![venta_id, cliente_id])?;
            Ok(())
        })
    }

    // — Pausa —

    pub fn pausar(&mut self, venta_id: i64, nota: &str) -> Resultado<()> {
        self.transaccion(|tx| {
            exigir(tx, venta_id, Estado::Abierta)?;
            if cantidad_de_partidas(tx, venta_id)? == 0 {
                return Err(Error::invalido("La venta está vacía: no hay nada que pausar."));
            }
            tx.execute(
                "UPDATE ventas SET estado = 'pausada', nota = ?2, pausada_en = ?3 WHERE id = ?1",
                params![venta_id, nota.trim(), ahora()],
            )?;
            Ok(())
        })
    }

    pub fn ventas_pausadas(&self) -> Resultado<Vec<Venta>> {
        cargar_varias(self.conn(), "SELECT id FROM ventas WHERE estado = 'pausada' ORDER BY pausada_en", [])
    }

    /// El cajero toma una venta en pausa; la que tenía en pantalla pasa a pausa (o se tira, si estaba vacía).
    pub fn retomar(&mut self, venta_id: i64, cajero_id: i64) -> Resultado<Venta> {
        self.transaccion(|tx| {
            exigir(tx, venta_id, Estado::Pausada)?;
            apartar_en_curso(tx, cajero_id)?;
            tx.execute("UPDATE ventas SET estado = 'abierta', cajero_id = ?2 WHERE id = ?1", [venta_id, cajero_id])?;
            cargar(tx, venta_id)
        })
    }

    /// Tira una venta que no se cobró.
    pub fn descartar(&mut self, venta_id: i64) -> Resultado<()> {
        let borradas =
            self.conn().execute("DELETE FROM ventas WHERE id = ?1 AND estado IN ('abierta', 'pausada')", [venta_id])?;
        match borradas {
            0 => Err(Error::invalido("Solo se puede descartar una venta que no se ha cobrado.")),
            _ => Ok(()),
        }
    }

    // — Cierre —

    /// Registra los pagos, asigna folio y descuenta inventario.
    ///
    /// `autorizacion`: un administrador permite vender por debajo del costo, si hace falta.
    pub fn cobrar(&mut self, venta_id: i64, cobros: &[Cobro], autorizacion: Option<Autorizacion>) -> Resultado<Venta> {
        self.cobrar_con_receta(venta_id, cobros, autorizacion, false)
    }

    /// Como [`Bd::cobrar`]; en una farmacia, `receta` dice que el cajero confirmó que el cliente presentó una receta
    /// médica válida. Sin ella no se cobra una venta que lleva productos con receta.
    pub fn cobrar_con_receta(
        &mut self,
        venta_id: i64,
        cobros: &[Cobro],
        autorizacion: Option<Autorizacion>,
        receta: bool,
    ) -> Resultado<Venta> {
        self.transaccion(|tx| {
            let venta = cargar(tx, venta_id)?;
            venta_en(&venta, Estado::Abierta)?;
            if venta.partidas.is_empty() {
                return Err(Error::invalido("La venta no tiene productos."));
            }
            let perdidas: Vec<String> =
                venta.partidas.iter().filter(|p| p.bajo_costo()).map(|p| p.descripcion.clone()).collect();
            if !perdidas.is_empty() && autorizacion.is_none() {
                return Err(Error::BajoCosto(perdidas));
            }
            if es_farmacia(tx)? {
                revisar_lotes(tx, &venta)?;
                let con_receta: Vec<&str> =
                    venta.partidas.iter().filter(|p| p.con_receta).map(|p| p.descripcion.as_str()).collect();
                if !con_receta.is_empty() && !receta {
                    return Err(Error::invalido(format!(
                        "Se vende solo con receta: {}. Confirma que el cliente presentó una receta médica válida.",
                        con_receta.join(", ")
                    )));
                }
            }
            let caja_id = caja::abierta(tx)?.ok_or(Error::invalido("Abre la caja antes de cobrar."))?;
            let aplicados = repartir(cobros, venta.totales.total)?; // valida antes de tocar nada

            let folio: i64 = tx.query_row("SELECT coalesce(max(folio), 0) + 1 FROM ventas", [], |f| f.get(0))?;
            let autorizo = autorizacion.filter(|_| !perdidas.is_empty()).map(|a| a.admin_id);
            tx.execute(
                "UPDATE ventas SET estado = 'pagada', folio = ?2, pagada_en = ?3, caja_id = ?4, autorizo_bajo_costo_id = ?5
                 WHERE id = ?1",
                params![venta_id, folio, ahora(), caja_id, autorizo],
            )?;
            let mut insertar =
                tx.prepare("INSERT INTO pagos (venta_id, metodo, recibido, monto) VALUES (?1, ?2, ?3, ?4)")?;
            for pago in &aplicados {
                insertar.execute(params![venta_id, pago.metodo, pago.recibido, pago.monto])?;
            }
            mover_inventario(tx, &venta, -1, TipoMovimiento::Venta, &format!("Venta {folio}"))?;
            cargar(tx, venta_id)
        })
    }

    /// Anula una venta cobrada y regresa la mercancía al inventario.
    pub fn cancelar(&mut self, venta_id: i64, autorizacion: Autorizacion) -> Resultado<Venta> {
        self.transaccion(|tx| {
            let venta = cargar(tx, venta_id)?;
            venta_en(&venta, Estado::Pagada)?;
            if venta.partidas.iter().any(|p| p.devuelto.es_positivo()) {
                return Err(Error::invalido("La venta ya tiene devoluciones: haz una devolución por lo que falta."));
            }
            tx.execute(
                "UPDATE ventas SET estado = 'cancelada', cancelo_id = ?2 WHERE id = ?1",
                [venta_id, autorizacion.admin_id],
            )?;
            let folio = venta.folio.unwrap_or_default();
            mover_inventario(tx, &venta, 1, TipoMovimiento::Cancelacion, &format!("Cancela venta {folio}"))?;
            cargar(tx, venta_id)
        })
    }

    /// Ventas con folio (pagadas o canceladas) en el rango de días, la más reciente primero.
    pub fn historial(&self, desde: NaiveDate, hasta: NaiveDate) -> Resultado<Vec<Venta>> {
        cargar_varias(
            self.conn(),
            "SELECT id FROM ventas WHERE folio IS NOT NULL AND date(pagada_en) BETWEEN ?1 AND ?2 ORDER BY folio DESC",
            params![desde, hasta],
        )
    }

    pub fn venta_por_folio(&self, folio: i64) -> Resultado<Venta> {
        let id: i64 = self
            .conn()
            .query_row("SELECT id FROM ventas WHERE folio = ?1", [folio], |f| f.get(0))
            .optional()?
            .ok_or(Error::NoEncontrado("Venta"))?;
        self.venta(id)
    }

    pub fn ticket(&self, venta_id: i64) -> Resultado<Ticket> {
        let venta = self.venta(venta_id)?;
        Ok(Ticket { negocio: self.negocio()?, cambio: venta.cambio(), venta })
    }

    /// Aplica `cambio` a una venta abierta y recalcula sus totales, todo o nada.
    fn editar(&mut self, venta_id: i64, cambio: impl FnOnce(&Connection) -> Resultado<()>) -> Resultado<Venta> {
        self.transaccion(|tx| {
            exigir(tx, venta_id, Estado::Abierta)?;
            cambio(tx)?;
            recalcular(tx, venta_id)?;
            cargar(tx, venta_id)
        })
    }
}

fn en_curso(conn: &Connection, cajero_id: i64) -> Resultado<Option<i64>> {
    Ok(conn
        .query_row("SELECT id FROM ventas WHERE cajero_id = ?1 AND estado = 'abierta'", [cajero_id], |f| f.get(0))
        .optional()?)
}

/// Deja libre la pantalla del cajero: su venta en curso pasa a pausa, o se tira si está vacía.
pub(crate) fn apartar_en_curso(conn: &Connection, cajero_id: i64) -> Resultado<()> {
    if let Some(actual) = en_curso(conn, cajero_id)? {
        match cantidad_de_partidas(conn, actual)? {
            0 => conn.execute("DELETE FROM ventas WHERE id = ?1", [actual])?,
            _ => conn.execute(
                "UPDATE ventas SET estado = 'pausada', pausada_en = ?2 WHERE id = ?1",
                params![actual, ahora()],
            )?,
        };
    }
    Ok(())
}

pub(crate) fn exigir(conn: &Connection, venta_id: i64, estado: Estado) -> Resultado<()> {
    let actual: Estado = conn
        .query_row("SELECT estado FROM ventas WHERE id = ?1", [venta_id], |f| f.get(0))
        .optional()?
        .ok_or(Error::NoEncontrado("Venta"))?;
    en_estado(actual, estado)
}

pub(crate) fn venta_en(venta: &Venta, estado: Estado) -> Resultado<()> {
    en_estado(venta.estado, estado)
}

fn en_estado(actual: Estado, esperado: Estado) -> Resultado<()> {
    if actual == esperado {
        Ok(())
    } else {
        Err(Error::invalido(format!("La venta está {}.", actual.etiqueta().to_lowercase())))
    }
}

struct PartidaEnVenta {
    producto_id: i64,
    lote_id: Option<i64>,
    cantidad: Cantidad,
}

fn partida(conn: &Connection, venta_id: i64, partida_id: i64) -> Resultado<PartidaEnVenta> {
    conn.query_row(
        "SELECT producto_id, lote_id, cantidad FROM partidas WHERE venta_id = ?1 AND id = ?2",
        [venta_id, partida_id],
        |f| Ok(PartidaEnVenta { producto_id: f.get(0)?, lote_id: f.get(1)?, cantidad: f.get(2)? }),
    )
    .optional()?
    .ok_or(Error::NoEncontrado("Producto en la venta"))
}

pub(crate) fn fijar_precio(conn: &Connection, partida_id: i64, precio: Dinero, cantidad: Cantidad) -> Resultado<()> {
    conn.execute(
        "UPDATE partidas SET precio = ?2, precio_fijo = 1, importe = ?3 WHERE id = ?1",
        params![partida_id, precio, cantidad * precio],
    )?;
    Ok(())
}

pub(crate) fn cantidad_de_partidas(conn: &Connection, venta_id: i64) -> Resultado<i64> {
    Ok(conn.query_row("SELECT count(*) FROM partidas WHERE venta_id = ?1", [venta_id], |f| f.get(0))?)
}

/// La cantidad que la venta ya lleva del producto, de ese lote (o sin lote).
pub(crate) fn cantidad_en(
    conn: &Connection,
    venta_id: i64,
    producto_id: i64,
    lote_id: Option<i64>,
) -> Resultado<Option<Cantidad>> {
    Ok(conn
        .query_row(
            "SELECT cantidad FROM partidas WHERE venta_id = ?1 AND producto_id = ?2 AND lote_id IS ?3",
            params![venta_id, producto_id, lote_id],
            |f| f.get(0),
        )
        .optional()?)
}

/// Deja la partida (del producto y lote) con `cantidad`; el precio sigue a la cantidad (mayoreo) salvo que se haya
/// fijado a mano. Devuelve el id de la partida, o `None` si quedó en cero y se quitó.
pub(crate) fn poner_cantidad(
    conn: &Connection,
    venta_id: i64,
    producto_id: i64,
    lote_id: Option<i64>,
    cantidad: Cantidad,
) -> Resultado<Option<i64>> {
    if !cantidad.es_positivo() {
        conn.execute(
            "DELETE FROM partidas WHERE venta_id = ?1 AND producto_id = ?2 AND lote_id IS ?3",
            params![venta_id, producto_id, lote_id],
        )?;
        return Ok(None);
    }
    let producto = catalogo::cargar(conn, producto_id)?;
    if !producto.activo {
        return Err(Error::invalido(format!("{} está desactivado.", producto.nombre)));
    }
    if let Some(lote_id) = lote_id {
        lotes::revisar_venta(conn, producto_id, lote_id, cantidad)?;
    }
    // El precio no cambia solo con la cantidad: es el que ya llevaba, o el normal si es nueva.
    let anterior: Option<(i64, Dinero)> = conn
        .query_row(
            "SELECT id, precio FROM partidas WHERE venta_id = ?1 AND producto_id = ?2 AND lote_id IS ?3",
            params![venta_id, producto_id, lote_id],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )
        .optional()?;
    if let Some((id, precio)) = anterior {
        conn.execute(
            "UPDATE partidas SET cantidad = ?2, importe = ?3 WHERE id = ?1",
            params![id, cantidad, cantidad * precio],
        )?;
        return Ok(Some(id));
    }
    conn.execute(
        "INSERT INTO partidas (venta_id, producto_id, lote_id, descripcion, unidad, cantidad, precio, costo, importe)
         VALUES (:venta_id, :producto_id, :lote_id, :descripcion, :unidad, :cantidad, :precio, :costo, :importe)",
        named_params! {
            ":venta_id": venta_id,
            ":producto_id": producto_id,
            ":lote_id": lote_id,
            ":descripcion": producto.nombre,
            ":unidad": producto.unidad,
            ":cantidad": cantidad,
            ":precio": producto.precio,
            ":costo": producto.costo,
            ":importe": cantidad * producto.precio,
        },
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Al cobrar en una farmacia: ningún lote caducó ni se acabó mientras tanto (otra venta pudo usarlo), y lo que va
/// sin lote cabe en la existencia sin lote.
fn revisar_lotes(conn: &Connection, venta: &Venta) -> Resultado<()> {
    for partida in &venta.partidas {
        match partida.lote_id {
            Some(lote_id) => lotes::revisar_venta(conn, partida.producto_id, lote_id, partida.cantidad)?,
            None => lotes::revisar_sin_lote(conn, partida.producto_id, partida.cantidad)?,
        }
    }
    Ok(())
}

fn otro_precio(producto: &catalogo::Producto, precio_id: i64) -> Resultado<Dinero> {
    producto.precios.iter().find(|p| p.id == precio_id).map(|p| p.precio).ok_or(Error::NoEncontrado("Precio"))
}

pub(crate) fn recalcular(conn: &Connection, venta_id: i64) -> Resultado<()> {
    let (importe, descuento): (Dinero, Dinero) = conn.query_row(
        "SELECT (SELECT coalesce(sum(importe), 0) FROM partidas WHERE venta_id = v.id), v.descuento
         FROM ventas v WHERE v.id = ?1",
        [venta_id],
        |f| Ok((f.get(0)?, f.get(1)?)),
    )?;
    let t = Totales::calcular(importe, descuento, tasa_iva(conn)?);
    conn.execute(
        "UPDATE ventas SET descuento = ?2, subtotal = ?3, iva = ?4, total = ?5 WHERE id = ?1",
        params![venta_id, t.descuento, t.subtotal, t.iva, t.total],
    )?;
    Ok(())
}

fn mover_inventario(
    conn: &Connection,
    venta: &Venta,
    signo: i64,
    tipo: TipoMovimiento,
    referencia: &str,
) -> Resultado<()> {
    for partida in &venta.partidas {
        let cantidad = Cantidad::crudo(signo * partida.cantidad.como_crudo());
        catalogo::mover_en(conn, partida.producto_id, partida.lote_id, cantidad, tipo, referencia, None)?;
    }
    Ok(())
}

pub(crate) fn cargar(conn: &Connection, id: i64) -> Resultado<Venta> {
    let mut venta = conn
        .query_row(
            "SELECT v.*, u.nombre AS cajero, c.nombre AS cliente
             FROM ventas v JOIN usuarios u ON u.id = v.cajero_id LEFT JOIN clientes c ON c.id = v.cliente_id
             WHERE v.id = ?1",
            [id],
            Venta::de_fila,
        )
        .optional()?
        .ok_or(Error::NoEncontrado("Venta"))?;

    let mut partidas = conn.prepare_cached(
        "SELECT pa.*, p.codigo, p.con_receta, l.numero AS lote, l.caducidad,
                (SELECT coalesce(sum(pd.cantidad), 0) FROM partidas_devueltas pd WHERE pd.partida_id = pa.id) AS devuelto
         FROM partidas pa JOIN productos p ON p.id = pa.producto_id LEFT JOIN lotes l ON l.id = pa.lote_id
         WHERE pa.venta_id = ?1 ORDER BY pa.id",
    )?;
    venta.partidas = partidas.query_map([id], Partida::de_fila)?.collect::<rusqlite::Result<_>>()?;
    if venta.estado == Estado::Abierta {
        let farmacia = es_farmacia(conn)?;
        for partida in &mut venta.partidas {
            partida.sugerencia = sugerencia(conn, partida)?;
            partida.pide_lote = farmacia
                && partida.lote_id.is_none()
                && lotes::revisar_sin_lote(conn, partida.producto_id, partida.cantidad).is_err();
        }
    }

    let mut pagos = conn.prepare_cached("SELECT metodo, recibido, monto FROM pagos WHERE venta_id = ?1 ORDER BY id")?;
    venta.pagos = pagos
        .query_map([id], |f| Ok(Aplicado { metodo: f.get(0)?, recibido: f.get(1)?, monto: f.get(2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(venta)
}

/// El mayoreo que alcanza la cantidad de la partida, si no es ya el precio que lleva.
fn sugerencia(conn: &Connection, partida: &Partida) -> Resultado<Option<Sugerencia>> {
    let precios = catalogo::otros_precios(conn, partida.producto_id)?;
    let alcanzado = precios
        .iter()
        .filter(|p| p.cantidad_minima.is_some_and(|minima| minima <= partida.cantidad))
        .max_by_key(|p| p.cantidad_minima);
    Ok(alcanzado.filter(|p| p.precio != partida.precio).map(|p| Sugerencia {
        precio_id: p.id,
        nombre: p.nombre.clone(),
        precio: p.precio,
    }))
}

pub(crate) fn cargar_varias(
    conn: &Connection,
    sql_ids: &str,
    parametros: impl rusqlite::Params,
) -> Resultado<Vec<Venta>> {
    let mut sentencia = conn.prepare(sql_ids)?;
    let ids: Vec<i64> = sentencia.query_map(parametros, |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    ids.into_iter().map(|id| cargar(conn, id)).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::catalogo::tests::{c, d, datos, mayoreo};
    use crate::catalogo::{DatosProducto, Producto};
    use crate::cobro::Metodo;
    use crate::usuarios::Rol;
    use crate::usuarios::tests::datos as usuario;

    /// Una tienda con un administrador (PIN 1111), una cajera (2222) con la caja abierta, y dos productos.
    pub(crate) struct Tienda {
        pub bd: Bd,
        pub cajera: i64,
        pub martillo: Producto,
        pub tornillo: Producto,
    }

    impl Tienda {
        pub(crate) fn nueva() -> Self {
            let mut bd = Bd::en_memoria().unwrap();
            bd.crear_usuario(&usuario("Admin", Rol::Administrador), "1111").unwrap();
            let cajera = bd.crear_usuario(&usuario("Carla", Rol::Cajero), "2222").unwrap().id;
            bd.abrir_caja(cajera, d("500")).unwrap();
            let martillo = bd.crear_producto(&datos("MAR", "Martillo", "116", "70")).unwrap();
            let tornillo = bd
                .crear_producto(&DatosProducto {
                    precios: vec![mayoreo("Mayoreo", "0.80", Some("10")), mayoreo("Contratista", "0.30", None)],
                    ..datos("TOR", "Tornillo", "1", "0.40")
                })
                .unwrap();
            for p in [&martillo, &tornillo] {
                bd.registrar_entrada(p.id, c("100"), "Inicial").unwrap();
            }
            Self { bd, cajera, martillo, tornillo }
        }

        pub(crate) fn efectivo(recibido: &str) -> Vec<Cobro> {
            vec![Cobro { metodo: Metodo::Efectivo, recibido: d(recibido) }]
        }

        /// Vende y cobra en efectivo exacto.
        pub(crate) fn vender(&mut self, lineas: &[(&Producto, &str)]) -> Venta {
            let venta = self.bd.venta_en_curso(self.cajera).unwrap();
            for (producto, cantidad) in lineas {
                self.bd.agregar(venta.id, producto.id, c(cantidad), None).unwrap();
            }
            let total = self.bd.venta(venta.id).unwrap().totales.total;
            self.bd.cobrar(venta.id, &Self::efectivo(&total.to_string()), None).unwrap()
        }

        /// Vende y cobra en efectivo exacto, del lote `lote`.
        pub(crate) fn vender_de_lote(&mut self, producto: &Producto, lote: i64, cantidad: &str) -> Venta {
            let venta = self.bd.venta_en_curso(self.cajera).unwrap();
            self.bd.agregar_de_lote(venta.id, producto.id, Some(lote), c(cantidad), None).unwrap();
            let total = self.bd.venta(venta.id).unwrap().totales.total;
            self.bd.cobrar(venta.id, &Self::efectivo(&total.to_string()), None).unwrap()
        }

        pub(crate) fn existencia(&self, producto: &Producto) -> Cantidad {
            self.bd.producto(producto.id).unwrap().existencia
        }
    }

    #[test]
    fn totales_con_iva_incluido_y_descuento() {
        let t = Totales::calcular(d("126"), d("10"), Porcentaje::entero(16));
        assert_eq!((t.total, t.subtotal, t.iva), (d("116"), d("100"), d("16")));
        assert_eq!(Totales::calcular(d("50"), d("80"), Porcentaje::CERO).total, Dinero::CERO);
    }

    #[test]
    fn una_venta_en_curso_por_cajero() {
        let mut t = Tienda::nueva();
        let a = t.bd.venta_en_curso(t.cajera).unwrap();
        let b = t.bd.venta_en_curso(t.cajera).unwrap();
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn arma_la_venta() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        let venta = t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        assert_eq!(venta.partidas.len(), 1);
        assert_eq!((venta.partidas[0].cantidad, venta.totales.total), (c("2"), d("232")));
        assert_eq!((venta.totales.subtotal, venta.totales.iva), (d("200"), d("32")));

        let venta = t.bd.aplicar_descuento(id, d("32")).unwrap();
        assert_eq!(venta.totales.total, d("200"));

        let venta = t.bd.agregar(id, t.martillo.id, c("-2"), None).unwrap();
        assert!(venta.partidas.is_empty());
        assert_eq!(venta.totales.total, Dinero::CERO);
    }

    #[test]
    fn el_mayoreo_se_sugiere_pero_no_se_aplica_solo() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        let precio = |v: Venta| v.partidas[0].precio;

        let venta = t.bd.agregar(id, t.tornillo.id, c("9"), None).unwrap();
        let partida = venta.partidas[0].id;
        assert_eq!((venta.partidas[0].precio, venta.partidas[0].sugerencia.clone()), (d("1"), None));

        // Al llegar a 10 se sugiere el mayoreo, pero el precio sigue siendo el normal.
        let venta = t.bd.agregar(id, t.tornillo.id, Cantidad::UNO, None).unwrap();
        assert_eq!(precio(venta.clone()), d("1"));
        let sugerencia = venta.partidas[0].sugerencia.clone().unwrap();
        assert_eq!((sugerencia.nombre.as_str(), sugerencia.precio), ("Mayoreo", d("0.80")));

        // Si el cajero lo toma, ya no se sugiere nada.
        let venta = t.bd.elegir_precio(id, partida, Some(sugerencia.precio_id)).unwrap();
        assert_eq!((precio(venta.clone()), venta.partidas[0].sugerencia.clone()), (d("0.80"), None));
        assert!(t.bd.venta(id).unwrap().partidas[0].sugerencia.is_none());

        let contratista = t.tornillo.precios.iter().find(|p| p.nombre == "Contratista").unwrap().id;
        let venta = t.bd.elegir_precio(id, partida, Some(contratista)).unwrap();
        assert_eq!((venta.partidas[0].precio, venta.totales.total), (d("0.30"), d("3")));
        assert_eq!(precio(t.bd.cambiar_cantidad(id, partida, c("2")).unwrap()), d("0.30"));
        assert!(t.bd.quitar(id, partida).unwrap().partidas.is_empty());
    }

    #[test]
    fn a_granel_se_vende_por_kilo() {
        let mut t = Tienda::nueva();
        let clavo = DatosProducto { granel: true, ..datos("CLA", "Clavo 2\"", "48", "30") };
        let clavo = t.bd.crear_producto(&clavo).unwrap();
        t.bd.registrar_entrada(clavo.id, c("10"), "").unwrap();

        let venta = t.vender(&[(&clavo, "0.750")]);
        assert_eq!((venta.partidas[0].unidad.as_str(), venta.totales.total), ("kg", d("36")));
        assert_eq!(t.existencia(&clavo), c("9.25"));
    }

    #[test]
    fn el_precio_se_elige_al_agregar() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        let contratista = t.tornillo.precios.iter().find(|p| p.nombre == "Contratista").unwrap().id;
        let venta = t.bd.agregar(id, t.tornillo.id, c("4"), Some(contratista)).unwrap();
        assert_eq!((venta.partidas[0].precio, venta.partidas[0].precio_fijo), (d("0.30"), true));

        // Agregar más sin elegir respeta el elegido, aunque llegue al mayoreo.
        let venta = t.bd.agregar(id, t.tornillo.id, c("20"), None).unwrap();
        assert_eq!((venta.partidas[0].cantidad, venta.partidas[0].precio), (c("24"), d("0.30")));
        assert!(t.bd.agregar(id, t.martillo.id, Cantidad::UNO, Some(contratista)).is_err()); // precio de otro producto
    }

    #[test]
    fn borrar_un_producto_lo_saca_de_las_ventas_pendientes() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        t.bd.agregar(id, t.tornillo.id, Cantidad::UNO, None).unwrap();
        t.bd.borrar_producto(t.martillo.id).unwrap();
        let venta = t.bd.venta(id).unwrap();
        assert_eq!((venta.partidas.len(), venta.totales.total), (1, d("1")));
    }

    #[test]
    fn cobra_asigna_folio_y_descuenta_inventario() {
        let mut t = Tienda::nueva();
        let primera = t.vender(&[(&t.martillo.clone(), "1"), (&t.tornillo.clone(), "1.5")]);
        assert_eq!((primera.folio, primera.estado), (Some(1), Estado::Pagada));
        assert_eq!(t.existencia(&t.tornillo), c("98.5"));

        let segunda = t.vender(&[(&t.martillo.clone(), "1")]);
        assert_eq!(segunda.folio, Some(2));
        assert_eq!(t.existencia(&t.martillo), c("98"));
    }

    #[test]
    fn da_cambio() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        let venta = t.bd.cobrar(id, &Tienda::efectivo("200"), None).unwrap();
        assert_eq!(venta.cambio(), d("84"));
        assert_eq!(t.bd.ticket(id).unwrap().cambio, d("84"));
    }

    #[test]
    fn no_cobra_mal() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        assert!(t.bd.cobrar(id, &Tienda::efectivo("10"), None).is_err()); // vacía
        t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        assert!(t.bd.cobrar(id, &Tienda::efectivo("100"), None).is_err()); // falta
        assert_eq!(t.existencia(&t.martillo), c("100")); // nada cambió
        assert_eq!(t.bd.venta(id).unwrap().estado, Estado::Abierta);
    }

    #[test]
    fn sin_caja_no_se_cobra() {
        let mut t = Tienda::nueva();
        let caja = t.bd.caja_abierta().unwrap().unwrap();
        t.bd.cerrar_caja(caja.id, t.cajera, d("500")).unwrap();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        let error = t.bd.cobrar(id, &Tienda::efectivo("116"), None).unwrap_err();
        assert_eq!(error.to_string(), "Abre la caja antes de cobrar.");
    }

    #[test]
    fn bajo_costo_necesita_autorizacion() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        let partida = t.bd.agregar(id, t.tornillo.id, Cantidad::UNO, None).unwrap().partidas[0].id;
        let contratista = t.tornillo.precios.iter().find(|p| p.nombre == "Contratista").unwrap().id;
        t.bd.elegir_precio(id, partida, Some(contratista)).unwrap();

        let error = t.bd.cobrar(id, &Tienda::efectivo("1"), None).unwrap_err();
        assert!(matches!(&error, Error::BajoCosto(nombres) if nombres == &["Tornillo"]));

        assert!(t.bd.autorizar("2222").is_err()); // la cajera no puede
        let permiso = t.bd.autorizar("1111").unwrap();
        assert_eq!(t.bd.cobrar(id, &Tienda::efectivo("1"), Some(permiso)).unwrap().estado, Estado::Pagada);
    }

    #[test]
    fn pausa_y_retoma() {
        let mut t = Tienda::nueva();
        let primera = t.bd.venta_en_curso(t.cajera).unwrap().id;
        assert!(t.bd.pausar(primera, "").is_err()); // vacía
        t.bd.agregar(primera, t.martillo.id, Cantidad::UNO, None).unwrap();
        t.bd.pausar(primera, "Señor de la camioneta").unwrap();
        assert!(t.bd.agregar(primera, t.martillo.id, Cantidad::UNO, None).is_err());

        let segunda = t.bd.venta_en_curso(t.cajera).unwrap().id;
        assert_ne!(primera, segunda);
        t.bd.agregar(segunda, t.tornillo.id, Cantidad::UNO, None).unwrap();

        let retomada = t.bd.retomar(primera, t.cajera).unwrap();
        assert_eq!((retomada.estado, retomada.nota.as_str()), (Estado::Abierta, "Señor de la camioneta"));
        let pausadas: Vec<i64> = t.bd.ventas_pausadas().unwrap().iter().map(|v| v.id).collect();
        assert_eq!(pausadas, [segunda]); // la que estaba en pantalla quedó en pausa
    }

    #[test]
    fn retomar_tira_la_venta_vacia() {
        let mut t = Tienda::nueva();
        let pausada = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(pausada, t.martillo.id, Cantidad::UNO, None).unwrap();
        t.bd.pausar(pausada, "").unwrap();
        let vacia = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.retomar(pausada, t.cajera).unwrap();
        assert!(matches!(t.bd.venta(vacia), Err(Error::NoEncontrado(_))));
        assert!(t.bd.ventas_pausadas().unwrap().is_empty());
    }

    #[test]
    fn cancelar_regresa_inventario() {
        let mut t = Tienda::nueva();
        let venta = t.vender(&[(&t.martillo.clone(), "3")]);
        assert!(t.bd.descartar(venta.id).is_err());
        let permiso = t.bd.autorizar("1111").unwrap();
        let cancelada = t.bd.cancelar(venta.id, permiso).unwrap();
        assert_eq!(cancelada.estado, Estado::Cancelada);
        assert_eq!(t.existencia(&t.martillo), c("100"));
        assert!(t.bd.cancelar(venta.id, permiso).is_err());
    }

    #[test]
    fn corte_de_caja() {
        let mut t = Tienda::nueva();
        t.vender(&[(&t.martillo.clone(), "1")]); // 116 en efectivo
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, c("2"), None).unwrap();
        let mixto = [
            Cobro { metodo: Metodo::Tarjeta, recibido: d("200") },
            Cobro { metodo: Metodo::Efectivo, recibido: d("50") },
        ];
        t.bd.cobrar(id, &mixto, None).unwrap(); // 232: 200 tarjeta + 32 efectivo, cambio 18

        let caja = t.bd.caja_abierta().unwrap().unwrap();
        let corte = t.bd.cerrar_caja(caja.id, t.cajera, d("640")).unwrap();
        assert_eq!((corte.ventas, corte.total), (2, d("348")));
        assert_eq!(corte.efectivo_esperado, d("648")); // 500 + 116 + 32
        assert_eq!(corte.diferencia, Some(d("-8")));
        assert!(t.bd.caja_abierta().unwrap().is_none());
        assert!(t.bd.cerrar_caja(caja.id, t.cajera, d("1")).is_err());
    }

    mod farmacia {
        use super::*;
        use crate::devoluciones::ADevolver;
        use crate::lotes::Origen;
        use crate::lotes::tests::{en_dias, farmacia, lote};

        /// Una farmacia con Paracetamol en dos lotes: A (10, caduca primero) y B (5).
        fn con_lotes() -> (Tienda, Producto, i64, i64) {
            let mut t = Tienda::nueva();
            farmacia(&mut t.bd);
            let p = t.bd.crear_producto(&datos("PAR", "Paracetamol", "35", "12")).unwrap();
            let a = t.bd.agregar_lote(p.id, &lote("A", en_dias(30)), c("10"), Origen::Entrada, "").unwrap().id;
            let b = t.bd.agregar_lote(p.id, &lote("B", en_dias(300)), c("5"), Origen::Entrada, "").unwrap().id;
            (t, p, a, b)
        }

        fn existencia_lote(t: &Tienda, lote: i64) -> Cantidad {
            t.bd.lote(lote).unwrap().existencia
        }

        #[test]
        fn cada_lote_en_su_partida_y_cada_uno_baja() {
            let (mut t, p, a, b) = con_lotes();
            let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
            t.bd.agregar_de_lote(id, p.id, Some(a), c("2"), None).unwrap();
            t.bd.agregar_de_lote(id, p.id, Some(b), c("1"), None).unwrap();
            let venta = t.bd.agregar_de_lote(id, p.id, Some(a), c("1"), None).unwrap();
            let lineas: Vec<_> = venta.partidas.iter().map(|l| (l.lote.clone().unwrap(), l.cantidad)).collect();
            assert_eq!(lineas, [("A".to_string(), c("3")), ("B".to_string(), c("1"))]);
            assert_eq!(venta.partidas[0].caducidad, Some(en_dias(30)));

            let partida_b = venta.partidas[1].id;
            assert_eq!(t.bd.cambiar_cantidad(id, partida_b, c("2")).unwrap().partidas[1].cantidad, c("2"));
            t.bd.cobrar(id, &Tienda::efectivo("175"), None).unwrap();
            assert_eq!((existencia_lote(&t, a), existencia_lote(&t, b)), (c("7"), c("3")));
            assert_eq!(t.existencia(&p), c("10"));
        }

        #[test]
        fn lo_que_va_con_receta_pide_confirmarla() {
            let mut t = Tienda::nueva();
            farmacia(&mut t.bd);
            let amoxicilina = DatosProducto { con_receta: true, ..datos("AMX", "Amoxicilina", "89", "38") };
            let amx = t.bd.crear_producto(&amoxicilina).unwrap();
            assert!(amx.con_receta);
            t.bd.registrar_entrada(amx.id, c("5"), "").unwrap();
            let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
            let venta = t.bd.agregar(id, amx.id, c("1"), None).unwrap();
            assert!(venta.partidas[0].con_receta);

            let sin = t.bd.cobrar(id, &Tienda::efectivo("100"), None).unwrap_err();
            assert_eq!(
                sin.to_string(),
                "Se vende solo con receta: Amoxicilina. Confirma que el cliente presentó una receta médica válida."
            );
            assert_eq!(t.existencia(&amx), c("5"));
            t.bd.cobrar_con_receta(id, &Tienda::efectivo("100"), None, true).unwrap();
            assert_eq!(t.existencia(&amx), c("4"));

            // Fuera de una farmacia la marca no estorba.
            t.bd.guardar_negocio(&crate::negocio::Negocio {
                giro: crate::negocio::Giro::Abarrotes,
                ..t.bd.negocio().unwrap()
            })
            .unwrap();
            let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
            t.bd.agregar(id, amx.id, c("1"), None).unwrap();
            t.bd.cobrar(id, &Tienda::efectivo("100"), None).unwrap();
        }

        #[test]
        fn no_vende_de_mas_ni_caducado() {
            let (mut t, p, a, _) = con_lotes();
            let viejo = t.bd.agregar_lote(p.id, &lote("V", en_dias(-1)), c("3"), Origen::Entrada, "").unwrap().id;
            let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
            let caducado = t.bd.agregar_de_lote(id, p.id, Some(viejo), c("1"), None).unwrap_err();
            assert!(caducado.to_string().contains("caducó"), "{caducado}");
            let de_mas = t.bd.agregar_de_lote(id, p.id, Some(a), c("11"), None).unwrap_err();
            assert_eq!(de_mas.to_string(), "Del lote A de Paracetamol solo quedan 10 pza.");
            let ajeno = t.bd.agregar_de_lote(id, t.martillo.id, Some(a), c("1"), None);
            assert!(matches!(ajeno, Err(Error::NoEncontrado(_))));

            // Otra venta se lleva el lote mientras esta espera: al cobrar ya no alcanza.
            t.bd.agregar_de_lote(id, p.id, Some(a), c("8"), None).unwrap();
            t.bd.pausar(id, "").unwrap();
            t.vender_de_lote(&p, a, "5");
            t.bd.retomar(id, t.cajera).unwrap();
            let error = t.bd.cobrar(id, &Tienda::efectivo("280"), None).unwrap_err();
            assert_eq!(error.to_string(), "Del lote A de Paracetamol solo quedan 5 pza.");
            assert_eq!(existencia_lote(&t, a), c("5"));
        }

        #[test]
        fn sin_lote_solo_lo_que_hay_sin_lote() {
            let (mut t, p, a, _) = con_lotes();
            let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
            t.bd.agregar(id, p.id, c("1"), None).unwrap();
            let error = t.bd.cobrar(id, &Tienda::efectivo("35"), None).unwrap_err();
            assert_eq!(error.to_string(), "Elige el lote de Paracetamol.");

            t.bd.registrar_entrada(p.id, c("2"), "Sin lote").unwrap();
            let venta = t.bd.agregar(id, p.id, c("2"), None).unwrap();
            assert!(t.bd.cobrar(id, &Tienda::efectivo("105"), None).unwrap_err().to_string().contains("solo hay 2"));

            // Elegir el lote de la partida; si ya había una de ese lote, se juntan.
            let partida = venta.partidas[0].id;
            let venta = t.bd.elegir_lote(id, partida, Some(a)).unwrap();
            assert_eq!(venta.partidas[0].lote_id, Some(a));
            t.bd.agregar(id, p.id, c("1"), None).unwrap();
            let suelta = t.bd.venta(id).unwrap().partidas[1].id;
            let venta = t.bd.elegir_lote(id, suelta, Some(a)).unwrap();
            assert_eq!((venta.partidas.len(), venta.partidas[0].cantidad), (1, c("4")));
            t.bd.cobrar(id, &Tienda::efectivo("140"), None).unwrap();
            assert_eq!((existencia_lote(&t, a), t.bd.existencia_sin_lote(p.id).unwrap()), (c("6"), c("2")));
        }

        #[test]
        fn cancelar_y_devolver_regresan_al_lote() {
            let (mut t, p, a, b) = con_lotes();
            let venta = t.vender_de_lote(&p, a, "4");
            let permiso = t.bd.autorizar("1111").unwrap();
            t.bd.cancelar(venta.id, permiso).unwrap();
            assert_eq!(existencia_lote(&t, a), c("10"));

            let venta = t.vender_de_lote(&p, b, "5");
            assert!(t.bd.lotes(p.id).unwrap().iter().all(|l| l.id != b)); // agotado: ya no se ofrece
            let dos = ADevolver { partida_id: venta.partidas[0].id, cantidad: c("2") };
            t.bd.devolver(venta.id, t.cajera, &[dos], Metodo::Efectivo, "", permiso).unwrap();
            assert_eq!(existencia_lote(&t, b), c("2"));
            assert_eq!(t.existencia(&p), c("12"));
        }

        #[test]
        fn fuera_de_farmacia_se_vende_como_siempre() {
            let (mut t, p, _, _) = con_lotes();
            let negocio = t.bd.negocio().unwrap();
            t.bd.guardar_negocio(&Negocio { giro: crate::negocio::Giro::Ferreteria, ..negocio }).unwrap();
            t.vender(&[(&p, "3")]);
            assert_eq!(t.existencia(&p), c("12"));
        }

        #[test]
        fn la_cotizacion_conserva_el_lote_si_alcanza() {
            let (mut t, p, a, b) = con_lotes();
            let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
            t.bd.agregar_de_lote(id, p.id, Some(a), c("3"), None).unwrap();
            t.bd.agregar_de_lote(id, p.id, Some(b), c("4"), None).unwrap();
            let cotizacion = t.bd.cotizar(id, 7, "").unwrap();
            t.vender_de_lote(&p, b, "2"); // de B ya solo quedan 3

            let venta = t.bd.convertir_cotizacion(cotizacion.id, t.cajera).unwrap();
            let lineas: Vec<_> = venta.partidas.iter().map(|l| (l.lote_id, l.cantidad)).collect();
            assert_eq!(lineas, [(Some(a), c("3")), (None, c("4"))]);
        }
    }

    #[test]
    fn historial_por_dia() {
        let mut t = Tienda::nueva();
        t.vender(&[(&t.martillo.clone(), "1")]);
        t.vender(&[(&t.martillo.clone(), "1")]);
        let hoy = ahora().date();
        let folios: Vec<_> = t.bd.historial(hoy, hoy).unwrap().iter().map(|v| v.folio).collect();
        assert_eq!(folios, [Some(2), Some(1)]);
        assert!(t.bd.historial(hoy.pred_opt().unwrap(), hoy.pred_opt().unwrap()).unwrap().is_empty());
    }
}
