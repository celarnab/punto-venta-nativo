//! Lotes con caducidad, para farmacias. Cada lote guarda la parte de la existencia del producto que le toca; lo
//! que no está en ningún lote es existencia «sin lote». Un lote caducado ya no se vende: se retira del
//! inventario y su costo cuenta como pérdida en los reportes.

use chrono::{Datelike, Days, Months, NaiveDate, NaiveDateTime};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::catalogo::{self, TipoMovimiento};
use crate::error::duplicado;
use crate::{Bd, Cantidad, Dinero, Error, Resultado, ahora};

/// Con cuánta anticipación se avisa que un lote va a caducar.
pub const DIAS_AVISO: u64 = 60;

enum_texto! {
    pub enum EstadoLote {
        Vigente = "vigente", "Vigente";
        PorCaducar = "por_caducar", "Por caducar";
        Caducado = "caducado", "Caducado";
        Retirado = "retirado", "Retirado";
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Lote {
    pub id: i64,
    pub producto_id: i64,
    pub codigo: String,
    pub producto: String,
    pub unidad: String,
    pub numero: String,
    pub fabricacion: Option<NaiveDate>,
    /// Se vende hasta este día, inclusive.
    pub caducidad: NaiveDate,
    pub existencia: Cantidad,
    /// El costo unitario actual del producto: lo que se perdería al retirarlo.
    pub costo: Dinero,
    pub retirado_en: Option<NaiveDateTime>,
}

impl Lote {
    pub fn caducado(&self, hoy: NaiveDate) -> bool {
        self.caducidad < hoy
    }

    pub fn estado(&self, hoy: NaiveDate) -> EstadoLote {
        if self.retirado_en.is_some() {
            EstadoLote::Retirado
        } else if self.caducado(hoy) {
            EstadoLote::Caducado
        } else if self.caducidad <= hoy + Days::new(DIAS_AVISO) {
            EstadoLote::PorCaducar
        } else {
            EstadoLote::Vigente
        }
    }

    /// Lo que costaría retirarlo hoy.
    pub fn perdida(&self) -> Dinero {
        self.existencia * self.costo
    }

    fn de_fila(fila: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: fila.get("id")?,
            producto_id: fila.get("producto_id")?,
            codigo: fila.get("codigo")?,
            producto: fila.get("producto")?,
            unidad: fila.get("unidad")?,
            numero: fila.get("numero")?,
            fabricacion: fila.get("fabricacion")?,
            caducidad: fila.get("caducidad")?,
            existencia: fila.get("existencia")?,
            costo: fila.get("costo")?,
            retirado_en: fila.get("retirado_en")?,
        })
    }
}

/// Lo que se captura de un lote.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DatosLote {
    pub numero: String,
    pub fabricacion: Option<NaiveDate>,
    pub caducidad: NaiveDate,
}

impl DatosLote {
    fn validar(&self) -> Resultado<&str> {
        let numero = self.numero.trim();
        if numero.is_empty() {
            return Err(Error::invalido("Falta el número de lote."));
        }
        if self.fabricacion.is_some_and(|f| f > self.caducidad) {
            return Err(Error::invalido("La fecha de fabricación es posterior a la de caducidad."));
        }
        Ok(numero)
    }
}

/// De dónde sale la mercancía de un lote nuevo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Origen {
    /// Llegó mercancía: la existencia del producto sube.
    Entrada,
    /// Ya estaba en la existencia, sin lote: solo se le asigna el lote.
    Existencia,
}

/// Lee una fecha como se escribe en una caja de medicina: «15/03/2027», «03/2027» (el último día de ese mes) o
/// «2027-03-15». Vacío = sin fecha.
pub fn leer_fecha(texto: &str) -> Resultado<Option<NaiveDate>> {
    let texto = texto.trim().replace(['-', '.'], "/");
    if texto.is_empty() {
        return Ok(None);
    }
    let invalida = || Error::invalido(format!("«{}» no es una fecha (usa dd/mm/aaaa).", texto.trim()));
    let partes: Vec<&str> = texto.split('/').collect();
    let numero = |t: &str| t.trim().parse::<u32>().map_err(|_| invalida());
    let fecha = match partes.as_slice() {
        [mes, anio] => {
            let primero = NaiveDate::from_ymd_opt(numero(anio)? as i32, numero(mes)?, 1).ok_or_else(invalida)?;
            primero.checked_add_months(Months::new(1)).and_then(|d| d.pred_opt())
        }
        [anio, mes, dia] if anio.len() == 4 => {
            NaiveDate::from_ymd_opt(numero(anio)? as i32, numero(mes)?, numero(dia)?)
        }
        [dia, mes, anio] if anio.len() == 4 => {
            NaiveDate::from_ymd_opt(numero(anio)? as i32, numero(mes)?, numero(dia)?)
        }
        _ => None,
    };
    let fecha = fecha.ok_or_else(invalida)?;
    if !(2000..=2100).contains(&fecha.year()) {
        return Err(invalida());
    }
    Ok(Some(fecha))
}

const SELECCION: &str = "
    SELECT l.*, p.codigo, p.nombre AS producto, p.unidad, p.costo
    FROM lotes l JOIN productos p ON p.id = l.producto_id";

impl Bd {
    /// Los lotes con existencia del producto (también los caducados), el que caduca primero al principio.
    pub fn lotes(&self, producto_id: i64) -> Resultado<Vec<Lote>> {
        consultar(
            self.conn(),
            &format!(
                "{SELECCION} WHERE l.producto_id = ?1 AND l.retirado_en IS NULL AND l.existencia > 0
                 ORDER BY l.caducidad, l.numero"
            ),
            [producto_id],
        )
    }

    /// Los que se le ofrecen al cajero: con existencia y sin caducar.
    pub fn lotes_a_la_venta(&self, producto_id: i64) -> Resultado<Vec<Lote>> {
        let hoy = ahora().date();
        Ok(self.lotes(producto_id)?.into_iter().filter(|l| !l.caducado(hoy)).collect())
    }

    pub fn lote(&self, id: i64) -> Resultado<Lote> {
        cargar(self.conn(), id)
    }

    /// La existencia del producto que no está en ningún lote.
    pub fn existencia_sin_lote(&self, producto_id: i64) -> Resultado<Cantidad> {
        sin_lote(self.conn(), producto_id)
    }

    /// Lotes caducados o que caducan en los próximos [`DIAS_AVISO`] días, de productos que siguen en el catálogo.
    pub fn avisos_de_caducidad(&self) -> Resultado<Vec<Lote>> {
        let limite = ahora().date() + Days::new(DIAS_AVISO);
        consultar(
            self.conn(),
            &format!(
                "{SELECCION} WHERE l.retirado_en IS NULL AND l.existencia > 0 AND p.borrado_en IS NULL
                   AND l.caducidad <= ?1
                 ORDER BY l.caducidad, p.nombre"
            ),
            [limite],
        )
    }

    /// Cuántos lotes caducados quedan por retirar.
    pub fn lotes_caducados(&self) -> Resultado<i64> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM lotes l JOIN productos p ON p.id = l.producto_id
             WHERE l.retirado_en IS NULL AND l.existencia > 0 AND p.borrado_en IS NULL AND l.caducidad < ?1",
            [ahora().date()],
            |f| f.get(0),
        )?)
    }

    /// Agrega `cantidad` al lote `datos.numero` del producto (lo crea si no existe).
    pub fn agregar_lote(
        &mut self,
        producto_id: i64,
        datos: &DatosLote,
        cantidad: Cantidad,
        origen: Origen,
        referencia: &str,
    ) -> Resultado<Lote> {
        let numero = datos.validar()?;
        if !cantidad.es_positivo() {
            return Err(Error::invalido("La cantidad del lote debe ser mayor que cero."));
        }
        self.transaccion(|tx| {
            let producto = catalogo::cargar(tx, producto_id)?;
            let existente: Option<Lote> = consultar(
                tx,
                &format!("{SELECCION} WHERE l.producto_id = ?1 AND l.numero = ?2"),
                params![producto_id, numero],
            )?
            .pop();
            let lote_id = match existente {
                Some(lote) if lote.existencia.es_positivo() && lote.caducidad != datos.caducidad => {
                    return Err(Error::invalido(format!(
                        "El lote {} ya existe con caducidad {}.",
                        lote.numero,
                        lote.caducidad.format("%d/%m/%Y")
                    )));
                }
                Some(lote) => {
                    // Vacío o retirado: se reutiliza con las fechas nuevas.
                    tx.execute(
                        "UPDATE lotes SET caducidad = ?2, fabricacion = coalesce(?3, fabricacion), retirado_en = NULL
                         WHERE id = ?1",
                        params![lote.id, datos.caducidad, datos.fabricacion],
                    )?;
                    lote.id
                }
                None => {
                    tx.execute(
                        "INSERT INTO lotes (producto_id, numero, fabricacion, caducidad) VALUES (?1, ?2, ?3, ?4)",
                        params![producto_id, numero, datos.fabricacion, datos.caducidad],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            match origen {
                Origen::Entrada => {
                    let referencia = match referencia.trim() {
                        "" => format!("Lote {numero}"),
                        r => format!("{r} · lote {numero}"),
                    };
                    catalogo::mover_en(
                        tx,
                        producto_id,
                        Some(lote_id),
                        cantidad,
                        TipoMovimiento::Entrada,
                        &referencia,
                        None,
                    )?;
                }
                Origen::Existencia => {
                    let libre = sin_lote(tx, producto_id)?;
                    if cantidad > libre {
                        return Err(Error::invalido(format!(
                            "Sin lote solo hay {libre} {}: registra el resto como mercancía que llegó.",
                            producto.unidad
                        )));
                    }
                    tx.execute(
                        "UPDATE lotes SET existencia = existencia + ?2 WHERE id = ?1",
                        params![lote_id, cantidad],
                    )?;
                }
            }
            cargar(tx, lote_id)
        })
    }

    /// Corrige el número o las fechas.
    pub fn editar_lote(&mut self, id: i64, datos: &DatosLote) -> Resultado<Lote> {
        let numero = datos.validar()?;
        let cambiados = self
            .conn()
            .execute(
                "UPDATE lotes SET numero = ?2, fabricacion = ?3, caducidad = ?4 WHERE id = ?1",
                params![id, numero, datos.fabricacion, datos.caducidad],
            )
            .map_err(duplicado(|| format!("Este producto ya tiene un lote {numero}.")))?;
        if cambiados == 0 {
            return Err(Error::NoEncontrado("Lote"));
        }
        self.lote(id)
    }

    /// Conteo físico de un lote: la diferencia queda como ajuste del lote y del producto.
    pub fn contar_lote(&mut self, id: i64, contada: Cantidad) -> Resultado<Lote> {
        if contada < Cantidad::CERO {
            return Err(Error::invalido("La cantidad contada no puede ser negativa."));
        }
        self.transaccion(|tx| {
            let lote = cargar(tx, id)?;
            let diferencia = contada - lote.existencia;
            if diferencia != Cantidad::CERO {
                let referencia = format!("Conteo del lote {}", lote.numero);
                catalogo::mover_en(
                    tx,
                    lote.producto_id,
                    Some(id),
                    diferencia,
                    TipoMovimiento::Ajuste,
                    &referencia,
                    None,
                )?;
            }
            cargar(tx, id)
        })
    }

    /// Saca del inventario lo que queda del lote, como pérdida por caducidad (a su costo de hoy). Sirve también
    /// para uno que aún no caduca, p. ej. si se regresa al proveedor.
    pub fn retirar_lote(&mut self, id: i64) -> Resultado<Lote> {
        self.transaccion(|tx| {
            let lote = cargar(tx, id)?;
            if lote.retirado_en.is_some() || !lote.existencia.es_positivo() {
                return Err(Error::invalido(format!("El lote {} ya no tiene existencia.", lote.numero)));
            }
            let referencia = format!("Caducidad del lote {}", lote.numero);
            let cantidad = Cantidad::crudo(-lote.existencia.como_crudo());
            catalogo::mover_en(
                tx,
                lote.producto_id,
                Some(id),
                cantidad,
                TipoMovimiento::Caducidad,
                &referencia,
                Some(lote.costo),
            )?;
            tx.execute("UPDATE lotes SET retirado_en = ?2 WHERE id = ?1", params![id, ahora()])?;
            cargar(tx, id)
        })
    }

    /// Todos los lotes que tienen o tuvieron existencia, para exportarlos.
    pub fn todos_los_lotes(&self) -> Resultado<Vec<Lote>> {
        consultar(
            self.conn(),
            &format!(
                "{SELECCION} WHERE p.borrado_en IS NULL AND (l.existencia > 0 OR l.retirado_en IS NOT NULL)
                 ORDER BY p.nombre, l.caducidad"
            ),
            [],
        )
    }
}

pub(crate) fn cargar(conn: &Connection, id: i64) -> Resultado<Lote> {
    consultar(conn, &format!("{SELECCION} WHERE l.id = ?1"), [id])?.pop().ok_or(Error::NoEncontrado("Lote"))
}

fn consultar(conn: &Connection, sql: &str, parametros: impl rusqlite::Params) -> Resultado<Vec<Lote>> {
    let mut consulta = conn.prepare_cached(sql)?;
    let lotes = consulta.query_map(parametros, Lote::de_fila)?.collect::<rusqlite::Result<_>>()?;
    Ok(lotes)
}

/// Lo que el producto tiene repartido en lotes que no se han retirado.
pub(crate) fn en_lotes(conn: &Connection, producto_id: i64) -> Resultado<Cantidad> {
    Ok(conn.query_row(
        "SELECT coalesce(sum(existencia), 0) FROM lotes WHERE producto_id = ?1 AND retirado_en IS NULL",
        [producto_id],
        |f| f.get(0),
    )?)
}

pub(crate) fn sin_lote(conn: &Connection, producto_id: i64) -> Resultado<Cantidad> {
    let existencia: Cantidad =
        conn.query_row("SELECT existencia FROM productos WHERE id = ?1", [producto_id], |f| f.get(0))?;
    Ok((existencia - en_lotes(conn, producto_id)?).max(Cantidad::CERO))
}

/// ¿Se pueden vender `cantidad` del lote `lote_id` de este producto? Devuelve el porqué si no.
pub(crate) fn revisar_venta(conn: &Connection, producto_id: i64, lote_id: i64, cantidad: Cantidad) -> Resultado<()> {
    let lote = conn
        .query_row(
            &format!("{SELECCION} WHERE l.id = ?1 AND l.producto_id = ?2"),
            [lote_id, producto_id],
            Lote::de_fila,
        )
        .optional()?
        .ok_or(Error::NoEncontrado("Lote"))?;
    if lote.retirado_en.is_some() || !lote.existencia.es_positivo() {
        return Err(Error::invalido(format!("El lote {} de {} ya no tiene existencia.", lote.numero, lote.producto)));
    }
    if lote.caducado(ahora().date()) {
        return Err(Error::invalido(format!(
            "El lote {} de {} caducó el {}: no se puede vender.",
            lote.numero,
            lote.producto,
            lote.caducidad.format("%d/%m/%Y")
        )));
    }
    if cantidad > lote.existencia {
        return Err(Error::invalido(format!(
            "Del lote {} de {} solo quedan {} {}.",
            lote.numero, lote.producto, lote.existencia, lote.unidad
        )));
    }
    Ok(())
}

/// Una partida sin lote de un producto que tiene lotes solo puede llevar lo que hay sin lote.
pub(crate) fn revisar_sin_lote(conn: &Connection, producto_id: i64, cantidad: Cantidad) -> Resultado<()> {
    if !en_lotes(conn, producto_id)?.es_positivo() {
        return Ok(()); // sin lotes se vende como siempre
    }
    let libre = sin_lote(conn, producto_id)?;
    if cantidad <= libre {
        return Ok(());
    }
    let (nombre, unidad): (String, String) =
        conn.query_row("SELECT nombre, unidad FROM productos WHERE id = ?1", [producto_id], |f| {
            Ok((f.get(0)?, f.get(1)?))
        })?;
    Err(Error::invalido(if libre.es_positivo() {
        format!("De {nombre} solo hay {libre} {unidad} sin lote: elige el lote del resto.")
    } else {
        format!("Elige el lote de {nombre}.")
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::catalogo::tests::{c, d, datos};
    use crate::negocio::Giro;

    pub(crate) fn lote(numero: &str, caducidad: NaiveDate) -> DatosLote {
        DatosLote { numero: numero.into(), fabricacion: None, caducidad }
    }

    pub(crate) fn en_dias(dias: i64) -> NaiveDate {
        let hoy = ahora().date();
        if dias >= 0 { hoy + Days::new(dias as u64) } else { hoy - Days::new((-dias) as u64) }
    }

    pub(crate) fn farmacia(bd: &mut Bd) {
        let negocio = bd.negocio().unwrap();
        bd.guardar_negocio(&crate::negocio::Negocio { giro: Giro::Farmacia, ..negocio }).unwrap();
    }

    fn fecha(texto: &str) -> NaiveDate {
        texto.parse().unwrap()
    }

    #[test]
    fn lee_fechas_de_caja() {
        assert_eq!(leer_fecha("15/03/2027").unwrap(), Some(fecha("2027-03-15")));
        assert_eq!(leer_fecha(" 03/2027 ").unwrap(), Some(fecha("2027-03-31")));
        assert_eq!(leer_fecha("2/2028").unwrap(), Some(fecha("2028-02-29")));
        assert_eq!(leer_fecha("12/2027").unwrap(), Some(fecha("2027-12-31")));
        assert_eq!(leer_fecha("2027-03-15").unwrap(), Some(fecha("2027-03-15")));
        assert_eq!(leer_fecha("15-03-2027").unwrap(), Some(fecha("2027-03-15")));
        assert_eq!(leer_fecha("").unwrap(), None);
        for mala in ["31/02/2027", "13/2027", "mañana", "15/03/27", "1/1/1990"] {
            assert!(leer_fecha(mala).is_err(), "{mala}");
        }
    }

    #[test]
    fn entrada_y_existencia_previa() {
        let mut bd = Bd::en_memoria().unwrap();
        let p = bd.crear_producto(&datos("PAR", "Paracetamol 500 mg", "35", "12")).unwrap();
        bd.registrar_entrada(p.id, c("30"), "Inventario anterior").unwrap();

        // 20 de las 30 que ya había son del lote A.
        let a = bd.agregar_lote(p.id, &lote("a-01", en_dias(200)), c("20"), Origen::Existencia, "").unwrap();
        assert_eq!((a.numero.as_str(), a.existencia), ("a-01", c("20")));
        assert_eq!(bd.producto(p.id).unwrap().existencia, c("30"));
        assert_eq!(bd.existencia_sin_lote(p.id).unwrap(), c("10"));
        let error = bd.agregar_lote(p.id, &lote("B", en_dias(300)), c("11"), Origen::Existencia, "").unwrap_err();
        assert!(error.to_string().contains("solo hay 10"));

        // Llega mercancía de otro lote, y más del mismo lote A (el número no distingue mayúsculas).
        bd.agregar_lote(p.id, &lote("B-7", en_dias(90)), c("12"), Origen::Entrada, "Factura 5").unwrap();
        bd.agregar_lote(p.id, &lote("A-01", en_dias(200)), c("5"), Origen::Entrada, "").unwrap();
        assert_eq!(bd.producto(p.id).unwrap().existencia, c("47"));
        let lotes: Vec<_> = bd.lotes(p.id).unwrap().into_iter().map(|l| (l.numero, l.existencia)).collect();
        assert_eq!(lotes, [("B-7".to_string(), c("12")), ("a-01".to_string(), c("25"))]); // primero el que caduca antes
        let movimiento = &bd.movimientos(p.id, 1).unwrap()[0];
        assert_eq!(movimiento.referencia, "Lote A-01");

        // El mismo número con otra caducidad es un error de captura.
        let otra_fecha = bd.agregar_lote(p.id, &lote("A-01", en_dias(10)), c("1"), Origen::Entrada, "");
        assert!(otra_fecha.unwrap_err().to_string().contains("ya existe con caducidad"));
        assert!(bd.agregar_lote(p.id, &lote(" ", en_dias(10)), c("1"), Origen::Entrada, "").is_err());
        assert!(bd.agregar_lote(p.id, &lote("C", en_dias(10)), c("0"), Origen::Entrada, "").is_err());
        let al_reves = DatosLote { fabricacion: Some(en_dias(20)), ..lote("C", en_dias(10)) };
        assert!(bd.agregar_lote(p.id, &al_reves, c("1"), Origen::Entrada, "").is_err());
    }

    #[test]
    fn editar_y_contar() {
        let mut bd = Bd::en_memoria().unwrap();
        let p = bd.crear_producto(&datos("AMX", "Amoxicilina", "80", "30")).unwrap();
        let a = bd.agregar_lote(p.id, &lote("A", en_dias(100)), c("10"), Origen::Entrada, "").unwrap();
        bd.agregar_lote(p.id, &lote("B", en_dias(100)), c("5"), Origen::Entrada, "").unwrap();

        let editado = DatosLote { fabricacion: Some(en_dias(-30)), ..lote("A2", en_dias(120)) };
        let a = bd.editar_lote(a.id, &editado).unwrap();
        assert_eq!((a.numero.as_str(), a.caducidad, a.fabricacion), ("A2", en_dias(120), Some(en_dias(-30))));
        assert!(bd.editar_lote(a.id, &lote("b", en_dias(1))).unwrap_err().to_string().contains("ya tiene un lote b"));

        let a = bd.contar_lote(a.id, c("7")).unwrap();
        assert_eq!((a.existencia, bd.producto(p.id).unwrap().existencia), (c("7"), c("12")));
        assert_eq!(bd.movimientos(p.id, 1).unwrap()[0].cantidad, c("-3"));
        assert!(bd.contar_lote(a.id, c("-1")).is_err());

        // El conteo general no puede dejar menos de lo que hay en lotes.
        assert!(bd.ajustar_existencia(p.id, c("11"), "Conteo").unwrap_err().to_string().contains("en lotes"));
        assert_eq!(bd.ajustar_existencia(p.id, c("14"), "Conteo").unwrap().existencia, c("14"));
        assert_eq!(bd.existencia_sin_lote(p.id).unwrap(), c("2"));

        // Un lote que llega a cero desaparece de la lista.
        bd.contar_lote(a.id, c("0")).unwrap();
        assert_eq!(bd.lotes(p.id).unwrap().len(), 1);
    }

    #[test]
    fn retirar_lo_caducado() {
        let mut bd = Bd::en_memoria().unwrap();
        let p = bd.crear_producto(&datos("JRB", "Jarabe", "60", "25")).unwrap();
        let viejo = bd.agregar_lote(p.id, &lote("V", en_dias(-1)), c("4"), Origen::Entrada, "").unwrap();
        let pronto = bd.agregar_lote(p.id, &lote("P", en_dias(30)), c("2"), Origen::Entrada, "").unwrap();
        bd.agregar_lote(p.id, &lote("N", en_dias(365)), c("6"), Origen::Entrada, "").unwrap();

        let hoy = ahora().date();
        assert_eq!(viejo.estado(hoy), EstadoLote::Caducado);
        assert_eq!(pronto.estado(hoy), EstadoLote::PorCaducar);
        let avisos: Vec<_> = bd.avisos_de_caducidad().unwrap().into_iter().map(|l| l.numero).collect();
        assert_eq!(avisos, ["V", "P"]);
        assert_eq!(bd.lotes_caducados().unwrap(), 1);
        let a_la_venta: Vec<_> = bd.lotes_a_la_venta(p.id).unwrap().into_iter().map(|l| l.numero).collect();
        assert_eq!(a_la_venta, ["P", "N"]);
        assert_eq!(viejo.perdida(), d("100"));

        let retirado = bd.retirar_lote(viejo.id).unwrap();
        assert_eq!((retirado.existencia, retirado.estado(hoy)), (c("0"), EstadoLote::Retirado));
        assert_eq!(bd.producto(p.id).unwrap().existencia, c("8"));
        let m = &bd.movimientos(p.id, 1).unwrap()[0];
        assert_eq!(
            (m.tipo, m.cantidad, m.referencia.as_str()),
            (TipoMovimiento::Caducidad, c("-4"), "Caducidad del lote V")
        );
        assert_eq!(bd.lotes_caducados().unwrap(), 0);
        assert!(bd.retirar_lote(viejo.id).is_err());

        let exportados: Vec<_> = bd.todos_los_lotes().unwrap().into_iter().map(|l| l.numero).collect();
        assert_eq!(exportados, ["V", "P", "N"]);
    }
}
