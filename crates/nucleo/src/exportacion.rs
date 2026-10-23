//! Hojas de Excel para llevarse los datos: el catálogo (que se puede volver a importar) y los reportes.

use rust_xlsxwriter::{Format, Workbook, Worksheet};

use crate::Resultado;
use crate::catalogo::Producto;
use crate::negocio::Negocio;
use crate::reportes::Reporte;
use crate::ventas::Venta;

/// Mismas columnas que reconoce la importación: se edita en Excel y se vuelve a subir.
pub fn productos_a_excel(productos: &[Producto]) -> Resultado<Vec<u8>> {
    let mut libro = Workbook::new();
    let hoja = libro.add_worksheet().set_name("Productos")?;
    let titulos =
        ["Código", "Código de barras", "Nombre", "Familia", "Precio", "Costo", "Existencia", "Unidad", "A granel"];
    encabezados(hoja, &titulos)?;
    for (fila, p) in (1..).zip(productos) {
        hoja.write_string(fila, 0, &p.codigo)?; // como texto: conserva ceros a la izquierda
        hoja.write_string(fila, 1, p.codigo_barras.as_deref().unwrap_or_default())?;
        hoja.write_string(fila, 2, &p.nombre)?;
        hoja.write_string(fila, 3, p.familia.as_deref().unwrap_or_default())?;
        hoja.write_number_with_format(fila, 4, p.precio.como_f64(), &pesos())?;
        hoja.write_number_with_format(fila, 5, p.costo.como_f64(), &pesos())?;
        hoja.write_number(fila, 6, p.existencia.como_f64())?;
        hoja.write_string(fila, 7, &p.unidad)?;
        hoja.write_string(fila, 8, if p.granel { "Sí" } else { "No" })?;
    }
    hoja.autofit();
    Ok(libro.save_to_buffer()?)
}

/// Una hoja con el resumen y una por cada apartado del reporte.
pub fn reporte_a_excel(reporte: &Reporte) -> Resultado<Vec<u8>> {
    let mut libro = Workbook::new();
    let r = &reporte.resumen;
    let hoja = libro.add_worksheet().set_name("Resumen")?;
    let periodo = format!("{} al {}", reporte.filtro.desde.format("%d/%m/%Y"), reporte.filtro.hasta.format("%d/%m/%Y"));
    hoja.write_string_with_format(0, 0, "Periodo", &negrita())?;
    hoja.write_string(0, 1, &periodo)?;

    let mut montos = vec![
        ("Vendido", r.vendido),
        ("Devoluciones", r.devoluciones),
        ("Total", r.total),
        ("Ticket promedio", r.promedio),
    ];
    if reporte.con_costos {
        montos.extend([("Costo", r.costo), ("Utilidad", r.utilidad)]);
    }
    hoja.write_string_with_format(2, 0, "Ventas", &negrita())?;
    hoja.write_number(2, 1, r.ventas as f64)?;
    hoja.write_string_with_format(3, 0, "Artículos", &negrita())?;
    hoja.write_number(3, 1, r.articulos.como_f64())?;
    for (fila, (etiqueta, monto)) in (4..).zip(montos) {
        hoja.write_string_with_format(fila, 0, etiqueta, &negrita())?;
        hoja.write_number_with_format(fila, 1, monto.como_f64(), &pesos())?;
    }
    if let Some(margen) = r.margen.filter(|_| reporte.con_costos) {
        hoja.write_string_with_format(10, 0, "Margen", &negrita())?;
        hoja.write_number_with_format(10, 1, margen / 100.0, &Format::new().set_num_format("0.0%"))?;
    }
    hoja.autofit();

    for apartado in &reporte.apartados {
        let hoja = libro.add_worksheet().set_name(apartado.titulo)?;
        encabezados(hoja, &[apartado.titulo, "Monto"])?;
        for (fila, f) in (1..).zip(&apartado.filas) {
            hoja.write_string(fila, 0, &f.etiqueta)?;
            hoja.write_number_with_format(fila, 1, f.monto.como_f64(), &pesos())?;
        }
        hoja.autofit();
    }
    Ok(libro.save_to_buffer()?)
}

/// Una cotización lista para mandarle al cliente.
pub fn cotizacion_a_excel(negocio: &Negocio, cotizacion: &Venta) -> Resultado<Vec<u8>> {
    let mut libro = Workbook::new();
    let folio = cotizacion.folio_cotizacion.unwrap_or_default();
    let hoja = libro.add_worksheet().set_name(format!("Cotización {folio}"))?;

    hoja.write_string_with_format(0, 0, &negocio.nombre, &Format::new().set_bold().set_font_size(14))?;
    let contacto = [negocio.direccion.as_str(), negocio.telefono.as_str(), negocio.rfc.as_str()];
    hoja.write_string(1, 0, contacto.iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join(" · "))?;

    let datos = [
        ("Cotización", folio.to_string()),
        ("Fecha", cotizacion.creada.format("%d/%m/%Y").to_string()),
        ("Válida hasta", cotizacion.vigencia.map(|v| v.format("%d/%m/%Y").to_string()).unwrap_or_default()),
        ("Cliente", cotizacion.cliente.clone().unwrap_or_else(|| "Público general".into())),
    ];
    for (fila, (etiqueta, valor)) in (3..).zip(datos) {
        hoja.write_string_with_format(fila, 0, etiqueta, &negrita())?;
        hoja.write_string(fila, 1, valor)?;
    }

    let inicio = 8;
    let titulos = ["Código", "Descripción", "Cantidad", "Unidad", "Precio", "Importe"];
    for (columna, titulo) in (0..).zip(titulos) {
        hoja.write_string_with_format(inicio, columna, titulo, &negrita())?;
    }
    for (fila, p) in (inicio + 1..).zip(&cotizacion.partidas) {
        hoja.write_string(fila, 0, &p.codigo)?;
        hoja.write_string(fila, 1, &p.descripcion)?;
        hoja.write_number(fila, 2, p.cantidad.como_f64())?;
        hoja.write_string(fila, 3, &p.unidad)?;
        hoja.write_number_with_format(fila, 4, p.precio.como_f64(), &pesos())?;
        hoja.write_number_with_format(fila, 5, p.importe.como_f64(), &pesos())?;
    }

    let t = &cotizacion.totales;
    let mut totales = vec![("Subtotal", t.subtotal), ("IVA", t.iva), ("Total", t.total)];
    if t.descuento.es_positivo() {
        totales.insert(0, ("Descuento", -t.descuento));
    }
    let despues = inicio + 2 + cotizacion.partidas.len() as u32;
    for (fila, (etiqueta, monto)) in (despues..).zip(totales) {
        hoja.write_string_with_format(fila, 4, etiqueta, &negrita())?;
        hoja.write_number_with_format(fila, 5, monto.como_f64(), &pesos())?;
    }
    hoja.autofit();
    Ok(libro.save_to_buffer()?)
}

fn encabezados(hoja: &mut Worksheet, titulos: &[&str]) -> Resultado<()> {
    for (columna, titulo) in (0..).zip(titulos) {
        hoja.write_string_with_format(0, columna, *titulo, &negrita())?;
    }
    hoja.set_freeze_panes(1, 0)?;
    Ok(())
}

fn negrita() -> Format {
    Format::new().set_bold()
}

fn pesos() -> Format {
    Format::new().set_num_format("$#,##0.00")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reportes::{Filtro, Seccion};
    use crate::ventas::tests::Tienda;

    #[test]
    fn exporta_una_cotizacion() {
        let mut t = Tienda::nueva();
        let id = t.bd.venta_en_curso(t.cajera).unwrap().id;
        t.bd.agregar(id, t.martillo.id, crate::Cantidad::UNO, None).unwrap();
        let cotizacion = t.bd.cotizar(id, 15, "").unwrap();
        let bytes = cotizacion_a_excel(&t.bd.negocio().unwrap(), &cotizacion).unwrap();
        assert!(bytes.starts_with(b"PK"));
    }

    #[test]
    fn exporta_un_reporte() {
        let mut t = Tienda::nueva();
        t.vender(&[(&t.martillo.clone(), "1")]);
        let hoy = crate::ahora().date();
        let filtro = Filtro { desde: hoy, hasta: hoy, cajero_id: None, cliente_id: None };
        let reporte = t.bd.reporte(&filtro, Seccion::TODOS).unwrap();
        let bytes = reporte_a_excel(&reporte).unwrap();
        assert!(bytes.starts_with(b"PK")); // un .xlsx es un zip
        assert!(textos(&bytes).iter().any(|t| t == "Utilidad"));
    }

    #[test]
    fn el_reporte_sin_costos_no_los_exporta() {
        let mut t = Tienda::nueva();
        t.vender(&[(&t.martillo.clone(), "1")]);
        let hoy = crate::ahora().date();
        let filtro = Filtro { desde: hoy, hasta: hoy, cajero_id: None, cliente_id: None };
        let mut reporte = t.bd.reporte(&filtro, Seccion::TODOS).unwrap();
        reporte.ocultar_costos();
        let textos = textos(&reporte_a_excel(&reporte).unwrap());
        assert!(textos.iter().any(|t| t == "Total"));
        for oculto in ["Costo", "Utilidad", "Margen", Seccion::Ganancia.etiqueta()] {
            assert!(!textos.iter().any(|t| t == oculto), "{oculto}");
        }
    }

    /// Todos los textos del libro, de todas las hojas, más el nombre de cada hoja.
    fn textos(bytes: &[u8]) -> Vec<String> {
        use calamine::{Data, Reader, Xlsx};
        let mut libro = Xlsx::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut textos = libro.sheet_names();
        for (_, hoja) in libro.worksheets() {
            textos.extend(hoja.used_cells().filter_map(|(_, _, c)| match c {
                Data::String(s) => Some(s.clone()),
                _ => None,
            }));
        }
        textos
    }
}
