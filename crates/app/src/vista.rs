//! De los datos del núcleo a lo que muestra la interfaz: textos ya formateados, listos para pintar.

use chrono::{Local, NaiveDate, NaiveDateTime};
use nucleo::caja::{Caja, Corte};
use nucleo::catalogo::{Familia, Movimiento, Producto};
use nucleo::clientes::Cliente;
use nucleo::devoluciones::Devolucion;
use nucleo::importacion::{Accion, Vista};
use nucleo::reportes::{Fila, Reporte};
use nucleo::usuarios::Usuario;
use nucleo::ventas::{Estado, Partida, Venta};
use nucleo::{Cantidad, Dinero};
use slint::{ModelRc, SharedString, VecModel};

use crate::{
    ApartadoUi, ClienteUi, CorteUi, FamiliaUi, FilaUi, ImportacionUi, MovimientoUi, PartidaUi, ProductoUi, ReporteUi,
    UsuarioUi, VentaUi,
};

pub fn modelo<T: Clone + 'static>(elementos: impl IntoIterator<Item = T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(elementos.into_iter().collect::<Vec<_>>()))
}

pub fn texto(t: impl AsRef<str>) -> SharedString {
    SharedString::from(t.as_ref())
}

pub fn pesos(monto: Dinero) -> SharedString {
    monto.pesos().into()
}

pub fn hoy() -> NaiveDate {
    Local::now().date_naive()
}

const DIAS: [&str; 7] = ["lun", "mar", "mié", "jue", "vie", "sáb", "dom"];

/// «Hoy 10:15», «Ayer 18:40», «jue 08/10 10:15»: lo que importa al buscar una venta en pausa.
pub fn cuando(momento: NaiveDateTime) -> String {
    let dias = (hoy() - momento.date()).num_days();
    let hora = momento.format("%H:%M");
    match dias {
        0 => format!("Hoy {hora}"),
        1 => format!("Ayer {hora}"),
        _ => {
            let dia = DIAS[momento.format("%u").to_string().parse::<usize>().unwrap_or(1) - 1];
            format!("{dia} {} {hora} (hace {dias} días)", momento.format("%d/%m"))
        }
    }
}

pub fn fecha_corta(momento: NaiveDateTime) -> String {
    momento.format("%d/%m/%Y %H:%M").to_string()
}

pub fn usuario(u: &Usuario) -> UsuarioUi {
    UsuarioUi {
        id: u.id as i32,
        nombre: texto(&u.nombre),
        rol: texto(u.rol.etiqueta()),
        admin: u.es_admin(),
        activo: u.activo,
    }
}

pub fn producto(p: &Producto) -> ProductoUi {
    ProductoUi {
        id: p.id as i32,
        codigo: texto(&p.codigo),
        nombre: texto(&p.nombre),
        familia: texto(p.familia.as_deref().unwrap_or_default()),
        precio: pesos(p.precio),
        precio_numero: p.precio.como_f64() as f32,
        costo: pesos(p.costo),
        existencia: texto(p.existencia.to_string()),
        unidad: texto(&p.unidad),
        granel: p.granel,
        activo: p.activo,
        alerta: p.en_alerta(),
        otros_precios: p.precios.len() as i32,
    }
}

pub fn partida(p: &Partida, a_devolver: Option<Cantidad>) -> PartidaUi {
    PartidaUi {
        id: p.id as i32,
        codigo: texto(&p.codigo),
        descripcion: texto(&p.descripcion),
        cantidad: texto(p.cantidad.to_string()),
        unidad: texto(&p.unidad),
        precio: texto(match p.unidad.as_str() {
            "kg" => format!("{}/kg", p.precio.pesos()),
            _ => p.precio.pesos(),
        }),
        importe: pesos(p.importe),
        fijo: p.precio_fijo,
        bajo_costo: p.bajo_costo(),
        devuelto: texto(if p.devuelto.es_positivo() { p.devuelto.to_string() } else { "—".into() }),
        pendiente: texto((p.cantidad - p.devuelto).to_string()),
        a_devolver: texto(a_devolver.map(|c| c.to_string()).unwrap_or_default()),
        sugerencia: texto(
            p.sugerencia.as_ref().map(|s| format!("{} {}", s.nombre, s.precio.pesos())).unwrap_or_default(),
        ),
    }
}

/// `devoluciones`: solo cuando se muestra el detalle de una venta cobrada.
pub fn venta(v: &Venta, devoluciones: &[Devolucion], a_devolver: impl Fn(i64) -> Option<Cantidad>) -> VentaUi {
    let fecha = match v.estado {
        Estado::Pausada => cuando(v.pausada_en.unwrap_or(v.creada)),
        Estado::Pagada | Estado::Cancelada => v.pagada_en.map(|f| f.format("%H:%M").to_string()).unwrap_or_default(),
        _ => v.creada.format("%d/%m/%Y").to_string(),
    };
    let mut formas: Vec<&str> = v.pagos.iter().map(|p| p.metodo.etiqueta()).collect();
    formas.dedup();
    let articulos: Cantidad = v.partidas.iter().map(|p| p.cantidad).sum();
    VentaUi {
        id: v.id as i32,
        folio: texto(v.folio.or(v.folio_cotizacion).map(|f| f.to_string()).unwrap_or_default()),
        estado: texto(v.estado.clave()),
        cliente: texto(v.cliente.as_deref().unwrap_or("Público general")),
        cajero: texto(&v.cajero),
        nota: texto(&v.nota),
        fecha: texto(fecha),
        descuento: pesos(v.totales.descuento),
        subtotal: pesos(v.totales.subtotal),
        iva: pesos(v.totales.iva),
        total: pesos(v.totales.total),
        articulos: texto(articulos.to_string()),
        partidas: modelo(v.partidas.iter().map(|p| partida(p, a_devolver(p.id)))),
        pagos: texto(formas.join(" + ")),
        vigencia: texto(v.vigencia.map(|f| f.format("%d/%m/%Y").to_string()).unwrap_or_default()),
        vencida: v.vigencia.is_some_and(|f| f < hoy()),
        con_devolucion: v.partidas.iter().any(|p| p.devuelto.es_positivo()),
        se_puede_devolver: v.partidas.iter().any(|p| p.devuelto < p.cantidad),
        devoluciones: modelo(devoluciones.iter().map(|d| FilaUi {
            id: d.id as i32,
            etiqueta: texto(format!("{} · {} · {}", fecha_corta(d.creada), d.metodo.etiqueta(), d.motivo)),
            valor: pesos(d.reembolso),
            proporcion: 0.0,
        })),
    }
}

pub fn cliente(c: &Cliente) -> ClienteUi {
    ClienteUi {
        id: c.id as i32,
        nombre: texto(&c.datos.nombre),
        telefono: texto(&c.datos.telefono),
        correo: texto(&c.datos.correo),
        rfc: texto(&c.datos.rfc),
        notas: texto(&c.datos.notas),
    }
}

pub fn familia(f: &Familia) -> FamiliaUi {
    FamiliaUi {
        id: f.id as i32,
        nombre: texto(&f.nombre),
        alerta: texto(f.alerta_stock.map(|a| a.to_string()).unwrap_or_default()),
    }
}

pub fn movimiento(m: &Movimiento) -> MovimientoUi {
    MovimientoUi {
        fecha: texto(m.creado.format("%d/%m %H:%M").to_string()),
        tipo: texto(m.tipo.etiqueta()),
        cantidad: texto(if m.cantidad.es_positivo() { format!("+{}", m.cantidad) } else { m.cantidad.to_string() }),
        referencia: texto(&m.referencia),
    }
}

fn filas(montos: impl IntoIterator<Item = (String, Dinero)>) -> ModelRc<FilaUi> {
    modelo(montos.into_iter().map(|(etiqueta, monto)| FilaUi {
        id: 0,
        etiqueta: texto(etiqueta),
        valor: pesos(monto),
        proporcion: 0.0,
    }))
}

pub fn caja_cerrada() -> CorteUi {
    CorteUi { abierta: false, ..Default::default() }
}

pub fn corte(c: &Corte) -> CorteUi {
    let caja: &Caja = &c.caja;
    let (etiqueta, diferencia) = match c.diferencia {
        None => ("", Dinero::CERO),
        Some(d) if d == Dinero::CERO => ("Cuadra exacto", d),
        Some(d) if d.es_positivo() => ("Sobrante", d),
        Some(d) => ("Faltante", -d),
    };
    CorteUi {
        abierta: caja.cerrada_en.is_none(),
        id: caja.id as i32,
        abrio: texto(&caja.abrio),
        cerro: texto(caja.cerro.as_deref().unwrap_or_default()),
        desde: texto(fecha_corta(caja.abierta_en)),
        hasta: texto(caja.cerrada_en.map(fecha_corta).unwrap_or_default()),
        fondo: pesos(caja.fondo_inicial),
        ventas: texto(c.ventas.to_string()),
        total: pesos(c.total),
        por_metodo: filas(c.por_metodo.iter().map(|m| (m.metodo.etiqueta().into(), m.monto))),
        por_cajero: filas(c.por_cajero.iter().map(|p| (format!("{} ({})", p.cajero, p.ventas), p.total))),
        devoluciones: filas(c.devoluciones.iter().map(|m| (m.metodo.etiqueta().to_lowercase(), m.monto))),
        esperado: pesos(c.efectivo_esperado),
        contado: texto(caja.efectivo_contado.map(|d| d.pesos()).unwrap_or_default()),
        diferencia_etiqueta: texto(etiqueta),
        diferencia: if etiqueta.is_empty() { texto("") } else { pesos(diferencia) },
    }
}

pub fn importacion(vista: &Vista) -> (ModelRc<ImportacionUi>, String) {
    let filas = vista.filas.iter().filter(|f| f.accion != Accion::SinCambios).map(|f| {
        let (accion, mut detalle) = match &f.accion {
            Accion::Crear => ("Nuevo", Vec::new()),
            Accion::Actualizar { cambios } => ("Cambia", cambios.clone()),
            Accion::SinCambios => ("Igual", Vec::new()),
            Accion::Error { mensaje } => ("Error", vec![mensaje.clone()]),
        };
        detalle.extend(f.notas.iter().cloned()); // variante, renglones juntados, código generado…
        let detalle = detalle.join(" · ");
        ImportacionUi {
            numero: texto(f.numero.to_string()),
            codigo: texto(&f.codigo),
            nombre: texto(&f.nombre),
            accion: texto(accion),
            detalle: texto(detalle),
        }
    });
    let resumen = format!(
        "{} nuevos · {} con cambios · {} sin cambios · {} con error (se saltan). Lo que no venga en la hoja no se toca.",
        vista.crear, vista.actualizar, vista.sin_cambios, vista.errores
    );
    (modelo(filas), resumen)
}

pub fn reporte(r: &Reporte) -> ReporteUi {
    let rs = &r.resumen;
    let periodo = if r.filtro.desde == r.filtro.hasta {
        r.filtro.desde.format("%d/%m/%Y").to_string()
    } else {
        format!("{} al {}", r.filtro.desde.format("%d/%m/%Y"), r.filtro.hasta.format("%d/%m/%Y"))
    };
    ReporteUi {
        periodo: texto(periodo),
        ventas: texto(rs.ventas.to_string()),
        vendido: pesos(rs.vendido),
        devoluciones: pesos(rs.devoluciones),
        total: pesos(rs.total),
        promedio: pesos(rs.promedio),
        utilidad: pesos(rs.utilidad),
        margen: texto(rs.margen.map(|m| format!("{m:.1} % de margen")).unwrap_or_default()),
        apartados: modelo(r.apartados.iter().map(|a| ApartadoUi { titulo: texto(a.titulo), filas: barras(&a.filas) })),
    }
}

/// Cada fila con su proporción respecto a la mayor, para dibujar la barra.
fn barras(filas: &[Fila]) -> ModelRc<FilaUi> {
    let maximo = filas.iter().map(|f| f.monto.como_f64()).fold(0.0, f64::max);
    modelo(filas.iter().map(|f| FilaUi {
        id: 0,
        etiqueta: texto(&f.etiqueta),
        valor: pesos(f.monto),
        proporcion: if maximo > 0.0 { (f.monto.como_f64() / maximo) as f32 } else { 0.0 },
    }))
}
