//! PDF: la cotización en hoja carta, y cualquier documento de ticket cuando no hay impresora térmica.
//! Se usan las fuentes que todo lector de PDF ya trae (Helvetica, Courier), así no hay que incluir ninguna.

use nucleo::ventas::Ticket;
use printpdf::{
    BuiltinFont, Color, Line, LinePoint, Mm, Op, PdfDocument, PdfFontHandle, PdfPage, PdfSaveOptions, Point, Pt, Rgb,
    TextItem,
};

use crate::documento::Documento;

const PUNTO: f32 = 0.352_778; // mm por punto tipográfico

/// El documento en un rollo de 80 mm, con letra de máquina para que las columnas cuadren como en el ticket.
pub fn rollo(documento: &Documento, columnas: usize) -> Vec<u8> {
    const ANCHO: f32 = 80.0;
    const MARGEN: f32 = 5.0;
    // Courier mide 0.6 em por carácter: el tamaño que llena el ancho útil con `columnas` caracteres.
    let tamano = (ANCHO - 2.0 * MARGEN) / PUNTO / (columnas as f32 * 0.6);

    let renglones = documento.renglones(columnas);
    let alto_de = |grande: bool| tamano * if grande { 2.0 } else { 1.0 } * 1.25 * PUNTO;
    let alto = 2.0 * MARGEN + renglones.iter().map(|r| alto_de(r.estilo.grande)).sum::<f32>();

    let mut ops = Vec::new();
    let mut y = alto - MARGEN;
    for renglon in &renglones {
        let grande = renglon.estilo.grande;
        y -= alto_de(grande);
        let fuente = if renglon.estilo.negrita { BuiltinFont::CourierBold } else { BuiltinFont::Courier };
        escribir(
            &mut ops,
            MARGEN,
            y + 0.25 * alto_de(grande),
            fuente,
            tamano * if grande { 2.0 } else { 1.0 },
            &renglon.texto,
        );
    }
    guardar("Ticket", vec![PdfPage::new(Mm(ANCHO), Mm(alto), ops)])
}

/// La cotización en hoja carta, lista para imprimir o mandar al cliente.
pub fn cotizacion(ticket: &Ticket) -> Vec<u8> {
    let Ticket { negocio, venta, .. } = ticket;
    let mut hoja = Hoja::nueva();

    // Encabezado: el negocio a la izquierda, el folio a la derecha.
    hoja.texto(MARGEN_HOJA, BuiltinFont::HelveticaBold, 18.0, &negocio.nombre);
    hoja.texto_derecha(ANCHO_HOJA - MARGEN_HOJA, BuiltinFont::Helvetica, 9.0, "COTIZACIÓN");
    hoja.bajar(7.0);
    let folio = format!("N.º {}", venta.folio_cotizacion.unwrap_or_default());
    hoja.texto_derecha(ANCHO_HOJA - MARGEN_HOJA, BuiltinFont::HelveticaBold, 16.0, &folio);
    for (prefijo, dato) in [("", &negocio.direccion), ("Tel. ", &negocio.telefono), ("RFC ", &negocio.rfc)] {
        if !dato.is_empty() {
            hoja.texto(MARGEN_HOJA, BuiltinFont::Helvetica, 9.5, &format!("{prefijo}{dato}"));
            hoja.bajar(4.5);
        }
    }
    hoja.bajar(2.0);
    hoja.raya(0.8);
    hoja.bajar(8.0);

    let cliente = venta.cliente.as_deref().unwrap_or("Público general");
    let mut datos = vec![("Cliente", cliente.to_owned()), ("Fecha", venta.creada.format("%d/%m/%Y").to_string())];
    if let Some(vigencia) = venta.vigencia {
        datos.push(("Válida hasta", vigencia.format("%d/%m/%Y").to_string()));
    }
    datos.push(("Atendió", venta.cajero.clone()));
    for (etiqueta, valor) in datos {
        hoja.texto(MARGEN_HOJA, BuiltinFont::Helvetica, 10.0, etiqueta);
        hoja.texto(MARGEN_HOJA + 30.0, BuiltinFont::Helvetica, 10.0, &valor);
        hoja.bajar(5.5);
    }
    hoja.bajar(4.0);

    // Tabla de partidas.
    let columnas = [MARGEN_HOJA, MARGEN_HOJA + 32.0]; // código, descripción
    let derecha = [ANCHO_HOJA - MARGEN_HOJA - 62.0, ANCHO_HOJA - MARGEN_HOJA - 30.0, ANCHO_HOJA - MARGEN_HOJA]; // cant., precio, importe
    let encabezados = |hoja: &mut Hoja| {
        hoja.texto(columnas[0], BuiltinFont::HelveticaBold, 8.5, "CÓDIGO");
        hoja.texto(columnas[1], BuiltinFont::HelveticaBold, 8.5, "DESCRIPCIÓN");
        for (x, titulo) in derecha.iter().zip(["CANTIDAD", "PRECIO", "IMPORTE"]) {
            hoja.texto_derecha(*x, BuiltinFont::HelveticaBold, 8.5, titulo);
        }
        hoja.bajar(2.0);
        hoja.raya(0.4);
        hoja.bajar(5.5);
    };
    encabezados(&mut hoja);
    let ancho_descripcion = derecha[0] - 26.0 - columnas[1];
    for p in &venta.partidas {
        let lineas = partir_por_ancho(&p.descripcion, ancho_descripcion, 10.0);
        if hoja.y - 6.0 * lineas.len() as f32 <= MARGEN_HOJA + 30.0 {
            hoja.nueva_pagina();
            encabezados(&mut hoja);
        }
        hoja.texto(columnas[0], BuiltinFont::Helvetica, 9.0, &p.codigo);
        hoja.texto_derecha(derecha[0], BuiltinFont::Helvetica, 10.0, &format!("{} {}", p.cantidad, p.unidad));
        hoja.texto_derecha(derecha[1], BuiltinFont::Helvetica, 10.0, &crate::documento::precio_por(p));
        hoja.texto_derecha(derecha[2], BuiltinFont::Helvetica, 10.0, &p.importe.pesos());
        for (i, linea) in lineas.iter().enumerate() {
            if i > 0 {
                hoja.bajar(4.5);
            }
            hoja.texto(columnas[1], BuiltinFont::Helvetica, 10.0, linea);
        }
        hoja.bajar(2.5);
        hoja.raya_gris();
        hoja.bajar(5.0);
    }

    // Totales.
    hoja.bajar(2.0);
    let t = &venta.totales;
    let mut totales = vec![("Subtotal", t.subtotal.pesos(), false), ("IVA", t.iva.pesos(), false)];
    if t.descuento.es_positivo() {
        totales.insert(0, ("Descuento", format!("-{}", t.descuento.pesos()), false));
    }
    totales.push(("Total", t.total.pesos(), true));
    for (etiqueta, monto, fuerte) in totales {
        let (fuente, tamano) = if fuerte { (BuiltinFont::HelveticaBold, 13.0) } else { (BuiltinFont::Helvetica, 10.0) };
        hoja.texto(derecha[1] - 25.0, fuente, tamano, etiqueta);
        hoja.texto_derecha(derecha[2], fuente, tamano, &monto);
        hoja.bajar(if fuerte { 7.0 } else { 5.5 });
    }
    if !venta.nota.is_empty() {
        hoja.bajar(4.0);
        hoja.texto(MARGEN_HOJA, BuiltinFont::Helvetica, 10.0, &venta.nota);
    }
    hoja.bajar(10.0);
    let pie = "Precios con IVA incluido, sujetos a existencia y a la vigencia de esta cotización.";
    hoja.texto(MARGEN_HOJA, BuiltinFont::Helvetica, 8.5, pie);

    guardar("Cotización", hoja.paginas())
}

const ANCHO_HOJA: f32 = 215.9;
const ALTO_HOJA: f32 = 279.4;
const MARGEN_HOJA: f32 = 18.0;

/// Escribe de arriba hacia abajo en hojas carta, pasando a la siguiente cuando se acaba el espacio.
struct Hoja {
    paginas: Vec<Vec<Op>>,
    y: f32,
}

impl Hoja {
    fn nueva() -> Self {
        Self { paginas: vec![Vec::new()], y: ALTO_HOJA - MARGEN_HOJA }
    }

    fn nueva_pagina(&mut self) {
        self.paginas.push(Vec::new());
        self.y = ALTO_HOJA - MARGEN_HOJA;
    }

    fn ops(&mut self) -> &mut Vec<Op> {
        self.paginas.last_mut().expect("siempre hay una página")
    }

    fn bajar(&mut self, mm: f32) {
        self.y -= mm;
    }

    fn texto(&mut self, x: f32, fuente: BuiltinFont, tamano: f32, texto: &str) {
        let y = self.y;
        escribir(self.ops(), x, y, fuente, tamano, texto);
    }

    fn texto_derecha(&mut self, x: f32, fuente: BuiltinFont, tamano: f32, texto: &str) {
        self.texto(x - ancho_helvetica(texto, tamano, fuente == BuiltinFont::HelveticaBold), fuente, tamano, texto);
    }

    fn raya(&mut self, grosor: f32) {
        let y = self.y;
        trazar(self.ops(), MARGEN_HOJA, ANCHO_HOJA - MARGEN_HOJA, y, grosor, 0.06);
    }

    fn raya_gris(&mut self) {
        let y = self.y;
        trazar(self.ops(), MARGEN_HOJA, ANCHO_HOJA - MARGEN_HOJA, y, 0.3, 0.8);
    }

    fn paginas(self) -> Vec<PdfPage> {
        self.paginas.into_iter().map(|ops| PdfPage::new(Mm(ANCHO_HOJA), Mm(ALTO_HOJA), ops)).collect()
    }
}

fn escribir(ops: &mut Vec<Op>, x: f32, y: f32, fuente: BuiltinFont, tamano: f32, texto: &str) {
    ops.extend([
        Op::StartTextSection,
        Op::SetFillColor { col: gris(0.07) },
        Op::SetFont { font: PdfFontHandle::Builtin(fuente), size: Pt(tamano) },
        Op::SetTextCursor { pos: Point::new(Mm(x), Mm(y)) },
        Op::ShowText { items: vec![TextItem::Text(texto.to_owned())] },
        Op::EndTextSection,
    ]);
}

fn trazar(ops: &mut Vec<Op>, desde: f32, hasta: f32, y: f32, grosor: f32, tono: f32) {
    let punto = |x: f32| LinePoint { p: Point::new(Mm(x), Mm(y)), bezier: false };
    ops.extend([
        Op::SetOutlineColor { col: gris(tono) },
        Op::SetOutlineThickness { pt: Pt(grosor) },
        Op::DrawLine { line: Line { points: vec![punto(desde), punto(hasta)], is_closed: false } },
    ]);
}

fn gris(tono: f32) -> Color {
    Color::Rgb(Rgb { r: tono, g: tono, b: tono, icc_profile: None })
}

fn guardar(titulo: &str, paginas: Vec<PdfPage>) -> Vec<u8> {
    PdfDocument::new(titulo).with_pages(paginas).save(&PdfSaveOptions::default(), &mut Vec::new())
}

/// Ancho aproximado de un texto en Helvetica, en mm (las medidas de la fuente, en milésimas de em).
fn ancho_helvetica(texto: &str, tamano: f32, negrita: bool) -> f32 {
    let milesimas: u32 = texto
        .chars()
        .map(|c| match c {
            '0'..='9' | '$' | 'a' | 'b' | 'd' | 'e' | 'g' | 'h' | 'n' | 'o' | 'p' | 'q' | 'u' => 556,
            '.' | ',' | ' ' | '/' | 'i' | 'l' | 'j' | 'f' | 't' | 'I' => 278,
            '-' | 'r' | '(' | ')' => 333,
            'c' | 'k' | 's' | 'v' | 'x' | 'y' | 'z' => 500,
            'm' | 'M' => 833,
            'w' | 'W' => 722,
            'Ó' | 'O' | 'C' | 'G' | 'Q' => 778,
            'A'..='Z' | 'Á' | 'É' | 'Í' | 'Ú' | 'Ñ' => 667,
            'º' => 365,
            _ => 556,
        })
        .sum();
    let factor = if negrita { 1.04 } else { 1.0 };
    milesimas as f32 / 1000.0 * tamano * PUNTO * factor
}

/// Parte en renglones que quepan en `ancho` mm con Helvetica de `tamano` puntos.
fn partir_por_ancho(texto: &str, ancho: f32, tamano: f32) -> Vec<String> {
    let mut renglones = Vec::new();
    let mut actual = String::new();
    for palabra in texto.split_whitespace() {
        let candidato = if actual.is_empty() { palabra.to_owned() } else { format!("{actual} {palabra}") };
        if !actual.is_empty() && ancho_helvetica(&candidato, tamano, false) > ancho {
            renglones.push(std::mem::replace(&mut actual, palabra.to_owned()));
        } else {
            actual = candidato;
        }
    }
    renglones.push(actual);
    renglones
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parte_descripciones_largas() {
        let lineas =
            partir_por_ancho("Tornillo para madera de cabeza plana 8 × 1 1/4 pulgadas galvanizado", 40.0, 10.0);
        assert!(lineas.len() > 1);
        assert!(lineas.iter().all(|l| ancho_helvetica(l, 10.0, false) <= 40.0 || !l.contains(' ')));
    }
}
