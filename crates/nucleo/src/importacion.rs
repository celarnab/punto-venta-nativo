//! Alta y actualización de productos desde una hoja de Excel, con vista previa.
//!
//! La primera fila con contenido son los encabezados; se reconocen por nombre en cualquier orden
//! («Código», «Código de barras», «Descripción», «Precio venta»…); las demás columnas (p. ej. «Impuesto») se
//! ignoran. Solo el código es obligatorio: lo que no venga en la hoja no se toca.
//!
//! Un nombre en celdas combinadas que abarcan varios renglones es un solo producto; si esos renglones traen
//! códigos de barras distintos, cada uno es una variante. Una hoja sin columna de código propio usa el código de
//! barras como código: el mismo código de barras con el mismo nombre se junta en un producto, y los que no traen
//! (o traen «NA») se buscan por nombre o reciben un código generado.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use calamine::{Data, Dimensions, Reader, Sheets, open_workbook_auto};
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
    /// Vacío solo en hojas sin columna de código propio: se busca por nombre o se le genera uno.
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
    /// Lo que se hizo al leerlo (variante, renglones juntados…), para la vista previa.
    pub notas: Vec<String>,
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
    pub notas: Vec<String>,
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
    let combinadas = match &mut libro {
        Sheets::Xlsx(xlsx) => xlsx.merge_cells_by_sheet_id(0).unwrap_or_default(),
        Sheets::Xls(xls) => xls.merge_cells_by_sheet_id(0).unwrap_or_default(),
        _ => Vec::new(),
    };
    // De posiciones de la hoja a posiciones de la tabla, que empieza en la primera celda con algo.
    let (fila0, columna0) = hoja.start().unwrap_or((0, 0));
    let combinadas: Vec<Dimensions> = combinadas
        .iter()
        .filter_map(|c| {
            let inicio = (c.start.0.checked_sub(fila0)?, c.start.1.checked_sub(columna0)?);
            Some(Dimensions::new(inicio, (c.end.0 - fila0, c.end.1 - columna0)))
        })
        .collect();
    leer_tabla_combinada(hoja.rows(), fila0 as usize, &combinadas)
}

/// `desplazamiento`: cuántas filas vacías hay arriba de la tabla, para numerar como Excel.
pub fn leer_tabla<'a>(filas: impl IntoIterator<Item = &'a [Data]>, desplazamiento: usize) -> Resultado<Vec<Renglon>> {
    leer_tabla_combinada(filas, desplazamiento, &[])
}

/// Como [`leer_tabla`], con las celdas combinadas de la hoja (en posiciones de la tabla).
fn leer_tabla_combinada<'a>(
    filas: impl IntoIterator<Item = &'a [Data]>,
    desplazamiento: usize,
    combinadas: &[Dimensions],
) -> Resultado<Vec<Renglon>> {
    let mut filas = filas
        .into_iter()
        .enumerate()
        .map(|(i, celdas)| (i, desplazamiento + i + 1, celdas))
        .filter(|(_, _, celdas)| celdas.iter().any(|c| !texto(c).is_empty()));

    let Some((_, _, encabezados)) = filas.next() else {
        return Err(Error::Excel("La hoja está vacía.".into()));
    };
    let mut columnas: HashMap<Columna, usize> = encabezados
        .iter()
        .enumerate()
        .filter_map(|(i, celda)| Some((Columna::reconocer(&texto(celda))?, i)))
        .rev() // si una columna se repite, gana la primera
        .collect();
    // Una hoja con solo «Código de barras» lo usa como código del producto.
    let sin_codigo_propio = !columnas.contains_key(&Columna::Codigo);
    if sin_codigo_propio && let Some(barras) = columnas.remove(&Columna::CodigoBarras) {
        columnas.insert(Columna::Codigo, barras);
    }
    if !columnas.contains_key(&Columna::Codigo) {
        let vistos: Vec<String> = encabezados.iter().map(texto).filter(|t| !t.is_empty()).collect();
        return Err(Error::Excel(format!(
            "No encontré la columna «Código». Encabezados de la hoja: {}.",
            vistos.join(", ")
        )));
    }

    // Un renglón con datos solo en columnas que no se usan (p. ej. el total al pie de la hoja) se ignora.
    let filas =
        filas.filter(|(_, _, celdas)| columnas.values().any(|&i| celdas.get(i).is_some_and(|c| !texto(c).is_empty())));
    // Un renglón cuyo nombre está en una celda combinada que empezó más arriba es parte del producto de arriba.
    let mut grupos: Vec<Vec<(usize, &[Data])>> = Vec::new();
    for (i, numero, celdas) in filas {
        let sigue = columnas
            .get(&Columna::Nombre)
            .is_some_and(|&c| combinadas.iter().any(|m| m.contains(i as u32, c as u32) && (m.start.0 as usize) < i));
        match grupos.last_mut() {
            Some(grupo) if sigue => grupo.push((numero, celdas)),
            _ => grupos.push(vec![(numero, celdas)]),
        }
    }

    let mut renglones = Vec::new();
    for grupo in &grupos {
        // De cada columna, lo primero que traiga el grupo (lo combinado solo está en su primer renglón).
        let celda = |columna| {
            let i = columnas.get(&columna)?;
            grupo.iter().filter_map(|(_, celdas)| celdas.get(*i)).find(|c| !vacia(columna, c))
        };
        let celda = |columna| celda(columna).unwrap_or(&Data::Empty);
        let numero = grupo[0].0;
        // Los códigos distintos del grupo, cada uno con el renglón donde viene.
        let mut codigos: Vec<(usize, String)> = Vec::new();
        if let Some(&i) = columnas.get(&Columna::Codigo) {
            for (n, celdas) in grupo {
                let codigo = celdas.get(i).map(|c| codigo_de(texto(c))).unwrap_or_default();
                if !codigo.is_empty() && !codigos.iter().any(|(_, visto)| visto.eq_ignore_ascii_case(&codigo)) {
                    codigos.push((*n, codigo));
                }
            }
        }
        if !sin_codigo_propio || codigos.len() < 2 {
            renglones.push(Renglon { numero, datos: leer_datos(celda, sin_codigo_propio), notas: Vec::new() });
            continue;
        }
        // Varias variantes (p. ej. tonos) bajo un mismo nombre: un producto por código de barras, numerados.
        // La existencia es del grupo y no dice cuántas hay de cada una: va toda a la primera.
        let (desde, hasta, total) = (numero, grupo[grupo.len() - 1].0, codigos.len());
        for (i, (n, codigo)) in codigos.into_iter().enumerate() {
            let datos = leer_datos(celda, true).map(|datos| Datos {
                codigo,
                nombre: datos.nombre.map(|nombre| format!("{nombre} {}", i + 1)),
                existencia: if i == 0 { datos.existencia } else { None },
                ..datos
            });
            let nota = format!("Variante {} de {total} (renglones {desde} a {hasta}).", i + 1);
            renglones.push(Renglon { numero: n, datos, notas: vec![nota] });
        }
    }
    Ok(if sin_codigo_propio { juntar_repetidos(renglones) } else { renglones })
}

/// En una hoja sin código propio, el mismo código de barras en dos renglones: si el nombre es el mismo, es el
/// mismo producto (p. ej. capturado con dos precios) y se junta en el primero; si no, el de abajo pierde el
/// código de barras y lleva uno propio.
fn juntar_repetidos(renglones: Vec<Renglon>) -> Vec<Renglon> {
    let mut salida: Vec<Renglon> = Vec::new();
    let mut por_codigo: HashMap<String, usize> = HashMap::new();
    for mut renglon in renglones {
        let Ok(datos) = &mut renglon.datos else {
            salida.push(renglon);
            continue;
        };
        let clave = datos.codigo.to_lowercase();
        let primero = por_codigo.get(&clave).map(|&i| &mut salida[i]);
        match primero {
            _ if clave.is_empty() => {}
            None => {
                por_codigo.insert(clave, salida.len());
            }
            Some(primero) => {
                let numero = primero.numero;
                if let Ok(base) = &mut primero.datos {
                    if mismo_nombre(base.nombre.as_deref(), datos.nombre.as_deref()) {
                        juntar(base, datos);
                        primero.notas.push(format!(
                            "Junta el renglón {} (mismo código de barras): existencias sumadas, precio mayor.",
                            renglon.numero
                        ));
                        continue;
                    }
                    let duenio = base.nombre.clone().unwrap_or_default();
                    renglon.notas.push(format!(
                        "El código de barras {} ya es de «{duenio}» (renglón {numero}); este lleva código propio.",
                        datos.codigo
                    ));
                    datos.codigo.clear();
                }
            }
        }
        salida.push(renglon);
    }
    salida
}

fn mismo_nombre(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => llave_de_nombre(a) == llave_de_nombre(b),
        _ => true, // uno sin nombre: es el mismo renglón partido
    }
}

/// Lo de `otro` en `base`: existencias sumadas, el precio mayor; lo demás, lo que `base` no traiga.
fn juntar(base: &mut Datos, otro: &Datos) {
    base.existencia = match (base.existencia, otro.existencia) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    };
    base.precio = base.precio.max(otro.precio);
    base.costo = base.costo.or(otro.costo);
    base.familia = base.familia.take().or(otro.familia.clone());
    base.unidad = base.unidad.take().or(otro.unidad.clone());
    base.granel = base.granel.or(otro.granel);
    base.nombre = base.nombre.take().or(otro.nombre.clone());
}

/// Una celda sin dato; en las columnas de código, también «NA», «S/C» y similares.
fn vacia(columna: Columna, celda: &Data) -> bool {
    let texto = texto(celda);
    match columna {
        Columna::Codigo | Columna::CodigoBarras => codigo_de(texto).is_empty(),
        _ => texto.is_empty(),
    }
}

/// El código tal cual, o vacío si es un «no tiene» («NA», «N/A», «S/C», «-»…).
fn codigo_de(texto: String) -> String {
    let sin_codigo = ["na", "n/a", "n.a.", "n.a", "-", "--", "s/c", "sc", "sin codigo", "no aplica", "ninguno"];
    if sin_codigo.contains(&normalizar(&texto).trim()) { String::new() } else { texto }
}

fn leer_datos<'a>(celda: impl Fn(Columna) -> &'a Data, sin_codigo_propio: bool) -> Result<Datos, String> {
    let codigo = codigo_de(texto(celda(Columna::Codigo)));
    if codigo.is_empty() && !sin_codigo_propio {
        return Err("Falta el código.".into());
    }
    // Los espacios de más (o «duros», como los que deja copiar de una página) se quitan.
    let opcional = |columna| {
        Some(texto(celda(columna)).split_whitespace().collect::<Vec<_>>().join(" ")).filter(|t| !t.is_empty())
    };
    Ok(Datos {
        codigo_barras: opcional(Columna::CodigoBarras)
            .map(codigo_de)
            .filter(|b| !b.is_empty() && !b.eq_ignore_ascii_case(&codigo)),
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
        Ok(planear(self.conn(), renglones)?.0)
    }

    /// Aplica todo lo que la vista previa marca como crear o actualizar; los renglones con error se saltan.
    pub fn importar(&mut self, renglones: &[Renglon]) -> Resultado<Vista> {
        self.transaccion(|tx| {
            let (vista, resueltos) = planear(tx, renglones)?;
            for (datos, fila) in resueltos.iter().zip(&vista.filas) {
                if let (Some(datos), Accion::Crear | Accion::Actualizar { .. }) = (datos, &fila.accion) {
                    aplicar(tx, datos)?;
                }
            }
            Ok(vista)
        })
    }
}

/// La vista previa y, por renglón, sus datos con el código ya resuelto (el generado o el del producto que se
/// encontró por nombre); `None` si el renglón no se puede leer.
fn planear(conn: &Connection, renglones: &[Renglon]) -> Resultado<(Vista, Vec<Option<Datos>>)> {
    let mut vista = Vista::default();
    let mut resueltos = Vec::new();
    let mut vistos = Vistos::new();
    let mut generador = Generador::nuevo(conn)?;
    let mut tomados: HashMap<String, usize> = HashMap::new(); // por nombre: cuántos sin código ya se reconocieron
    for renglon in renglones {
        let mut notas = renglon.notas.clone();
        let (codigo, nombre, accion) = match &renglon.datos {
            Err(mensaje) => {
                resueltos.push(None);
                (String::new(), String::new(), Accion::Error { mensaje: mensaje.clone() })
            }
            Ok(datos) => {
                // Sin código: el producto que ya se llame igual (si varios se llaman igual, el primer renglón con
                // ese nombre es el primero de ellos, el segundo el segundo…) o, si es nuevo, uno generado.
                let (datos, existente) = if datos.codigo.is_empty() {
                    let mut ya = None;
                    if let Some(llave) = datos.nombre.as_deref().map(llave_de_nombre) {
                        let tomado = tomados.entry(llave.clone()).or_insert(0);
                        ya = mismos_nombres(conn, &llave)?.get(*tomado).copied();
                        *tomado += 1;
                    }
                    match ya {
                        Some(id) => {
                            let producto = catalogo::cargar(conn, id)?;
                            (Datos { codigo: producto.codigo.clone(), ..datos.clone() }, Some(producto))
                        }
                        // Solo se gasta un código en lo que sí se va a crear.
                        None => match accion(datos, None) {
                            Accion::Error { mensaje } => {
                                resueltos.push(None);
                                let accion = Accion::Error { mensaje };
                                contar(&mut vista, &accion);
                                let nombre = datos.nombre.clone().unwrap_or_default();
                                let numero = renglon.numero;
                                vista.filas.push(FilaVista { numero, codigo: String::new(), nombre, accion, notas });
                                continue;
                            }
                            _ => {
                                let codigo = generador.siguiente(conn)?;
                                notas.push(format!("Código generado: {codigo}."));
                                (Datos { codigo, ..datos.clone() }, None)
                            }
                        },
                    }
                } else {
                    let existente = buscar(conn, &datos.codigo)?;
                    (datos.clone(), existente)
                };
                let datos = &datos;
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
                resueltos.push(Some(datos.clone()));
                (datos.codigo.clone(), nombre, accion)
            }
        };
        contar(&mut vista, &accion);
        vista.filas.push(FilaVista { numero: renglon.numero, codigo, nombre, accion, notas });
    }
    Ok((vista, resueltos))
}

fn contar(vista: &mut Vista, accion: &Accion) {
    match accion {
        Accion::Crear => vista.crear += 1,
        Accion::Actualizar { .. } => vista.actualizar += 1,
        Accion::SinCambios => vista.sin_cambios += 1,
        Accion::Error { .. } => vista.errores += 1,
    }
}

/// El nombre sin acentos, mayúsculas ni espacios de más, para compararlo.
fn llave_de_nombre(nombre: &str) -> String {
    normalizar(nombre).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Los productos que se llaman así, en el orden en que se dieron de alta: así se reconocen en una segunda
/// importación los que no traen código.
fn mismos_nombres(conn: &Connection, llave: &str) -> Resultado<Vec<i64>> {
    let mut consulta = conn.prepare_cached("SELECT id, nombre FROM productos WHERE borrado_en IS NULL ORDER BY id")?;
    let ids = consulta
        .query_map([], |f| Ok((f.get::<_, i64>(0)?, f.get::<_, String>(1)?)))?
        .filter_map(Result::ok)
        .filter(|(_, nombre)| llave_de_nombre(nombre) == llave)
        .map(|(id, _)| id)
        .collect();
    Ok(ids)
}

/// Códigos para los productos que no traen: las iniciales del negocio y un número que sigue al último usado
/// («Farmacia Vida Sana» → FVS-0001, FVS-0002…).
struct Generador {
    prefijo: String,
    siguiente: u32,
}

impl Generador {
    fn nuevo(conn: &Connection) -> Resultado<Self> {
        let negocio: String = conn.query_row("SELECT nombre FROM negocio", [], |f| f.get(0)).unwrap_or_default();
        let mut prefijo: String =
            normalizar(&negocio).split_whitespace().filter_map(|p| p.chars().next()).take(4).collect();
        prefijo.retain(|c| c.is_ascii_alphanumeric());
        let prefijo = if prefijo.is_empty() { "P".to_owned() } else { prefijo.to_uppercase() };
        // Incluye los borrados: un código generado no se reusa, para no confundirlo en el historial.
        let mut consulta = conn.prepare("SELECT codigo FROM productos WHERE codigo LIKE ?1 || '-%'")?;
        let ultimo = consulta
            .query_map([&prefijo], |f| f.get::<_, String>(0))?
            .filter_map(Result::ok)
            .filter_map(|c| c.get(prefijo.len() + 1..)?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        Ok(Self { prefijo, siguiente: ultimo + 1 })
    }

    fn siguiente(&mut self, conn: &Connection) -> Resultado<String> {
        loop {
            let codigo = format!("{}-{:04}", self.prefijo, self.siguiente);
            self.siguiente += 1;
            if catalogo::quien_usa(conn, &codigo, None)?.is_none() {
                return Ok(codigo);
            }
        }
    }
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

    fn farmacia(bd: &mut Bd) {
        let mut negocio = bd.negocio().unwrap();
        negocio.nombre = "Vida Sana".into();
        bd.guardar_negocio(&negocio).unwrap();
    }

    /// Como la hoja FULL: sin columna de código propio, con encabezados que no se usan.
    fn lista(filas: Vec<Vec<Data>>, combinadas: &[Dimensions]) -> Vec<Renglon> {
        let mut todas = vec![
            [
                "DESCRIPCION",
                "MARCA LABORATORIO",
                "EXISTENCIA",
                "PRECIO DE VENTA",
                "CANTIDAD VENDIDA",
                "CODIGO DE BARRAS",
            ]
            .iter()
            .map(|e| s(e))
            .collect(),
        ];
        todas.extend(filas);
        leer_tabla_combinada(todas.iter().map(Vec::as_slice), 0, combinadas).unwrap()
    }

    fn f(n: f64) -> Data {
        Data::Float(n)
    }

    #[test]
    fn nombre_combinado_con_varios_codigos_son_variantes() {
        let e = Data::Empty;
        // Renglones 2 a 5: «Hidratante» combinado en A, D y E; tres códigos de barras en los de abajo.
        // Renglones 6 y 7: combinado también el código de barras, que solo está en el 6.
        let renglones = lista(
            vec![
                vec![s("Hidratante de labios"), e.clone(), f(1.0), f(25.0), f(6.0), e.clone()],
                vec![e.clone(), e.clone(), e.clone(), e.clone(), e.clone(), f(6974452880555.0)],
                vec![e.clone(), e.clone(), e.clone(), e.clone(), e.clone(), s("6974452880579")],
                vec![e.clone(), e.clone(), e.clone(), e.clone(), e.clone(), f(6972947345046.0)],
                vec![s("Bissu labial"), e.clone(), f(1.0), f(45.0), e.clone(), f(7503041351474.0)],
                vec![e.clone(), e.clone(), e.clone(), e.clone(), e.clone(), e.clone()],
            ],
            &[
                Dimensions::new((1, 0), (4, 0)),
                Dimensions::new((1, 2), (4, 2)),
                Dimensions::new((1, 3), (4, 3)),
                Dimensions::new((5, 0), (6, 0)),
                Dimensions::new((5, 5), (6, 5)),
            ],
        );
        let leidos: Vec<_> = renglones
            .iter()
            .map(|r| {
                let d = r.datos.as_ref().unwrap();
                (r.numero, d.codigo.as_str(), d.nombre.as_deref().unwrap(), d.precio, d.existencia)
            })
            .collect();
        assert_eq!(
            leidos,
            [
                (3, "6974452880555", "Hidratante de labios 1", Some(d("25")), Some(c("1"))),
                (4, "6974452880579", "Hidratante de labios 2", Some(d("25")), None),
                (5, "6972947345046", "Hidratante de labios 3", Some(d("25")), None),
                (6, "7503041351474", "Bissu labial", Some(d("45")), Some(c("1"))),
            ]
        );
        assert_eq!(renglones[1].notas, ["Variante 2 de 3 (renglones 2 a 5)."]);
    }

    #[test]
    fn mismo_codigo_de_barras_se_junta_o_lleva_codigo_propio() {
        let e = Data::Empty;
        let renglones = lista(
            vec![
                vec![s("ALTIVER TAB. 25 MG."), s("MAVER"), f(3.0), f(25.0), e.clone(), f(7503000422405.0)],
                vec![s("ALKA-SELTZER 100 TAB"), s("BAYER"), f(1.0), f(300.0), e.clone(), f(7501008443026.0)],
                vec![s("altiver  tab. 25 mg."), e.clone(), f(2.0), f(20.0), e.clone(), f(7503000422405.0)],
                vec![s("ALKA-SELTZER 1 PAR"), s("BAYER"), f(40.0), f(9.0), e.clone(), f(7501008443026.0)],
            ],
            &[],
        );
        assert_eq!(renglones.len(), 3);
        let altiver = renglones[0].datos.as_ref().unwrap();
        assert_eq!((altiver.existencia, altiver.precio), (Some(c("5")), Some(d("25"))));
        assert_eq!(
            renglones[0].notas,
            ["Junta el renglón 4 (mismo código de barras): existencias sumadas, precio mayor."]
        );
        let suelto = renglones[2].datos.as_ref().unwrap();
        assert_eq!((renglones[2].numero, suelto.codigo.as_str()), (5, ""));
        assert!(renglones[2].notas[0].contains("ya es de «ALKA-SELTZER 100 TAB» (renglón 3)"));
    }

    #[test]
    fn sin_codigo_de_barras_se_genera_y_se_reconoce_por_nombre() {
        let mut bd = Bd::en_memoria().unwrap();
        farmacia(&mut bd);
        let e = Data::Empty;
        let renglones = lista(
            vec![
                vec![s("TIJERAS PENMAX"), s("NA"), f(2.0), f(35.0), e.clone(), s("NA")],
                vec![s("MEDICIÓN DE SIGNOS VITALES"), e.clone(), e.clone(), f(60.0), e.clone(), s("NA")],
                vec![s("MEDICIÓN DE SIGNOS VITALES"), e.clone(), e.clone(), f(20.0), e.clone(), e.clone()],
                vec![s("ALKA-SELTZER 100 TAB"), e.clone(), f(1.0), f(300.0), e.clone(), f(7501008443026.0)],
                vec![s("ALKA-SELTZER 1 PAR"), e.clone(), f(40.0), f(9.0), e.clone(), f(7501008443026.0)],
            ],
            &[],
        );
        let vista = bd.previsualizar_importacion(&renglones).unwrap();
        let codigos: Vec<_> = vista.filas.iter().map(|f| f.codigo.as_str()).collect();
        assert_eq!(codigos, ["VS-0001", "VS-0002", "VS-0003", "7501008443026", "VS-0004"]);
        assert_eq!(vista.filas[0].notas, ["Código generado: VS-0001."]);
        assert!(bd.producto_por_codigo("VS-0001").unwrap().is_none()); // la vista previa no toca nada

        assert_eq!(bd.importar(&renglones).unwrap().crear, 5);
        let tijeras = bd.producto_por_codigo("VS-0001").unwrap().unwrap();
        assert_eq!((tijeras.nombre.as_str(), tijeras.precio, tijeras.existencia), ("TIJERAS PENMAX", d("35"), c("2")));
        assert_eq!(bd.producto_por_codigo("VS-0004").unwrap().unwrap().nombre, "ALKA-SELTZER 1 PAR");

        // Otra vez el mismo archivo: los que no traen código se reconocen por nombre, en orden si se repite.
        let otra = bd.previsualizar_importacion(&renglones).unwrap();
        assert_eq!((otra.crear, otra.sin_cambios, otra.errores), (0, 5, 0));
        let codigos: Vec<_> = otra.filas.iter().map(|f| f.codigo.as_str()).collect();
        assert_eq!(codigos, ["VS-0001", "VS-0002", "VS-0003", "7501008443026", "VS-0004"]);
        // Los siguientes generados siguen la numeración.
        // Los siguientes generados siguen la numeración; lo que no se va a crear no gasta código; el total al pie
        // (en una columna que no se usa) no es un renglón.
        let nuevo = lista(
            vec![
                vec![s("SIN PRECIO"), e.clone(), e.clone(), e.clone(), e.clone(), s("NA")],
                vec![s("CINTA MÉTRICA"), e.clone(), e.clone(), f(55.0), e.clone(), s("N/A")],
                vec![e.clone(), s(""), e.clone(), e.clone(), e.clone(), e.clone(), f(324687.5)],
            ],
            &[],
        );
        let vista = bd.previsualizar_importacion(&nuevo).unwrap();
        assert_eq!(vista.filas.len(), 2);
        assert_eq!(vista.filas[0].accion, Accion::Error { mensaje: "Producto nuevo sin precio.".into() });
        assert_eq!(vista.filas[1].codigo, "VS-0005");
    }

    #[test]
    fn na_no_es_un_codigo_en_hojas_con_codigo_propio() {
        let renglones = tabla(vec![
            vec![s("Código"), s("Código de barras"), s("Nombre"), s("Precio")],
            vec![s("NA"), s("750"), s("Algo"), Data::Float(1.0)],
            vec![s("A1"), s("n/a"), s("Otro"), Data::Float(1.0)],
        ]);
        assert_eq!(renglones[0].datos, Err("Falta el código.".into()));
        assert_eq!(renglones[1].datos.as_ref().unwrap().codigo_barras, None);
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
