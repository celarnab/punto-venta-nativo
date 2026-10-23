//! Qué se vendió, cuándo y a quién: las consultas detrás de cada apartado del reporte.

use std::collections::BTreeMap;
use std::iter::successors;

use chrono::{Datelike, Days, Months, NaiveDate};
use rusqlite::Connection;
use rusqlite::types::FromSql;
use serde::{Deserialize, Serialize};

use crate::usuarios::{Permiso, Usuario};
use crate::{Bd, Cantidad, Dinero, Error, Resultado};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filtro {
    pub desde: NaiveDate,
    pub hasta: NaiveDate,
    #[serde(default)]
    pub cajero_id: Option<i64>,
    #[serde(default)]
    pub cliente_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fila {
    pub etiqueta: String,
    pub monto: Dinero,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Resumen {
    pub ventas: i64,
    /// Lo cobrado en ventas.
    pub vendido: Dinero,
    /// Lo regresado a clientes por devoluciones.
    pub devoluciones: Dinero,
    /// Vendido menos devoluciones.
    pub total: Dinero,
    pub articulos: Cantidad,
    pub promedio: Dinero,
    pub costo: Dinero,
    pub utilidad: Dinero,
    /// Utilidad sobre lo vendido, en %.
    pub margen: Option<f64>,
}

enum_texto! {
    pub enum Seccion {
        Evolucion = "evolucion", "Evolución de ventas";
        Familias = "familias", "Ventas por familia";
        Ganancia = "ganancia", "Ganancia por familia";
        Productos = "productos", "Productos más vendidos";
        Empleados = "empleados", "Ventas por empleado";
        Clientes = "clientes", "Ventas por cliente";
        Metodos = "metodos", "Ventas por forma de pago";
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Apartado {
    pub seccion: Seccion,
    pub titulo: &'static str,
    pub filas: Vec<Fila>,
}

impl Seccion {
    /// Lo que ve un cajero de sus propias ventas: sin la ganancia (muestra costos) ni el desglose por empleado.
    pub const DEL_CAJERO: &[Self] = &[Self::Evolucion, Self::Familias, Self::Productos, Self::Clientes, Self::Metodos];
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reporte {
    pub filtro: Filtro,
    pub resumen: Resumen,
    pub apartados: Vec<Apartado>,
    /// `false` si el costo, la utilidad y el margen se ocultaron (vienen en cero).
    pub con_costos: bool,
}

impl Reporte {
    /// Borra lo que revela el costo de la mercancía.
    pub fn ocultar_costos(&mut self) {
        self.resumen.costo = Dinero::default();
        self.resumen.utilidad = Dinero::default();
        self.resumen.margen = None;
        self.apartados.retain(|a| a.seccion != Seccion::Ganancia);
        self.con_costos = false;
    }
}

/// Lo que entra en el periodo; cada consulta parte de aquí. Las devoluciones restan en el día en que
/// se hacen y se atribuyen al cajero y al cliente de la venta original.
const BASE: &str = "
    WITH
    v AS (
        SELECT * FROM ventas
        WHERE estado = 'pagada'
          AND pagada_en >= :desde AND pagada_en < date(:hasta, '+1 day')
          AND (:cajero IS NULL OR cajero_id = :cajero)
          AND (:cliente IS NULL OR cliente_id = :cliente)
    ),
    d AS (
        SELECT dv.*, ve.cajero_id AS vendedor_id, ve.cliente_id
        FROM devoluciones dv JOIN ventas ve ON ve.id = dv.venta_id
        WHERE dv.creada >= :desde AND dv.creada < date(:hasta, '+1 day')
          AND (:cajero IS NULL OR ve.cajero_id = :cajero)
          AND (:cliente IS NULL OR ve.cliente_id = :cliente)
    ),
    -- El dinero de cada venta y de cada devolución, en su fecha.
    dinero AS (
        SELECT pagada_en AS fecha, cajero_id, cliente_id, total AS monto FROM v
        UNION ALL
        SELECT creada, vendedor_id, cliente_id, -reembolso FROM d
    ),
    -- Por forma de pago: lo cobrado menos lo reembolsado.
    cobros AS (
        SELECT pg.metodo, pg.monto FROM pagos pg JOIN v ON v.id = pg.venta_id
        UNION ALL
        SELECT metodo, -reembolso FROM d
    ),
    -- La mercancía: lo vendido suma y lo devuelto resta. El costo se redondea como `Cantidad * Dinero`
    -- (las cantidades guardadas nunca son negativas).
    lineas AS (
        SELECT pa.producto_id, pa.descripcion, pa.importe,
               (pa.cantidad * pa.costo + 500) / 1000 AS costo,
               pa.cantidad AS unidades
        FROM partidas pa JOIN v ON v.id = pa.venta_id
        UNION ALL
        SELECT pa.producto_id, pa.descripcion, -pd.importe,
               -((pd.cantidad * pa.costo + 500) / 1000),
               -pd.cantidad
        FROM partidas_devueltas pd JOIN d ON d.id = pd.devolucion_id JOIN partidas pa ON pa.id = pd.partida_id
    )";

const POR_FAMILIA: &str = "
    FROM lineas l JOIN productos p ON p.id = l.producto_id LEFT JOIN familias f ON f.id = p.familia_id
    GROUP BY 1";

impl Bd {
    pub fn reporte(&self, filtro: &Filtro, secciones: &[Seccion]) -> Resultado<Reporte> {
        if filtro.desde > filtro.hasta {
            return Err(Error::invalido("La fecha inicial es posterior a la final."));
        }
        let consulta = Consulta { conn: self.conn(), filtro };
        let apartados = secciones
            .iter()
            .map(|&seccion| Ok(Apartado { seccion, titulo: seccion.etiqueta(), filas: consulta.apartado(seccion)? }))
            .collect::<Resultado<_>>()?;
        Ok(Reporte { filtro: *filtro, resumen: consulta.resumen()?, apartados, con_costos: true })
    }

    /// El reporte que le toca a quien lo pide: un administrador ve todo; un cajero con permiso, solo sus
    /// ventas y sin costos. La pantalla y el Excel salen de aquí para que muestren lo mismo.
    pub fn reporte_para(&self, usuario: &Usuario, filtro: &Filtro) -> Resultado<Reporte> {
        if usuario.es_admin() {
            return self.reporte(filtro, Seccion::TODOS);
        }
        if !usuario.puede(Permiso::VerReportes) {
            return Err(Error::SinPermiso);
        }
        let propio = Filtro { cajero_id: Some(usuario.id), ..*filtro };
        let mut reporte = self.reporte(&propio, Seccion::DEL_CAJERO)?;
        reporte.ocultar_costos();
        Ok(reporte)
    }
}

struct Consulta<'a> {
    conn: &'a Connection,
    filtro: &'a Filtro,
}

impl Consulta<'_> {
    fn resumen(&self) -> Resultado<Resumen> {
        let (ventas, vendido, devoluciones, articulos, costo): (i64, Dinero, Dinero, Cantidad, Dinero) = self.fila(
            "SELECT (SELECT count(*) FROM v), (SELECT coalesce(sum(total), 0) FROM v),
                    (SELECT coalesce(sum(reembolso), 0) FROM d),
                    (SELECT coalesce(sum(unidades), 0) FROM lineas), (SELECT coalesce(sum(costo), 0) FROM lineas)",
            |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)),
        )?;
        let total = vendido - devoluciones;
        let utilidad = total - costo;
        Ok(Resumen {
            ventas,
            vendido,
            devoluciones,
            total,
            articulos,
            promedio: if ventas > 0 { Dinero::crudo(vendido.como_crudo() / ventas) } else { Dinero::CERO },
            costo,
            utilidad,
            margen: total.es_positivo().then(|| utilidad.como_f64() / total.como_f64() * 100.0),
        })
    }

    fn apartado(&self, seccion: Seccion) -> Resultado<Vec<Fila>> {
        match seccion {
            Seccion::Evolucion => {
                let por_dia = self.lista::<NaiveDate>("SELECT date(fecha), sum(monto) FROM dinero GROUP BY 1")?;
                Ok(evolucion(self.filtro.desde, self.filtro.hasta, &por_dia))
            }
            Seccion::Familias => self.ranking::<String>(
                &format!("SELECT coalesce(f.nombre, 'Sin familia'), sum(l.importe) {POR_FAMILIA}"),
                None,
            ),
            Seccion::Ganancia => self.ranking::<String>(
                &format!("SELECT coalesce(f.nombre, 'Sin familia'), sum(l.importe - l.costo) {POR_FAMILIA}"),
                None,
            ),
            Seccion::Productos => {
                self.ranking::<String>("SELECT descripcion, sum(importe) FROM lineas GROUP BY producto_id", Some(15))
            }
            Seccion::Empleados => self.ranking::<String>(
                "SELECT u.nombre, sum(monto) FROM dinero JOIN usuarios u ON u.id = dinero.cajero_id
                 GROUP BY dinero.cajero_id",
                None,
            ),
            Seccion::Clientes => self.ranking::<String>(
                "SELECT coalesce(c.nombre, 'Público general'), sum(monto)
                 FROM dinero LEFT JOIN clientes c ON c.id = dinero.cliente_id GROUP BY dinero.cliente_id",
                Some(15),
            ),
            Seccion::Metodos => {
                self.ranking::<crate::cobro::Metodo>("SELECT metodo, sum(monto) FROM cobros GROUP BY 1", None)
            }
        }
    }

    /// `sql` agrupa en pares (etiqueta, monto); salen de mayor a menor.
    fn ranking<E: FromSql + ToString>(&self, sql: &str, limite: Option<usize>) -> Resultado<Vec<Fila>> {
        let limite = limite.map_or(-1, |l| l as i64);
        let filas = self.lista::<E>(&format!("{sql} ORDER BY 2 DESC LIMIT {limite}"))?;
        Ok(filas.into_iter().map(|(etiqueta, monto)| Fila { etiqueta: etiqueta.to_string(), monto }).collect())
    }

    fn lista<E: FromSql>(&self, sql: &str) -> Resultado<Vec<(E, Dinero)>> {
        let mut sentencia = self.conn.prepare(&format!("{BASE} {sql}"))?;
        let filas = sentencia
            .query_map(self.parametros().as_slice(), |f| Ok((f.get(0)?, f.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(filas)
    }

    fn fila<T>(&self, sql: &str, leer: impl FnOnce(&rusqlite::Row) -> rusqlite::Result<T>) -> Resultado<T> {
        Ok(self.conn.query_row(&format!("{BASE} {sql}"), self.parametros().as_slice(), leer)?)
    }

    fn parametros(&self) -> [(&str, &dyn rusqlite::ToSql); 4] {
        let Filtro { desde, hasta, cajero_id, cliente_id } = self.filtro;
        [(":desde", desde), (":hasta", hasta), (":cajero", cajero_id), (":cliente", cliente_id)]
    }
}

// — Evolución en el tiempo —

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Escala {
    Dia,
    Semana,
    Mes,
}

const MESES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];

impl Escala {
    /// Días para un par de meses, semanas hasta un año, meses para más.
    fn para(desde: NaiveDate, hasta: NaiveDate) -> Self {
        match (hasta - desde).num_days() {
            ..=62 => Self::Dia,
            63..=400 => Self::Semana,
            _ => Self::Mes,
        }
    }

    /// El primer día del periodo al que pertenece `dia`.
    fn inicio(self, dia: NaiveDate) -> NaiveDate {
        match self {
            Self::Dia => dia,
            Self::Semana => dia - Days::new(dia.weekday().num_days_from_monday().into()),
            Self::Mes => dia.with_day(1).expect("todo mes tiene día 1"),
        }
    }

    fn siguiente(self, inicio: NaiveDate) -> Option<NaiveDate> {
        match self {
            Self::Dia => inicio.checked_add_days(Days::new(1)),
            Self::Semana => inicio.checked_add_days(Days::new(7)),
            Self::Mes => inicio.checked_add_months(Months::new(1)),
        }
    }

    fn etiqueta(self, inicio: NaiveDate) -> String {
        match self {
            Self::Dia => inicio.format("%d/%m").to_string(),
            Self::Semana => inicio.format("Sem. %d/%m").to_string(),
            Self::Mes => format!("{} {}", MESES[inicio.month0() as usize], inicio.year()),
        }
    }
}

/// Suma lo vendido por periodo; los periodos sin ventas valen cero.
fn evolucion(desde: NaiveDate, hasta: NaiveDate, por_dia: &[(NaiveDate, Dinero)]) -> Vec<Fila> {
    let escala = Escala::para(desde, hasta);
    let mut por_periodo: BTreeMap<NaiveDate, Dinero> = BTreeMap::new();
    for &(dia, monto) in por_dia {
        *por_periodo.entry(escala.inicio(dia)).or_default() += monto;
    }
    successors(Some(escala.inicio(desde)), |&inicio| escala.siguiente(inicio))
        .take_while(|&inicio| inicio <= hasta)
        .map(|inicio| Fila {
            etiqueta: escala.etiqueta(inicio),
            monto: por_periodo.get(&inicio).copied().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cantidad;
    use crate::catalogo::tests::{c, d, datos};
    use crate::catalogo::{DatosFamilia, DatosProducto};
    use crate::clientes::DatosCliente;
    use crate::cobro::Metodo;
    use crate::devoluciones::ADevolver;
    use crate::usuarios::Permisos;
    use crate::ventas::tests::Tienda;

    fn dia(texto: &str) -> NaiveDate {
        texto.parse().unwrap()
    }

    fn filas(filas: &[Fila]) -> Vec<(&str, String)> {
        filas.iter().map(|f| (f.etiqueta.as_str(), f.monto.to_string())).collect()
    }

    #[test]
    fn evolucion_por_dia_semana_y_mes() {
        let ventas = [(dia("2026-01-05"), d("10")), (dia("2026-01-07"), d("5"))];
        assert_eq!(
            filas(&evolucion(dia("2026-01-05"), dia("2026-01-07"), &ventas)),
            [("05/01", "10.00".into()), ("06/01", "0.00".into()), ("07/01", "5.00".into())]
        );
        let semanas = evolucion(dia("2026-01-01"), dia("2026-06-30"), &ventas);
        assert_eq!(semanas[0].etiqueta, "Sem. 29/12");
        assert_eq!(semanas[1].monto, d("15")); // el 5 y el 7 caen en la semana del lunes 5
        let meses = evolucion(dia("2025-12-15"), dia("2027-02-01"), &ventas);
        assert_eq!((meses.len(), meses[1].etiqueta.as_str(), meses[1].monto), (15, "ene 2026", d("15")));
    }

    #[test]
    fn reporte_completo() {
        let mut t = Tienda::nueva();
        let herramientas =
            t.bd.crear_familia(&DatosFamilia { nombre: "Herramientas".into(), alerta_stock: None }).unwrap();
        let martillo = DatosProducto { familia_id: Some(herramientas.id), ..datos("MAR", "Martillo", "116", "70") };
        t.bd.actualizar_producto(t.martillo.id, &martillo).unwrap();

        t.vender(&[(&t.martillo.clone(), "2"), (&t.tornillo.clone(), "5")]); // 232 + 5
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        let jose = t.bd.crear_cliente(&DatosCliente { nombre: "José".into(), ..Default::default() }).unwrap();
        t.bd.asignar_cliente(id, Some(jose.id)).unwrap();
        t.bd.agregar(id, t.martillo.id, Cantidad::UNO, None).unwrap();
        t.bd.cobrar(id, &Tienda::efectivo("116"), None).unwrap();
        t.bd.venta_en_curso(t.cajera).unwrap(); // abierta: no cuenta

        let hoy = crate::ahora().date();
        let filtro = Filtro { desde: hoy, hasta: hoy, cajero_id: None, cliente_id: None };
        let reporte = t.bd.reporte(&filtro, Seccion::TODOS).unwrap();

        let r = &reporte.resumen;
        assert_eq!((r.ventas, r.total, r.articulos), (2, d("353"), c("8")));
        assert_eq!((r.costo, r.utilidad), (d("212"), d("141"))); // 3 × 70 + 5 × 0.40
        assert_eq!(r.promedio, d("176.50"));

        let apartado = |s: Seccion| filas(&reporte.apartados.iter().find(|a| a.seccion == s).unwrap().filas);
        assert_eq!(apartado(Seccion::Familias), [("Herramientas", "348.00".into()), ("Sin familia", "5.00".into())]);
        assert_eq!(apartado(Seccion::Ganancia), [("Herramientas", "138.00".into()), ("Sin familia", "3.00".into())]);
        assert_eq!(apartado(Seccion::Clientes), [("Público general", "237.00".into()), ("José", "116.00".into())]);
        assert_eq!(apartado(Seccion::Metodos), [("Efectivo", "353.00".into())]);
        assert_eq!(apartado(Seccion::Empleados), [("Carla", "353.00".into())]);
        assert_eq!(apartado(Seccion::Evolucion).len(), 1);

        let solo_jose = Filtro { cliente_id: Some(jose.id), ..filtro };
        assert_eq!(t.bd.reporte(&solo_jose, &[]).unwrap().resumen.total, d("116"));
        let ayer = Filtro { desde: hoy.pred_opt().unwrap(), hasta: hoy.pred_opt().unwrap(), ..filtro };
        assert_eq!(t.bd.reporte(&ayer, &[]).unwrap().resumen.ventas, 0);
        let al_reves = Filtro { desde: hoy, hasta: hoy.pred_opt().unwrap(), ..filtro };
        assert!(t.bd.reporte(&al_reves, &[]).is_err());
    }

    #[test]
    fn las_devoluciones_restan() {
        let mut t = Tienda::nueva();
        let venta = t.vender(&[(&t.martillo.clone(), "2"), (&t.tornillo.clone(), "5")]); // 232 + 5
        let permiso = t.bd.autorizar("1111").unwrap();
        let un_martillo = ADevolver { partida_id: venta.partidas[0].id, cantidad: c("1") };
        t.bd.devolver(venta.id, t.cajera, &[un_martillo], Metodo::Efectivo, "", permiso).unwrap();

        let hoy = crate::ahora().date();
        let filtro = Filtro { desde: hoy, hasta: hoy, cajero_id: None, cliente_id: None };
        let reporte = t.bd.reporte(&filtro, Seccion::TODOS).unwrap();
        let r = &reporte.resumen;
        assert_eq!((r.ventas, r.vendido, r.devoluciones, r.total), (1, d("237"), d("116"), d("121")));
        assert_eq!((r.articulos, r.costo), (c("6"), d("72"))); // 1 martillo (70) + 5 tornillos (2)

        let apartado = |s: Seccion| filas(&reporte.apartados.iter().find(|a| a.seccion == s).unwrap().filas);
        assert_eq!(apartado(Seccion::Productos), [("Martillo", "116.00".into()), ("Tornillo", "5.00".into())]);
        assert_eq!(apartado(Seccion::Metodos), [("Efectivo", "121.00".into())]);
        assert_eq!(apartado(Seccion::Empleados), [("Carla", "121.00".into())]);
        let evolucion = &reporte.apartados.iter().find(|a| a.seccion == Seccion::Evolucion).unwrap().filas;
        assert_eq!(evolucion[0].monto, d("121"));
    }

    #[test]
    fn un_cajero_solo_ve_sus_ventas_y_sin_costos() {
        let mut t = Tienda::nueva();
        t.vender(&[(&t.martillo.clone(), "1")]); // Carla: 116
        let admin = t.bd.usuario(1).unwrap();
        let id = t.bd.venta_en_curso(admin.id).unwrap().id;
        t.bd.agregar(id, t.tornillo.id, c("5"), None).unwrap();
        t.bd.cobrar(id, &Tienda::efectivo("5"), None).unwrap(); // Admin: 5

        let hoy = crate::ahora().date();
        let filtro = Filtro { desde: hoy, hasta: hoy, cajero_id: None, cliente_id: None };

        let todo = t.bd.reporte_para(&admin, &filtro).unwrap();
        assert!(todo.con_costos);
        assert_eq!((todo.resumen.total, todo.apartados.len()), (d("121"), Seccion::TODOS.len()));

        let carla = t.bd.usuario(t.cajera).unwrap();
        assert!(matches!(t.bd.reporte_para(&carla, &filtro), Err(Error::SinPermiso)));

        let carla = Usuario { permisos: Permisos { ver_reportes: true, ..Default::default() }, ..carla };
        // Aunque pida las de otro empleado, solo ve las suyas.
        let ajeno = Filtro { cajero_id: Some(admin.id), ..filtro };
        let suyo = t.bd.reporte_para(&carla, &ajeno).unwrap();
        assert_eq!(suyo.filtro.cajero_id, Some(carla.id));
        assert_eq!((suyo.resumen.ventas, suyo.resumen.total), (1, d("116")));
        assert!(!suyo.con_costos);
        assert_eq!((suyo.resumen.costo, suyo.resumen.utilidad, suyo.resumen.margen), (d("0"), d("0"), None));
        let secciones: Vec<_> = suyo.apartados.iter().map(|a| a.seccion).collect();
        assert_eq!(secciones, Seccion::DEL_CAJERO);
    }
}
