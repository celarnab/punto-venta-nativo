//! Alta y actualización de productos desde una hoja de Excel, con vista previa.
//!
//! La primera fila con contenido son los encabezados; se reconocen por nombre en cualquier orden
//! («Código», «Código de barras», «Descripción», «Precio venta»…); las demás columnas (p. ej. «Impuesto») se
//! ignoran. Solo el código es obligatorio: lo que no venga en la hoja no se toca.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use calamine::{Data, Reader, open_workbook_auto};
use rusqlite::{Connection, OptionalExtension, named_params};
use serde::Serialize;

use crate::catalogo::{self, Producto, TipoMovimiento};
use crate::texto::normalizar;
use crate::{Bd, Cantidad, Dinero, Error, Resultado};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Columna {
    Codigo,
    CodigoBarras,
    Nombre,
    Familia,
    Precio,
    Costo,
    Existencia,
    Unidad,
    Granel,
}

impl Columna {
    fn reconocer(encabezado: &str) -> Option<Self> {
        let encabezado = normalizar(encabezado);
        let alias: &[(&[&str], Self)] = &[
            (
                &[
                    "codigo",
                    "clave",
                    "sku",
                    "cod",
                    "codigo del producto",
                    "codigo de producto",
                    "codigo producto",
                    "clave del producto",
                    "codigo interno",
                ],
                Self::Codigo,
            ),
            (&["codigo de barras", "codigo barras", "barras", "ean", "upc"], Self::CodigoBarras),
            (&["nombre", "descripcion", "producto", "articulo"], Self::Nombre),
            (&["familia", "departamento", "categoria", "linea"], Self::Familia),
            (&["precio", "precio venta", "precio de venta", "precio publico", "p. venta", "pventa"], Self::Precio),
            (
                &[
                    "costo",
                    "precio compra",
                    "precio de compra",
                    "costo unitario",
                    "p. compra",
                    "costo ultimo",
                    "ultimo costo",
                ],
                Self::Costo,
            ),
            (&["existencia", "existencias", "stock", "inventario", "cantidad"], Self::Existencia),
            (&["unidad", "unidad de medida", "um"], Self::Unidad),
            (&["a granel", "granel", "venta a granel", "por kilo"], Self::Granel),
        ];
        alias.iter().find(|(nombres, _)| nombres.contains(&encabezado.trim())).map(|&(_, columna)| columna)
    }
}

/// Lo que trae un renglón de la hoja; `None` = la columna no viene o la celda está vacía.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Datos {
    pub codigo: String,
    pub codigo_barras: Option<String>,
    pub nombre: Option<String>,
    pub familia: Option<String>,
    pub precio: Option<Dinero>,
    pub costo: Option<Dinero>,
    pub existencia: Option<Cantidad>,
    pub unidad: Option<String>,
    /// Se vende por kilo.
    pub granel: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Renglon {
    /// Como lo ve el usuario en Excel (el primero es 1).
    pub numero: usize,
    pub datos: Result<Datos, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "tipo", rename_all = "snake_case")]
pub enum Accion {
    Crear,
    Actualizar { cambios: Vec<String> },
    SinCambios,
    Error { mensaje: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FilaVista {
    pub numero: usize,
    pub codigo: String,
    pub nombre: String,
    pub accion: Accion,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Vista {
    pub filas: Vec<FilaVista>,
    pub crear: usize,
    pub actualizar: usize,
    pub sin_cambios: usize,
    pub errores: usize,
}

/// Lee la primera hoja de un .xlsx, .xls u .ods.
pub fn leer_excel(ruta: impl AsRef<Path>) -> Resultado<Vec<Renglon>> {
    let mut libro = open_workbook_auto(ruta).map_err(|e| Error::Excel(e.to_string()))?;
    let hoja = libro
        .worksheet_range_at(0)
        .ok_or_else(|| Error::Excel("El archivo no tiene hojas.".into()))?
        .map_err(|e| Error::Excel(e.to_string()))?;
    let desplazamiento = hoja.start().map_or(0, |(fila, _)| fila as usize);
    leer_tabla(hoja.rows(), desplazamiento)
}

/// `desplazamiento`: cuántas filas vacías hay arriba de la tabla, para numerar como Excel.
pub fn leer_tabla<'a>(filas: impl IntoIterator<Item = &'a [Data]>, desplazamiento: usize) -> Resultado<Vec<Renglon>> {
    let mut filas = filas
        .into_iter()
        .enumerate()
        .map(|(i, celdas)| (desplazamiento + i + 1, celdas))
        .filter(|(_, celdas)| celdas.iter().any(|c| !texto(c).is_empty()));

    let Some((_, encabezados)) = filas.next() else {
        return Err(Error::Excel("La hoja está vacía.".into()));
    };
    let mut columnas: HashMap<Columna, usize> = encabezados
        .iter()
        .enumerate()
        .filter_map(|(i, celda)| Some((Columna::reconocer(&texto(celda))?, i)))
        .rev() // si una columna se repite, gana la primera
        .collect();
    // Una hoja con solo «Código de barras» lo usa como código del producto.
    if !columnas.contains_key(&Columna::Codigo)
        && let Some(barras) = columnas.remove(&Columna::CodigoBarras)
    {
        columnas.insert(Columna::Codigo, barras);
    }
    if !columnas.contains_key(&Columna::Codigo) {
        let vistos: Vec<String> = encabezados.iter().map(texto).filter(|t| !t.is_empty()).collect();
        return Err(Error::Excel(format!(
            "No encontré la columna «Código». Encabezados de la hoja: {}.",
            vistos.join(", ")
        )));
    }

    Ok(filas
        .map(|(numero, celdas)| {
            let celda = |columna| columnas.get(&columna).and_then(|&i| celdas.get(i)).unwrap_or(&Data::Empty);
            Renglon { numero, datos: leer_datos(celda) }
        })
        .collect())
}

fn leer_datos<'a>(celda: impl Fn(Columna) -> &'a Data) -> Result<Datos, String> {
    let codigo = texto(celda(Columna::Codigo));
    if codigo.is_empty() {
        return Err("Falta el código.".into());
    }
    // Los espacios de más (o «duros», como los que deja copiar de una página) se quitan.
    let opcional = |columna| {
        Some(texto(celda(columna)).split_whitespace().collect::<Vec<_>>().join(" ")).filter(|t| !t.is_empty())
    };
    Ok(Datos {
        codigo_barras: opcional(Columna::CodigoBarras).filter(|b| !b.eq_ignore_ascii_case(&codigo)),
        codigo,
        nombre: opcional(Columna::Nombre),
        familia: opcional(Columna::Familia),
        precio: numero(celda(Columna::Precio), Dinero::redondeando).map_err(|e| format!("Precio: {e}"))?,
        costo: numero(celda(Columna::Costo), Dinero::redondeando).map_err(|e| format!("Costo: {e}"))?,
        existencia: numero(celda(Columna::Existencia), Cantidad::redondeando)
            .map_err(|e| format!("Existencia: {e}"))?,
        unidad: opcional(Columna::Unidad).map(unidad),
        granel: si_o_no(celda(Columna::Granel)).map_err(|e| format!("A granel: {e}"))?,
    })
}

/// «PIEZA», «Pzas»… → «pza», como la usa la app; cualquier otra se queda como viene.
fn unidad(unidad: String) -> String {
    match normalizar(&unidad).trim_end_matches('.') {
        "pieza" | "piezas" | "pza" | "pzas" | "pz" | "pzs" => "pza".into(),
        _ => unidad,
    }
}

/// «Sí», «x», «1», «granel»… → sí; «No», «0», vacío… → no.
fn si_o_no(celda: &Data) -> Result<Option<bool>, String> {
    match celda {
        Data::Empty => Ok(None),
        Data::Bool(b) => Ok(Some(*b)),
        Data::Int(n) => Ok(Some(*n != 0)),
        Data::Float(n) => Ok(Some(*n != 0.0)),
        otro => match normalizar(&texto(otro)).as_str() {
            "" => Ok(None),
            "si" | "s" | "x" | "1" | "verdadero" | "true" | "granel" | "kg" => Ok(Some(true)),
            "no" | "n" | "0" | "falso" | "false" | "unidad" | "pieza" | "pza" => Ok(Some(false)),
            _ => Err(format!("«{}» debe ser Sí o No.", texto(otro))),
        },
    }
}

fn texto(celda: &Data) -> String {
    match celda {
        Data::String(s) => s.trim().to_owned(),
        Data::Empty => String::new(),
        otro => otro.to_string(), // números: 7501234567890 sale sin exponente ni decimales
    }
}

/// Los números de Excel son `f64`: se redondean; el texto se lee con el formato habitual (`$1,200.50`).
fn numero<N: FromStr<Err = Error>>(celda: &Data, redondear: fn(f64) -> N) -> Result<Option<N>, String> {
    match celda {
        Data::Empty => Ok(None),
        Data::Float(f) => Ok(Some(redondear(*f))),
        Data::Int(i) => Ok(Some(redondear(*i as f64))),
        Data::String(s) if s.trim().is_empty() => Ok(None),
        Data::String(s) => s.parse().map(Some).map_err(|e: Error| e.to_string()),
        otro => Err(format!("«{otro}» no es un número.")),
    }
}

impl Bd {
    pub fn previsualizar_importacion(&self, renglones: &[Renglon]) -> Resultado<Vista> {
        planear(self.conn(), renglones)
    }

    /// Aplica todo lo que la vista previa marca como crear o actualizar; los renglones con error se saltan.
    pub fn importar(&mut self, renglones: &[Renglon]) -> Resultado<Vista> {
        self.transaccion(|tx| {
            let vista = planear(tx, renglones)?;
            for (renglon, fila) in renglones.iter().zip(&vista.filas) {
                if let (Ok(datos), Accion::Crear | Accion::Actualizar { .. }) = (&renglon.datos, &fila.accion) {
                    aplicar(tx, datos)?;
                }
            }
            Ok(vista)
        })
    }
}

fn planear(conn: &Connection, renglones: &[Renglon]) -> Resultado<Vista> {
    let mut vista = Vista::default();
    let mut vistos = Vistos::new();
    for renglon in renglones {
        let (codigo, nombre, accion) = match &renglon.datos {
            Err(mensaje) => (String::new(), String::new(), Accion::Error { mensaje: mensaje.clone() }),
            Ok(datos) => {
                let existente = buscar(conn, &datos.codigo)?;
                let choque = match repetido(&vistos, datos) {
                    Some(mensaje) => Some(mensaje),
                    None => ocupado(conn, datos, existente.as_ref())?,
                };
                // Un renglón que se salta no aparta sus códigos: el que sí entra los puede usar.
                if choque.is_none() {
                    for (clave, es_barras) in claves(datos) {
                        vistos.insert(clave.to_lowercase(), (renglon.numero, es_barras));
                    }
                }
                let accion = match choque {
                    Some(mensaje) => Accion::Error { mensaje },
                    None => accion(datos, existente.as_ref()),
                };
                let nombre = datos.nombre.clone().or(existente.map(|p| p.nombre)).unwrap_or_default();
                (datos.codigo.clone(), nombre, accion)
            }
        };
        match accion {
            Accion::Crear => vista.crear += 1,
            Accion::Actualizar { .. } => vista.actualizar += 1,
            Accion::SinCambios => vista.sin_cambios += 1,
            Accion::Error { .. } => vista.errores += 1,
        }
        vista.filas.push(FilaVista { numero: renglon.numero, codigo, nombre, accion });
    }
    Ok(vista)
}

/// Código o código de barras (en minúsculas) → el renglón donde apareció y si ahí era código de barras.
type Vistos = HashMap<String, (usize, bool)>;

/// El código del renglón y, si trae, su código de barras (`true`).
fn claves(datos: &Datos) -> impl Iterator<Item = (&String, bool)> {
    [Some((&datos.codigo, false)), datos.codigo_barras.as_ref().map(|b| (b, true))].into_iter().flatten()
}

/// Cada código y código de barras puede venir una sola vez en la hoja, en cualquiera de las dos columnas.
fn repetido(vistos: &Vistos, datos: &Datos) -> Option<String> {
    claves(datos).find_map(|(clave, es_barras)| {
        let &(n, era_barras) = vistos.get(&clave.to_lowercase())?;
        Some(match (era_barras, es_barras) {
            (false, false) => format!("Código repetido: ya viene en el renglón {n}."),
            (true, true) => format!("Código de barras repetido: ya viene en el renglón {n}."),
            (false, true) => format!("El código de barras {clave} ya viene como código en el renglón {n}."),
            (true, false) => format!("El código {clave} ya viene como código de barras en el renglón {n}."),
        })
    })
}

/// Lo que ya está en la base: el código de barras no puede ser de otro producto, ni un código nuevo ser el
/// código de barras de alguien más.
fn ocupado(conn: &Connection, datos: &Datos, existente: Option<&Producto>) -> Resultado<Option<String>> {
    if existente.is_none()
        && let Some(otro) = catalogo::quien_usa(conn, &datos.codigo, None)?
    {
        return Ok(Some(format!("El código {} ya es el código de barras de «{otro}».", datos.codigo)));
    }
    if let Some(barras) = &datos.codigo_barras
        && let Some(otro) = catalogo::quien_usa(conn, barras, existente.map(|p| p.id))?
    {
        return Ok(Some(format!("El código de barras {barras} ya lo usa «{otro}».")));
    }
    Ok(None)
}

fn accion(datos: &Datos, existente: Option<&Producto>) -> Accion {
    let Some(actual) = existente else {
        return match (&datos.nombre, datos.precio) {
            (None, _) => Accion::Error { mensaje: "Producto nuevo sin nombre.".into() },
            (_, None) => Accion::Error { mensaje: "Producto nuevo sin precio.".into() },
            _ => Accion::Crear,
        };
    };

    let mut cambios = Vec::new();
    let mut comparar = |campo: &str, antes: String, despues: Option<String>| {
        if let Some(despues) = despues.filter(|d| normalizar(d) != normalizar(&antes)) {
            cambios.push(format!("{campo}: {antes} → {despues}"));
        }
    };
    comparar("Código de barras", actual.codigo_barras.clone().unwrap_or_default(), datos.codigo_barras.clone());
    comparar("Nombre", actual.nombre.clone(), datos.nombre.clone());
    comparar("Familia", actual.familia.clone().unwrap_or_default(), datos.familia.clone());
    comparar("Precio", actual.precio.pesos(), datos.precio.map(Dinero::pesos));
    comparar("Costo", actual.costo.pesos(), datos.costo.map(Dinero::pesos));
    comparar("Existencia", actual.existencia.to_string(), datos.existencia.map(|e| e.to_string()));
    comparar("Unidad", actual.unidad.clone(), datos.unidad.clone());
    let si_no = |b: bool| if b { "Sí" } else { "No" }.to_string();
    comparar("A granel", si_no(actual.granel), datos.granel.map(si_no));

    if cambios.is_empty() { Accion::SinCambios } else { Accion::Actualizar { cambios } }
}

fn buscar(conn: &Connection, codigo: &str) -> Resultado<Option<Producto>> {
    let id = conn
        .query_row("SELECT id FROM productos WHERE codigo = ?1 AND borrado_en IS NULL", [codigo], |f| f.get(0))
        .optional()?;
    id.map(|id| catalogo::cargar(conn, id)).transpose()
}

/// Crea o actualiza el producto; solo toca las columnas que trae la hoja.
fn aplicar(conn: &Connection, datos: &Datos) -> Resultado<()> {
    let familia_id = datos.familia.as_deref().map(|f| catalogo::familia_por_nombre(conn, f)).transpose()?;
    let parametros = named_params! {
        ":codigo": datos.codigo,
        ":codigo_barras": datos.codigo_barras,
        ":nombre": datos.nombre,
        ":familia_id": familia_id,
        ":precio": datos.precio,
        ":costo": datos.costo,
        ":unidad": datos.unidad,
        ":granel": datos.granel,
    };
    let actualizados = conn.execute(
        "UPDATE productos SET codigo_barras = coalesce(:codigo_barras, codigo_barras),
                              nombre = coalesce(:nombre, nombre), familia_id = coalesce(:familia_id, familia_id),
                              precio = coalesce(:precio, precio), costo = coalesce(:costo, costo),
                              granel = coalesce(:granel, granel),
                              unidad = CASE WHEN coalesce(:granel, granel) THEN 'kg' ELSE coalesce(:unidad, unidad) END
         WHERE codigo = :codigo AND borrado_en IS NULL",
        parametros,
    )?;
    if actualizados == 0 {
        conn.execute(
            "INSERT INTO productos (codigo, codigo_barras, nombre, familia_id, precio, costo, granel, unidad)
             VALUES (:codigo, :codigo_barras, :nombre, :familia_id, :precio, coalesce(:costo, 0), coalesce(:granel, 0),
                     CASE WHEN :granel THEN 'kg' ELSE coalesce(:unidad, 'pza') END)",
            parametros,
        )?;
    }
    if let Some(contada) = datos.existencia {
        let (id, existencia): (i64, Cantidad) =
            conn.query_row("SELECT id, existencia FROM productos WHERE codigo = ?1", [&datos.codigo], |f| {
                Ok((f.get(0)?, f.get(1)?))
            })?;
        if contada != existencia {
            catalogo::mover(conn, id, contada - existencia, TipoMovimiento::Ajuste, "Importación de Excel")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogo::DatosProducto;
    use crate::catalogo::tests::{c, d, datos};

    fn s(texto: &str) -> Data {
        Data::String(texto.into())
    }

    fn tabla(filas: Vec<Vec<Data>>) -> Vec<Renglon> {
        leer_tabla(filas.iter().map(Vec::as_slice), 0).unwrap()
    }

    #[test]
    fn reconoce_encabezados_en_cualquier_orden() {
        let renglones = tabla(vec![
            vec![s("Precio Venta"), s("Descripción"), s("CÓDIGO"), s("Otra cosa")],
            vec![Data::Float(12.499), s("Martillo"), Data::Float(7501234567890.0), s("x")],
            vec![Data::Empty, Data::Empty, Data::Empty],
            vec![s("$1,200"), s("Taladro"), s("TAL-1")],
        ]);
        assert_eq!(renglones.len(), 2);
        let martillo = renglones[0].datos.as_ref().unwrap();
        assert_eq!(
            martillo,
            &Datos {
                codigo: "7501234567890".into(),
                nombre: Some("Martillo".into()),
                precio: Some(d("12.50")),
                ..Default::default()
            }
        );
        assert_eq!(renglones[1].numero, 4);
        assert_eq!(renglones[1].datos.as_ref().unwrap().precio, Some(d("1200")));
    }

    #[test]
    fn errores_por_renglon_o_de_hoja() {
        let renglones =
            tabla(vec![vec![s("codigo"), s("precio"), s("existencia")], vec![s(""), s("10")], vec![s("A"), s("diez")]]);
        assert_eq!(renglones[0].datos, Err("Falta el código.".into()));
        assert_eq!(renglones[1].datos, Err("Precio: «diez» no es un número válido.".into()));

        let sin_codigo = leer_tabla([vec![s("Nombre"), s("Precio")]].iter().map(Vec::as_slice), 0);
        assert!(sin_codigo.unwrap_err().to_string().contains("Nombre, Precio"));
    }

    #[test]
    fn vista_previa_e_importacion() {
        let mut bd = Bd::en_memoria().unwrap();
        bd.crear_producto(&datos("MAR", "Martillo", "100", "60")).unwrap();
        bd.crear_producto(&datos("PIN", "Pinza", "80", "50")).unwrap();

        let renglones = tabla(vec![
            vec![s("Código"), s("Nombre"), s("Familia"), s("Precio"), s("Costo"), s("Existencia")],
            vec![s("mar"), Data::Empty, Data::Empty, Data::Float(110.0), Data::Empty, Data::Float(5.0)],
            vec![s("PIN"), s("pinza"), Data::Empty, Data::Float(80.0)],
            vec![s("CLA"), s("Clavo 2\""), s("Ferretería"), Data::Float(0.5), Data::Float(0.2), Data::Float(1000.0)],
            vec![s("TOR"), s("Tornillo")],
            vec![s("CLA"), s("Clavo repetido"), Data::Empty, Data::Float(1.0)],
        ]);

        let vista = bd.previsualizar_importacion(&renglones).unwrap();
        assert_eq!((vista.crear, vista.actualizar, vista.sin_cambios, vista.errores), (1, 1, 1, 2));
        assert_eq!(
            vista.filas[0].accion,
            Accion::Actualizar { cambios: vec!["Precio: $100.00 → $110.00".into(), "Existencia: 0 → 5".into()] }
        );
        assert_eq!(vista.filas[0].nombre, "Martillo");
        assert_eq!(vista.filas[3].accion, Accion::Error { mensaje: "Producto nuevo sin precio.".into() });
        assert!(matches!(&vista.filas[4].accion, Accion::Error { mensaje } if mensaje.contains("renglón 4")));
        assert!(bd.producto_por_codigo("CLA").unwrap().is_none()); // la vista previa no toca nada

        bd.importar(&renglones).unwrap();
        let martillo = bd.producto_por_codigo("MAR").unwrap().unwrap();
        assert_eq!((martillo.precio, martillo.costo, martillo.existencia), (d("110"), d("60"), c("5")));
        let clavo = bd.producto_por_codigo("CLA").unwrap().unwrap();
        assert_eq!((clavo.nombre.as_str(), clavo.familia.as_deref()), ("Clavo 2\"", Some("Ferretería")));
        assert_eq!(clavo.existencia, c("1000"));

        let de_nuevo = bd.previsualizar_importacion(&renglones).unwrap();
        assert_eq!(de_nuevo.sin_cambios, 3);
    }

    #[test]
    fn plantilla_con_codigo_del_producto_y_de_barras() {
        let mut bd = Bd::en_memoria().unwrap();
        let encabezados = [
            "Código del producto",
            "Código de barras",
            "Descripción",
            "Precio de venta",
            "Costo ultimo",
            "Existencia",
            "Impuesto",
            "Unidad",
        ];
        let renglones = tabla(vec![
            encabezados.iter().map(|e| s(e)).collect(),
            vec![
                Data::Float(10254.0),
                s("7501206654088"),
                s("Arco jardinero tubular, 21\""),
                Data::Float(145.0),
                Data::Empty,
                Data::Float(3.0),
                s("SYS"),
                s("PIEZA"),
            ],
            vec![
                Data::Float(11004.0),
                Data::Empty,
                s("  Sierra circular,\u{a0}1500 w  "),
                Data::Float(1875.0),
                Data::Float(1200.0),
                Data::Float(1.0),
                s("SYS"),
                s("PIEZA"),
            ],
            vec![Data::Float(7.0), s("7"), s("Caja"), Data::Float(1.0), Data::Empty, Data::Empty, s("SYS"), s("CAJA")],
        ]);
        let arco = renglones[0].datos.as_ref().unwrap();
        assert_eq!(
            arco,
            &Datos {
                codigo: "10254".into(),
                codigo_barras: Some("7501206654088".into()),
                nombre: Some("Arco jardinero tubular, 21\"".into()),
                precio: Some(d("145")),
                existencia: Some(c("3")),
                unidad: Some("pza".into()),
                ..Default::default()
            }
        );
        let sierra = renglones[1].datos.as_ref().unwrap();
        assert_eq!(
            (sierra.codigo_barras.as_deref(), sierra.nombre.as_deref()),
            (None, Some("Sierra circular, 1500 w"))
        );
        assert_eq!(sierra.costo, Some(d("1200")));
        let caja = renglones[2].datos.as_ref().unwrap();
        assert_eq!((caja.codigo_barras.as_deref(), caja.unidad.as_deref()), (None, Some("CAJA"))); // igual al código

        assert_eq!(bd.importar(&renglones).unwrap().crear, 3);
        let por_barras = bd.producto_por_codigo("7501206654088").unwrap().unwrap();
        assert_eq!((por_barras.codigo.as_str(), por_barras.unidad.as_str()), ("10254", "pza"));
        assert_eq!(bd.previsualizar_importacion(&renglones).unwrap().sin_cambios, 3);
    }

    #[test]
    fn sin_columna_codigo_usa_el_de_barras() {
        let renglones = tabla(vec![vec![s("Código de barras"), s("Nombre")], vec![s("750"), s("Martillo")]]);
        let martillo = renglones[0].datos.as_ref().unwrap();
        assert_eq!((martillo.codigo.as_str(), martillo.codigo_barras.as_deref()), ("750", None));
    }

    #[test]
    fn codigos_y_codigos_de_barras_no_chocan() {
        let mut bd = Bd::en_memoria().unwrap();
        let ya = DatosProducto { codigo_barras: "750-YA".into(), ..datos("YA", "Ya existía", "1", "1") };
        bd.crear_producto(&ya).unwrap();

        let fila = |codigo: &str, barras: &str| {
            vec![s(codigo), if barras.is_empty() { Data::Empty } else { s(barras) }, s("Algo"), Data::Float(1.0)]
        };
        let renglones = tabla(vec![
            vec![s("Código"), s("Código de barras"), s("Nombre"), s("Precio")],
            fila("17054", "750-NIVEL"),       // 2
            fila("750-NIVEL", "750-MEDIDOR"), // 3: su código es el de barras del renglón 2
            fila("48088", "750-PLACA"),       // 4
            fila("48090", "750-PLACA"),       // 5: mismo código de barras
            fila("1", "17054"),               // 6: su código de barras es el código del renglón 2
            fila("2", "750-YA"),              // 7: ya lo usa un producto de la base
            fila("750-YA", ""),               // 8: es el código de barras de uno de la base
            fila("YA", "750-YA"),             // 9: el mismo producto; su código de barras sigue siendo suyo
        ]);
        let vista = bd.previsualizar_importacion(&renglones).unwrap();
        let mensajes: Vec<_> = vista
            .filas
            .iter()
            .map(|f| match &f.accion {
                Accion::Error { mensaje } => mensaje.as_str(),
                Accion::Crear => "crear",
                _ => "otro",
            })
            .collect();
        assert_eq!(
            mensajes,
            [
                "crear",
                "El código 750-NIVEL ya viene como código de barras en el renglón 2.",
                "crear",
                "Código de barras repetido: ya viene en el renglón 4.",
                "El código de barras 17054 ya viene como código en el renglón 2.",
                "El código de barras 750-YA ya lo usa «Ya existía».",
                "El código 750-YA ya es el código de barras de «Ya existía».",
                "otro",
            ]
        );
        assert_eq!(vista.filas[7].accion, Accion::Actualizar { cambios: vec!["Nombre: Ya existía → Algo".into()] });
        let hecho = bd.importar(&renglones).unwrap();
        assert_eq!((hecho.crear, hecho.actualizar, hecho.errores), (2, 1, 5));
    }

    #[test]
    fn importa_venta_a_granel() {
        let mut bd = Bd::en_memoria().unwrap();
        let renglones = tabla(vec![
            vec![s("Código"), s("Nombre"), s("Precio"), s("Unidad"), s("A granel")],
            vec![s("CLA"), s("Clavo"), Data::Float(48.0), s("pza"), s("Sí")],
            vec![s("CAB"), s("Cable"), Data::Float(14.0), s("m"), s("no")],
            vec![s("XXX"), s("Raro"), Data::Float(1.0), Data::Empty, s("quizá")],
        ]);
        let vista = bd.importar(&renglones).unwrap();
        assert_eq!((vista.crear, vista.errores), (2, 1));
        let clavo = bd.producto_por_codigo("CLA").unwrap().unwrap();
        assert_eq!((clavo.granel, clavo.unidad.as_str()), (true, "kg"));
        assert_eq!(bd.producto_por_codigo("CAB").unwrap().unwrap().unidad, "m");
    }

    #[test]
    fn lo_exportado_se_vuelve_a_importar_igual() {
        let mut bd = Bd::en_memoria().unwrap();
        let martillo = bd
            .crear_producto(&DatosProducto {
                codigo_barras: "0750123".into(),
                ..datos("007", "Martillo", "116.5", "70")
            })
            .unwrap();
        bd.crear_producto(&DatosProducto { granel: true, ..datos("CLA", "Clavo", "48", "30") }).unwrap();
        bd.registrar_entrada(martillo.id, c("2.5"), "").unwrap();
        let productos = bd.buscar_productos("", false, 100).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let ruta = dir.path().join("productos.xlsx");
        std::fs::write(&ruta, crate::exportacion::productos_a_excel(&productos).unwrap()).unwrap();

        let renglones = leer_excel(&ruta).unwrap();
        let leido = renglones.iter().find_map(|r| r.datos.as_ref().ok().filter(|d| d.codigo == "007")).unwrap();
        assert_eq!((leido.codigo.as_str(), leido.precio, leido.existencia), ("007", Some(d("116.5")), Some(c("2.5"))));
        assert_eq!(leido.codigo_barras.as_deref(), Some("0750123"));
        assert_eq!(bd.previsualizar_importacion(&renglones).unwrap().sin_cambios, 2);
    }
}
