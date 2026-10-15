//! Productos, familias, precios de mayoreo y existencias.

use chrono::NaiveDateTime;
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};
use serde::{Deserialize, Serialize};

use crate::error::duplicado;
use crate::texto::palabras_json;
use crate::{Bd, Cantidad, Dinero, Error, Resultado, ahora};

// — Familias —

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Familia {
    pub id: i64,
    pub nombre: String,
    /// Avisa cuando a un producto de la familia le queden esta cantidad o menos.
    pub alerta_stock: Option<Cantidad>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DatosFamilia {
    pub nombre: String,
    pub alerta_stock: Option<Cantidad>,
}

// — Productos —

/// Un precio distinto del normal: con `cantidad_minima` se aplica solo (mayoreo);
/// sin ella, el cajero lo elige a mano (p. ej. «Contratista»).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtroPrecio {
    #[serde(default)]
    pub id: i64,
    pub nombre: String,
    pub precio: Dinero,
    pub cantidad_minima: Option<Cantidad>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Producto {
    pub id: i64,
    pub codigo: String,
    /// Opcional; el escáner y la búsqueda también lo encuentran por él.
    pub codigo_barras: Option<String>,
    pub nombre: String,
    pub familia_id: Option<i64>,
    pub familia: Option<String>,
    /// IVA incluido.
    pub precio: Dinero,
    pub costo: Dinero,
    pub existencia: Cantidad,
    pub stock_minimo: Option<Cantidad>,
    pub stock_maximo: Option<Cantidad>,
    pub alerta_stock: Option<Cantidad>,
    /// El aviso que aplica: el del producto o, si no tiene, el de su familia.
    pub umbral_alerta: Option<Cantidad>,
    pub activo: bool,
    /// A granel: se vende por kilo (con decimales) y la existencia se cuenta en kilos.
    pub granel: bool,
    /// En la que se vende y se cuenta la existencia: «pza», «m», «caja»… o «kg» si es a granel.
    pub unidad: String,
    pub precios: Vec<OtroPrecio>,
}

impl Producto {
    /// El precio de mayoreo que corresponde a `cantidad` (el escalón más alto que alcanza), si hay.
    /// Es una sugerencia: el precio de una venta siempre lo decide el cajero.
    pub fn sugerido(&self, cantidad: Cantidad) -> Option<&OtroPrecio> {
        self.precios
            .iter()
            .filter(|p| p.cantidad_minima.is_some_and(|minima| minima <= cantidad))
            .max_by_key(|p| p.cantidad_minima)
    }

    /// Ganancia sobre el precio normal, en %.
    pub fn margen(&self) -> Option<f64> {
        self.precio.es_positivo().then(|| (self.precio - self.costo).como_f64() / self.precio.como_f64() * 100.0)
    }

    /// Cuánto comprar para llegar al máximo.
    pub fn a_pedir(&self) -> Option<Cantidad> {
        self.stock_maximo.filter(|&maximo| self.existencia < maximo).map(|maximo| maximo - self.existencia)
    }

    pub fn en_alerta(&self) -> bool {
        self.umbral_alerta.is_some_and(|umbral| self.existencia <= umbral)
    }

    fn de_fila(fila: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: fila.get("id")?,
            codigo: fila.get("codigo")?,
            codigo_barras: fila.get("codigo_barras")?,
            nombre: fila.get("nombre")?,
            familia_id: fila.get("familia_id")?,
            familia: fila.get("familia")?,
            precio: fila.get("precio")?,
            costo: fila.get("costo")?,
            existencia: fila.get("existencia")?,
            stock_minimo: fila.get("stock_minimo")?,
            stock_maximo: fila.get("stock_maximo")?,
            alerta_stock: fila.get("alerta_stock")?,
            umbral_alerta: fila.get("umbral_alerta")?,
            activo: fila.get("activo")?,
            granel: fila.get("granel")?,
            unidad: fila.get("unidad")?,
            precios: Vec::new(),
        })
    }
}

/// Lo que se captura al dar de alta o editar un producto. La existencia no: esa cambia por movimientos.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct DatosProducto {
    pub codigo: String,
    /// Vacío = sin código de barras.
    #[serde(default)]
    pub codigo_barras: String,
    pub nombre: String,
    pub familia_id: Option<i64>,
    pub precio: Dinero,
    pub costo: Dinero,
    pub stock_minimo: Option<Cantidad>,
    pub stock_maximo: Option<Cantidad>,
    pub alerta_stock: Option<Cantidad>,
    pub activo: bool,
    #[serde(default)]
    pub granel: bool,
    /// Vacía = «pza». A granel siempre es «kg».
    #[serde(default)]
    pub unidad: String,
    #[serde(default)]
    pub precios: Vec<OtroPrecio>,
}

impl DatosProducto {
    fn validar(&self) -> Resultado<()> {
        let error = |mensaje: &str| Err(Error::invalido(mensaje));
        if self.codigo.trim().is_empty() {
            return error("Falta el código.");
        }
        if self.nombre.trim().is_empty() {
            return error("Falta el nombre.");
        }
        if self.precio < Dinero::CERO || self.costo < Dinero::CERO {
            return error("El precio y el costo no pueden ser negativos.");
        }
        if let (Some(minimo), Some(maximo)) = (self.stock_minimo, self.stock_maximo)
            && minimo > maximo
        {
            return error("El stock máximo no puede ser menor que el mínimo.");
        }
        for otro in &self.precios {
            if otro.nombre.trim().is_empty() {
                return error("Cada precio adicional necesita un nombre (p. ej. «Mayoreo»).");
            }
            if otro.precio < Dinero::CERO {
                return error("Los precios no pueden ser negativos.");
            }
            if otro.cantidad_minima.is_some_and(|c| !c.es_positivo()) {
                return error("La cantidad mínima de un precio debe ser mayor que cero.");
            }
        }
        Ok(())
    }

    fn codigo_barras(&self) -> Option<&str> {
        Some(self.codigo_barras.trim()).filter(|b| !b.is_empty() && !b.eq_ignore_ascii_case(self.codigo.trim()))
    }

    fn unidad(&self) -> &str {
        match self.unidad.trim() {
            _ if self.granel => "kg",
            "" => "pza",
            unidad => unidad,
        }
    }

    /// Inserta (`id` = `None`) o actualiza el producto según `sql`.
    fn guardar(&self, conn: &Connection, sql: &str, id: Option<i64>) -> Resultado<usize> {
        let codigo = self.codigo.trim();
        let barras = self.codigo_barras();
        for (campo, clave) in [("código", Some(codigo)), ("código de barras", barras)] {
            if let Some(clave) = clave
                && let Some(otro) = quien_usa(conn, clave, id)?
            {
                return Err(Error::invalido(format!("El {campo} {clave} ya lo usa «{otro}».")));
            }
        }
        conn.execute(
            sql,
            named_params! {
                ":id": id,
                ":codigo": codigo,
                ":codigo_barras": barras,
                ":nombre": self.nombre.trim(),
                ":familia_id": self.familia_id,
                ":precio": self.precio,
                ":costo": self.costo,
                ":stock_minimo": self.stock_minimo,
                ":stock_maximo": self.stock_maximo,
                ":alerta_stock": self.alerta_stock,
                ":activo": self.activo,
                ":granel": self.granel,
                ":unidad": self.unidad(),
            },
        )
        .map_err(duplicado(|| format!("Ya existe un producto con el código {codigo}.")))
    }
}

// — Movimientos de inventario —

enum_texto! {
    pub enum TipoMovimiento {
        Venta = "venta", "Venta";
        Entrada = "entrada", "Entrada";
        Ajuste = "ajuste", "Ajuste";
        Cancelacion = "cancelacion", "Cancelación";
        Devolucion = "devolucion", "Devolución";
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Movimiento {
    pub id: i64,
    pub tipo: TipoMovimiento,
    /// Positivo suma, negativo resta.
    pub cantidad: Cantidad,
    pub referencia: String,
    pub creado: NaiveDateTime,
}

const SELECCION: &str = "
    SELECT p.*, f.nombre AS familia, coalesce(p.alerta_stock, f.alerta_stock) AS umbral_alerta
    FROM productos p LEFT JOIN familias f ON f.id = p.familia_id
    WHERE p.borrado_en IS NULL";

impl Bd {
    // — Familias —

    pub fn familias(&self) -> Resultado<Vec<Familia>> {
        let mut consulta = self.conn().prepare("SELECT id, nombre, alerta_stock FROM familias ORDER BY nombre")?;
        let familias = consulta
            .query_map([], |f| Ok(Familia { id: f.get(0)?, nombre: f.get(1)?, alerta_stock: f.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(familias)
    }

    pub fn crear_familia(&mut self, datos: &DatosFamilia) -> Resultado<Familia> {
        let id = crear_familia(self.conn(), datos)?;
        self.familia(id)
    }

    pub fn actualizar_familia(&mut self, id: i64, datos: &DatosFamilia) -> Resultado<Familia> {
        let nombre = nombre_de_familia(datos)?;
        self.conn()
            .execute(
                "UPDATE familias SET nombre = ?2, alerta_stock = ?3 WHERE id = ?1",
                params![id, nombre, datos.alerta_stock],
            )
            .map_err(duplicado(|| format!("Ya existe la familia {nombre}.")))?;
        self.familia(id)
    }

    /// Sus productos quedan sin familia.
    pub fn borrar_familia(&mut self, id: i64) -> Resultado<()> {
        self.conn().execute("DELETE FROM familias WHERE id = ?1", [id])?;
        Ok(())
    }

    fn familia(&self, id: i64) -> Resultado<Familia> {
        self.familias()?.into_iter().find(|f| f.id == id).ok_or(Error::NoEncontrado("Familia"))
    }

    // — Productos —

    /// Cada palabra debe aparecer en el código, el código de barras, el nombre o la familia, sin importar acentos.
    /// Un código idéntico (lo que manda el escáner) sale primero.
    pub fn buscar_productos(&self, consulta: &str, solo_activos: bool, limite: usize) -> Resultado<Vec<Producto>> {
        let sql = format!(
            "{SELECCION}
               AND (p.activo OR NOT :solo_activos)
               AND NOT EXISTS (
                   SELECT 1 FROM json_each(:palabras) palabra
                   WHERE instr(normalizar(p.codigo || ' ' || coalesce(p.codigo_barras, '') || ' ' || p.nombre
                                         || ' ' || coalesce(f.nombre, '')),
                               palabra.value) = 0
               )
             ORDER BY p.codigo = :consulta OR coalesce(p.codigo_barras = :consulta, 0) DESC, p.nombre
             LIMIT :limite"
        );
        let parametros = named_params! {
            ":solo_activos": solo_activos,
            ":palabras": palabras_json(consulta),
            ":consulta": consulta.trim(),
            ":limite": limite as i64,
        };
        consultar(self.conn(), &sql, parametros)
    }

    pub fn producto(&self, id: i64) -> Resultado<Producto> {
        cargar(self.conn(), id)
    }

    /// Lo que manda el escáner: el código del producto o su código de barras.
    pub fn producto_por_codigo(&self, codigo: &str) -> Resultado<Option<Producto>> {
        // Si (por datos viejos) uno tuviera de código el código de barras de otro, gana el código: va al final.
        let sql = format!("{SELECCION} AND (p.codigo = ?1 OR p.codigo_barras = ?1) ORDER BY p.codigo = ?1");
        Ok(consultar(self.conn(), &sql, [codigo.trim()])?.pop())
    }

    pub fn crear_producto(&mut self, datos: &DatosProducto) -> Resultado<Producto> {
        datos.validar()?;
        self.transaccion(|tx| {
            datos.guardar(
                tx,
                "INSERT INTO productos (id, codigo, codigo_barras, nombre, familia_id, precio, costo, stock_minimo,
                                        stock_maximo, alerta_stock, activo, granel, unidad)
                 VALUES (:id, :codigo, :codigo_barras, :nombre, :familia_id, :precio, :costo, :stock_minimo,
                         :stock_maximo, :alerta_stock, :activo, :granel, :unidad)",
                None,
            )?;
            let id = tx.last_insert_rowid();
            guardar_precios(tx, id, &datos.precios)?;
            cargar(tx, id)
        })
    }

    pub fn actualizar_producto(&mut self, id: i64, datos: &DatosProducto) -> Resultado<Producto> {
        datos.validar()?;
        self.transaccion(|tx| {
            let cambiados = datos.guardar(
                tx,
                "UPDATE productos
                 SET codigo = :codigo, codigo_barras = :codigo_barras, nombre = :nombre, familia_id = :familia_id, precio = :precio, costo = :costo,
                     stock_minimo = :stock_minimo, stock_maximo = :stock_maximo, alerta_stock = :alerta_stock,
                     activo = :activo, granel = :granel, unidad = :unidad
                 WHERE id = :id AND borrado_en IS NULL",
                Some(id),
            )?;
            if cambiados == 0 {
                return Err(Error::NoEncontrado("Producto"));
            }
            guardar_precios(tx, id, &datos.precios)?;
            cargar(tx, id)
        })
    }

    /// Lo quita del catálogo. Las ventas y movimientos que ya tiene se conservan en el historial;
    /// de las ventas en curso o en pausa se quita. Su código queda libre para otro producto.
    pub fn borrar_producto(&mut self, id: i64) -> Resultado<()> {
        self.transaccion(|tx| {
            let borrados = tx.execute(
                "UPDATE productos SET borrado_en = ?2, activo = 0 WHERE id = ?1 AND borrado_en IS NULL",
                params![id, ahora()],
            )?;
            if borrados == 0 {
                return Err(Error::NoEncontrado("Producto"));
            }
            let mut sentencia = tx.prepare(
                "SELECT DISTINCT pa.venta_id FROM partidas pa JOIN ventas v ON v.id = pa.venta_id
                 WHERE pa.producto_id = ?1 AND v.estado IN ('abierta', 'pausada')",
            )?;
            let pendientes: Vec<i64> = sentencia.query_map([id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
            for venta_id in pendientes {
                tx.execute("DELETE FROM partidas WHERE venta_id = ?1 AND producto_id = ?2", [venta_id, id])?;
                crate::ventas::recalcular(tx, venta_id)?;
            }
            Ok(())
        })
    }

    // — Existencias —

    /// Llegó mercancía.
    pub fn registrar_entrada(&mut self, id: i64, cantidad: Cantidad, referencia: &str) -> Resultado<Producto> {
        if !cantidad.es_positivo() {
            return Err(Error::invalido("La cantidad que entra debe ser mayor que cero."));
        }
        self.transaccion(|tx| {
            mover(tx, id, cantidad, TipoMovimiento::Entrada, referencia)?;
            cargar(tx, id)
        })
    }

    /// Corrige la existencia a lo que se contó físicamente; queda la diferencia como ajuste.
    pub fn ajustar_existencia(&mut self, id: i64, contada: Cantidad, motivo: &str) -> Resultado<Producto> {
        self.transaccion(|tx| {
            let producto = cargar(tx, id)?;
            let diferencia = contada - producto.existencia;
            if diferencia == Cantidad::CERO {
                return Ok(producto);
            }
            mover(tx, id, diferencia, TipoMovimiento::Ajuste, motivo)?;
            cargar(tx, id)
        })
    }

    pub fn movimientos(&self, producto_id: i64, limite: usize) -> Resultado<Vec<Movimiento>> {
        let mut consulta = self.conn().prepare(
            "SELECT id, tipo, cantidad, referencia, creado FROM movimientos
             WHERE producto_id = ?1 ORDER BY creado DESC, id DESC LIMIT ?2",
        )?;
        let movimientos = consulta
            .query_map(params![producto_id, limite as i64], |f| {
                Ok(Movimiento {
                    id: f.get(0)?,
                    tipo: f.get(1)?,
                    cantidad: f.get(2)?,
                    referencia: f.get(3)?,
                    creado: f.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(movimientos)
    }

    /// Productos activos que llegaron a su aviso de existencia.
    pub fn productos_en_alerta(&self) -> Resultado<Vec<Producto>> {
        let sql = format!(
            "{SELECCION} AND p.activo AND p.existencia <= coalesce(p.alerta_stock, f.alerta_stock)
             ORDER BY p.existencia - coalesce(p.alerta_stock, f.alerta_stock), p.nombre"
        );
        consultar(self.conn(), &sql, [])
    }
}

pub(crate) fn cargar(conn: &Connection, id: i64) -> Resultado<Producto> {
    let sql = format!("{SELECCION} AND p.id = ?1");
    consultar(conn, &sql, [id])?.pop().ok_or(Error::NoEncontrado("Producto"))
}

/// Ajusta la existencia y deja constancia del movimiento.
pub(crate) fn mover(
    conn: &Connection,
    producto_id: i64,
    cantidad: Cantidad,
    tipo: TipoMovimiento,
    referencia: &str,
) -> Resultado<()> {
    conn.execute("UPDATE productos SET existencia = existencia + ?2 WHERE id = ?1", params![producto_id, cantidad])?;
    conn.execute(
        "INSERT INTO movimientos (producto_id, tipo, cantidad, referencia) VALUES (?1, ?2, ?3, ?4)",
        params![producto_id, tipo, cantidad, referencia.trim()],
    )?;
    Ok(())
}

pub(crate) fn crear_familia(conn: &Connection, datos: &DatosFamilia) -> Resultado<i64> {
    let nombre = nombre_de_familia(datos)?;
    conn.execute("INSERT INTO familias (nombre, alerta_stock) VALUES (?1, ?2)", params![nombre, datos.alerta_stock])
        .map_err(duplicado(|| format!("Ya existe la familia {nombre}.")))?;
    Ok(conn.last_insert_rowid())
}

fn nombre_de_familia(datos: &DatosFamilia) -> Resultado<&str> {
    match datos.nombre.trim() {
        "" => Err(Error::invalido("Falta el nombre de la familia.")),
        nombre => Ok(nombre),
    }
}

/// El nombre del producto (otro que `excepto`) que ya usa `clave` como código o como código de barras:
/// cada uno identifica a un solo producto, para que el escáner no tenga dudas.
pub(crate) fn quien_usa(conn: &Connection, clave: &str, excepto: Option<i64>) -> Resultado<Option<String>> {
    let nombre = conn
        .query_row(
            "SELECT nombre FROM productos
             WHERE borrado_en IS NULL AND id IS NOT ?2 AND (codigo = ?1 OR codigo_barras = ?1)",
            params![clave.trim(), excepto],
            |f| f.get(0),
        )
        .optional()?;
    Ok(nombre)
}

fn consultar(conn: &Connection, sql: &str, parametros: impl rusqlite::Params) -> Resultado<Vec<Producto>> {
    let mut consulta = conn.prepare(sql)?;
    let mut productos: Vec<Producto> =
        consulta.query_map(parametros, Producto::de_fila)?.collect::<rusqlite::Result<_>>()?;
    for producto in &mut productos {
        producto.precios = otros_precios(conn, producto.id)?;
    }
    Ok(productos)
}

pub(crate) fn otros_precios(conn: &Connection, producto_id: i64) -> Resultado<Vec<OtroPrecio>> {
    let mut consulta = conn.prepare_cached(
        "SELECT id, nombre, precio, cantidad_minima FROM precios WHERE producto_id = ?1
         ORDER BY cantidad_minima IS NULL, cantidad_minima, precio",
    )?;
    let precios = consulta
        .query_map([producto_id], |f| {
            Ok(OtroPrecio { id: f.get(0)?, nombre: f.get(1)?, precio: f.get(2)?, cantidad_minima: f.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(precios)
}

fn guardar_precios(conn: &Connection, producto_id: i64, precios: &[OtroPrecio]) -> Resultado<()> {
    conn.execute("DELETE FROM precios WHERE producto_id = ?1", [producto_id])?;
    let mut insertar = conn
        .prepare_cached("INSERT INTO precios (producto_id, nombre, precio, cantidad_minima) VALUES (?1, ?2, ?3, ?4)")?;
    for p in precios {
        insertar.execute(params![producto_id, p.nombre.trim(), p.precio, p.cantidad_minima])?;
    }
    Ok(())
}

/// Busca la familia por nombre (sin importar mayúsculas) y la crea si no existe.
pub(crate) fn familia_por_nombre(conn: &Connection, nombre: &str) -> Resultado<i64> {
    let existente =
        conn.query_row("SELECT id FROM familias WHERE nombre = ?1", [nombre.trim()], |f| f.get(0)).optional()?;
    match existente {
        Some(id) => Ok(id),
        None => crear_familia(conn, &DatosFamilia { nombre: nombre.into(), alerta_stock: None }),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn d(texto: &str) -> Dinero {
        texto.parse().unwrap()
    }

    pub(crate) fn c(texto: &str) -> Cantidad {
        texto.parse().unwrap()
    }

    pub(crate) fn datos(codigo: &str, nombre: &str, precio: &str, costo: &str) -> DatosProducto {
        DatosProducto {
            codigo: codigo.into(),
            nombre: nombre.into(),
            precio: d(precio),
            costo: d(costo),
            activo: true,
            ..Default::default()
        }
    }

    pub(crate) fn mayoreo(nombre: &str, precio: &str, desde: Option<&str>) -> OtroPrecio {
        OtroPrecio { id: 0, nombre: nombre.into(), precio: d(precio), cantidad_minima: desde.map(c) }
    }

    fn nombres(productos: &[Producto]) -> Vec<&str> {
        productos.iter().map(|p| p.nombre.as_str()).collect()
    }

    #[test]
    fn busca_sin_acentos_por_cada_palabra() {
        let mut bd = Bd::en_memoria().unwrap();
        let plomeria = bd.crear_familia(&DatosFamilia { nombre: "Plomería".into(), alerta_stock: None }).unwrap();
        bd.crear_producto(&DatosProducto { familia_id: Some(plomeria.id), ..datos("T1", "Tubo PVC ½", "30", "20") })
            .unwrap();
        bd.crear_producto(&datos("C1", "Codo de cobre", "15", "10")).unwrap();
        bd.crear_producto(&DatosProducto { activo: false, ..datos("T2", "Tubo galvanizado", "80", "60") }).unwrap();

        assert_eq!(nombres(&bd.buscar_productos("PLOMERIA tubo", true, 10).unwrap()), ["Tubo PVC ½"]);
        assert_eq!(nombres(&bd.buscar_productos("tubo", true, 10).unwrap()), ["Tubo PVC ½"]);
        assert_eq!(bd.buscar_productos("tubo", false, 10).unwrap().len(), 2);
        assert_eq!(bd.buscar_productos("", true, 10).unwrap().len(), 2);
        assert_eq!(bd.producto_por_codigo("c1").unwrap().unwrap().nombre, "Codo de cobre");
    }

    #[test]
    fn el_codigo_exacto_sale_primero() {
        let mut bd = Bd::en_memoria().unwrap();
        bd.crear_producto(&datos("1234", "Aaa con 12345 en el nombre", "1", "1")).unwrap();
        bd.crear_producto(&datos("12345", "Zzz", "1", "1")).unwrap();
        assert_eq!(nombres(&bd.buscar_productos("12345", true, 10).unwrap()), ["Zzz", "Aaa con 12345 en el nombre"]);
    }

    #[test]
    fn codigo_unico_y_validaciones() {
        let mut bd = Bd::en_memoria().unwrap();
        bd.crear_producto(&datos("A1", "Martillo", "100", "60")).unwrap();
        let error = bd.crear_producto(&datos("a1", "Otro", "1", "1")).unwrap_err();
        assert_eq!(error.to_string(), "El código a1 ya lo usa «Martillo».");

        let invertido =
            DatosProducto { stock_minimo: Some(c("10")), stock_maximo: Some(c("5")), ..datos("B", "B", "1", "1") };
        assert!(bd.crear_producto(&invertido).is_err());
        assert!(bd.crear_producto(&datos("C", "", "1", "1")).is_err());
    }

    #[test]
    fn escalones_de_mayoreo() {
        let mut bd = Bd::en_memoria().unwrap();
        let tornillo = bd
            .crear_producto(&DatosProducto {
                precios: vec![
                    mayoreo("Contratista", "0.50", None),
                    mayoreo("Caja", "0.70", Some("100")),
                    mayoreo("Mayoreo", "0.80", Some("10")),
                ],
                ..datos("TOR", "Tornillo", "1", "0.4")
            })
            .unwrap();
        let sugerido = |cantidad: &str| tornillo.sugerido(c(cantidad)).map(|p| p.precio);
        assert_eq!(sugerido("9"), None);
        assert_eq!(sugerido("10"), Some(d("0.80")));
        assert_eq!(sugerido("250"), Some(d("0.70")));
        assert_eq!(tornillo.precios.last().unwrap().nombre, "Contratista");
    }

    #[test]
    fn existencias_por_movimientos() {
        let mut bd = Bd::en_memoria().unwrap();
        let p = bd.crear_producto(&datos("A", "Arena", "10", "5")).unwrap();
        bd.registrar_entrada(p.id, c("20"), "Factura 77").unwrap();
        let p = bd.ajustar_existencia(p.id, c("18.5"), "Conteo").unwrap();
        assert_eq!(p.existencia, c("18.5"));
        let tipos: Vec<_> = bd.movimientos(p.id, 10).unwrap().iter().map(|m| (m.tipo, m.cantidad)).collect();
        assert_eq!(tipos, [(TipoMovimiento::Ajuste, c("-1.5")), (TipoMovimiento::Entrada, c("20"))]);
        assert!(bd.registrar_entrada(p.id, c("0"), "").is_err());
    }

    #[test]
    fn alerta_propia_o_de_la_familia() {
        let mut bd = Bd::en_memoria().unwrap();
        let familia =
            bd.crear_familia(&DatosFamilia { nombre: "Pinturas".into(), alerta_stock: Some(c("5")) }).unwrap();
        let en_familia = DatosProducto { familia_id: Some(familia.id), ..datos("P1", "Pintura", "1", "1") };
        let p1 = bd.crear_producto(&en_familia).unwrap();
        let propia = DatosProducto { codigo: "P2".into(), alerta_stock: Some(c("1")), ..en_familia.clone() };
        let p2 = bd.crear_producto(&propia).unwrap();
        bd.registrar_entrada(p2.id, c("3"), "").unwrap();

        assert!(p1.en_alerta()); // 0 ≤ 5
        assert_eq!(nombres(&bd.productos_en_alerta().unwrap()), ["Pintura"]);
        assert_eq!(bd.productos_en_alerta().unwrap()[0].id, p1.id); // p2: 3 > 1, el suyo manda
    }

    #[test]
    fn a_granel_siempre_es_por_kilo() {
        let mut bd = Bd::en_memoria().unwrap();
        let clavo = DatosProducto { granel: true, unidad: "pza".into(), ..datos("CLA", "Clavo 2\"", "48", "30") };
        assert_eq!(bd.crear_producto(&clavo).unwrap().unidad, "kg");
        let cable = DatosProducto { unidad: " m ".into(), ..datos("CAB", "Cable", "14", "9") };
        assert_eq!(bd.crear_producto(&cable).unwrap().unidad, "m");
        assert_eq!(bd.crear_producto(&datos("MAR", "Martillo", "1", "1")).unwrap().unidad, "pza");
    }

    #[test]
    fn borrar_libera_el_codigo_y_conserva_el_historial() {
        let mut bd = Bd::en_memoria().unwrap();
        let martillo = bd.crear_producto(&datos("MAR", "Martillo", "100", "60")).unwrap();
        bd.registrar_entrada(martillo.id, c("5"), "").unwrap();
        bd.borrar_producto(martillo.id).unwrap();

        assert!(bd.buscar_productos("martillo", false, 10).unwrap().is_empty());
        assert!(bd.producto_por_codigo("MAR").unwrap().is_none());
        assert!(bd.borrar_producto(martillo.id).is_err());
        assert_eq!(bd.movimientos(martillo.id, 10).unwrap().len(), 1);
        bd.crear_producto(&datos("MAR", "Martillo nuevo", "120", "70")).unwrap(); // el código quedó libre
    }

    #[test]
    fn el_escaner_encuentra_por_codigo_de_barras() {
        let mut bd = Bd::en_memoria().unwrap();
        let con_barras =
            DatosProducto { codigo_barras: " 7501206654088 ".into(), ..datos("10254", "Arco", "145", "0") };
        let arco = bd.crear_producto(&con_barras).unwrap();
        assert_eq!(arco.codigo_barras.as_deref(), Some("7501206654088"));
        bd.crear_producto(&datos("Z1", "Zapapico", "300", "200")).unwrap();

        assert_eq!(bd.producto_por_codigo("7501206654088").unwrap().unwrap().id, arco.id);
        assert_eq!(bd.producto_por_codigo("10254").unwrap().unwrap().id, arco.id);
        assert_eq!(nombres(&bd.buscar_productos("6654088", true, 10).unwrap()), ["Arco"]);
        assert_eq!(nombres(&bd.buscar_productos("7501206654088", true, 10).unwrap()), ["Arco"]);
        // sin código de barras no estorba el orden alfabético
        assert_eq!(nombres(&bd.buscar_productos("", true, 10).unwrap()), ["Arco", "Zapapico"]);

        // vacío o igual al código = sin código de barras; se puede quitar al editar
        let igual = DatosProducto { codigo_barras: "a7".into(), ..datos("A7", "Otro", "1", "1") };
        assert_eq!(bd.crear_producto(&igual).unwrap().codigo_barras, None);
        let sin = bd.actualizar_producto(arco.id, &datos("10254", "Arco", "145", "0")).unwrap();
        assert_eq!(sin.codigo_barras, None);
        assert!(bd.producto_por_codigo("7501206654088").unwrap().is_none());
    }

    #[test]
    fn un_codigo_identifica_a_un_solo_producto() {
        let mut bd = Bd::en_memoria().unwrap();
        let arco = DatosProducto { codigo_barras: "750A".into(), ..datos("10254", "Arco", "145", "0") };
        let arco = bd.crear_producto(&arco).unwrap();

        let mismo_barras = DatosProducto { codigo_barras: "750a".into(), ..datos("2", "Placa", "1", "1") };
        assert_eq!(
            bd.crear_producto(&mismo_barras).unwrap_err().to_string(),
            "El código de barras 750a ya lo usa «Arco»."
        );
        let barras_es_codigo = DatosProducto { codigo_barras: "10254".into(), ..datos("3", "Placa", "1", "1") };
        assert_eq!(
            bd.crear_producto(&barras_es_codigo).unwrap_err().to_string(),
            "El código de barras 10254 ya lo usa «Arco»."
        );
        assert_eq!(
            bd.crear_producto(&datos("750A", "Medidor", "1", "1")).unwrap_err().to_string(),
            "El código 750A ya lo usa «Arco»."
        );

        // editarse a sí mismo no choca; al borrarlo, sus códigos quedan libres
        bd.actualizar_producto(
            arco.id,
            &DatosProducto { codigo_barras: "750A".into(), ..datos("10254", "Arco", "150", "0") },
        )
        .unwrap();
        bd.borrar_producto(arco.id).unwrap();
        bd.crear_producto(&mismo_barras).unwrap();
    }

    #[test]
    fn a_pedir_y_margen() {
        let mut bd = Bd::en_memoria().unwrap();
        let p =
            bd.crear_producto(&DatosProducto { stock_maximo: Some(c("10")), ..datos("A", "A", "100", "75") }).unwrap();
        assert_eq!(p.a_pedir(), Some(c("10")));
        assert_eq!(p.margen(), Some(25.0));
    }
}
