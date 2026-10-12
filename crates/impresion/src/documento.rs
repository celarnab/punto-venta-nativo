//! Lo que se imprime, sin importar en qué: renglones con un poco de formato. De aquí salen tanto los
//! bytes ESC/POS para la impresora térmica como el PDF de respaldo.

use nucleo::caja::Corte;
use nucleo::devoluciones::ComprobanteDevolucion;
use nucleo::negocio::Negocio;
use nucleo::ventas::{Estado, Ticket};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Estilo {
    pub centrado: bool,
    pub negrita: bool,
    /// Doble alto y ancho: cabe la mitad de caracteres por renglón.
    pub grande: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Linea {
    Texto(String, Estilo),
    /// Etiqueta a la izquierda y valor a la derecha («Total ....... $120.00»).
    Par {
        izquierda: String,
        derecha: String,
        estilo: Estilo,
    },
    Separador,
    Espacio,
    /// Una raya para firmar, con su leyenda abajo.
    Firma(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Documento {
    pub lineas: Vec<Linea>,
}

/// Un renglón ya acomodado al ancho del papel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Renglon {
    pub texto: String,
    pub estilo: Estilo,
}

const CENTRADO: Estilo = Estilo { centrado: true, negrita: false, grande: false };
const NEGRITA: Estilo = Estilo { centrado: false, negrita: true, grande: false };
const TITULO: Estilo = Estilo { centrado: true, negrita: true, grande: false };
const TOTAL: Estilo = Estilo { centrado: false, negrita: true, grande: true };

impl Documento {
    fn texto(&mut self, texto: impl Into<String>) -> &mut Self {
        self.con_estilo(texto, Estilo::default())
    }

    fn con_estilo(&mut self, texto: impl Into<String>, estilo: Estilo) -> &mut Self {
        self.lineas.push(Linea::Texto(texto.into(), estilo));
        self
    }

    fn par(&mut self, izquierda: impl Into<String>, derecha: impl Into<String>) -> &mut Self {
        self.par_con_estilo(izquierda, derecha, Estilo::default())
    }

    fn par_con_estilo(
        &mut self,
        izquierda: impl Into<String>,
        derecha: impl Into<String>,
        estilo: Estilo,
    ) -> &mut Self {
        self.lineas.push(Linea::Par { izquierda: izquierda.into(), derecha: derecha.into(), estilo });
        self
    }

    fn separador(&mut self) -> &mut Self {
        self.lineas.push(Linea::Separador);
        self
    }

    fn espacio(&mut self) -> &mut Self {
        self.lineas.push(Linea::Espacio);
        self
    }

    fn encabezado(&mut self, negocio: &Negocio) -> &mut Self {
        self.con_estilo(&negocio.nombre, Estilo { grande: true, ..TITULO });
        for dato in
            [negocio.direccion.clone(), con_prefijo("Tel. ", &negocio.telefono), con_prefijo("RFC ", &negocio.rfc)]
        {
            if !dato.is_empty() {
                self.con_estilo(dato, CENTRADO);
            }
        }
        self.separador()
    }

    /// Acomoda el documento a `columnas` caracteres por renglón.
    pub fn renglones(&self, columnas: usize) -> Vec<Renglon> {
        let mut renglones = Vec::new();
        for linea in &self.lineas {
            match linea {
                Linea::Texto(texto, estilo) => {
                    let ancho = ancho_para(columnas, estilo);
                    for parte in partir(texto, ancho) {
                        renglones.push(Renglon { texto: alinear(&parte, ancho, estilo.centrado), estilo: *estilo });
                    }
                }
                Linea::Par { izquierda, derecha, estilo } => {
                    let ancho = ancho_para(columnas, estilo);
                    let espacio = ancho.saturating_sub(largo(izquierda) + largo(derecha));
                    if espacio >= 1 {
                        let texto = format!("{izquierda}{}{derecha}", " ".repeat(espacio));
                        renglones.push(Renglon { texto, estilo: *estilo });
                    } else {
                        for parte in partir(izquierda, ancho) {
                            renglones.push(Renglon { texto: parte, estilo: *estilo });
                        }
                        renglones.push(Renglon { texto: format!("{derecha:>ancho$}"), estilo: *estilo });
                    }
                }
                Linea::Separador => renglones.push(Renglon { texto: "-".repeat(columnas), estilo: Estilo::default() }),
                Linea::Espacio => renglones.push(Renglon { texto: String::new(), estilo: Estilo::default() }),
                Linea::Firma(leyenda) => {
                    renglones.push(Renglon { texto: String::new(), estilo: Estilo::default() });
                    renglones.push(Renglon { texto: String::new(), estilo: Estilo::default() });
                    let raya = "_".repeat(columnas * 3 / 4);
                    renglones.push(Renglon { texto: alinear(&raya, columnas, true), estilo: CENTRADO });
                    renglones.push(Renglon { texto: alinear(leyenda, columnas, true), estilo: CENTRADO });
                }
            }
        }
        renglones
    }
}

/// El ticket de una venta o de una cotización.
pub fn ticket(ticket: &Ticket) -> Documento {
    let Ticket { negocio, venta, cambio } = ticket;
    let mut doc = Documento::default();
    doc.encabezado(negocio);

    if venta.estado == Estado::Cotizacion {
        doc.con_estilo(format!("COTIZACIÓN {}", venta.folio_cotizacion.unwrap_or_default()), TITULO);
        doc.texto(venta.creada.format("%d/%m/%Y %H:%M").to_string());
        if let Some(vigencia) = venta.vigencia {
            doc.texto(format!("Válida hasta el {}", vigencia.format("%d/%m/%Y")));
        }
    } else {
        let fecha = venta.pagada_en.unwrap_or(venta.creada).format("%d/%m/%Y %H:%M").to_string();
        doc.par_con_estilo(format!("Folio {}", venta.folio.unwrap_or_default()), fecha, NEGRITA);
    }
    doc.texto(format!("Atendió: {}", venta.cajero));
    if let Some(cliente) = &venta.cliente {
        doc.texto(format!("Cliente: {cliente}"));
    }
    if venta.estado == Estado::Cancelada {
        doc.con_estilo("VENTA CANCELADA", TITULO);
    }
    doc.separador();

    for p in &venta.partidas {
        doc.texto(&p.descripcion);
        doc.par(format!("  {} {} x {}", p.cantidad, p.unidad, precio_por(p)), p.importe.pesos());
    }
    doc.separador();

    let t = &venta.totales;
    if t.descuento.es_positivo() {
        doc.par("Descuento", format!("-{}", t.descuento.pesos()));
    }
    doc.par("Subtotal", t.subtotal.pesos()).par("IVA", t.iva.pesos());
    doc.par_con_estilo("TOTAL", t.total.pesos(), TOTAL);
    for pago in &venta.pagos {
        doc.par(pago.metodo.etiqueta(), pago.recibido.pesos());
    }
    if cambio.es_positivo() {
        doc.par("Cambio", cambio.pesos());
    }
    doc.espacio();
    let pie = if venta.estado == Estado::Cotizacion {
        "Precios sujetos a existencia y a la vigencia de esta cotización."
    } else {
        negocio.pie_ticket.as_str()
    };
    if !pie.is_empty() {
        doc.con_estilo(pie, CENTRADO);
    }
    doc
}

/// El comprobante que se lleva el cliente al devolver mercancía (y que firma la copia de la tienda).
pub fn devolucion(comprobante: &ComprobanteDevolucion) -> Documento {
    let ComprobanteDevolucion { negocio, devolucion: d } = comprobante;
    let mut doc = Documento::default();
    doc.encabezado(negocio);
    doc.con_estilo("DEVOLUCIÓN", TITULO);
    doc.texto(format!("De la venta folio {}", d.folio_venta));
    doc.texto(d.creada.format("%d/%m/%Y %H:%M").to_string());
    doc.texto(format!("Atendió: {}", d.cajero)).texto(format!("Autorizó: {}", d.autorizo));
    if !d.motivo.is_empty() {
        doc.texto(format!("Motivo: {}", d.motivo));
    }
    doc.separador();
    for p in &d.partidas {
        doc.texto(&p.descripcion);
        doc.par(format!("  {} {} x {}", p.cantidad, p.unidad, p.precio.pesos()), p.importe.pesos());
    }
    doc.separador();
    doc.par_con_estilo("REEMBOLSO", d.reembolso.pesos(), TOTAL);
    doc.par("Forma", d.metodo.etiqueta());
    doc.lineas.push(Linea::Firma("Firma del cliente".into()));
    doc
}

/// El corte de caja, para guardarlo junto con el efectivo.
pub fn corte(negocio: &Negocio, corte: &Corte) -> Documento {
    let caja = &corte.caja;
    let mut doc = Documento::default();
    doc.encabezado(negocio);
    doc.con_estilo("CORTE DE CAJA", TITULO);
    doc.par("Abrió", format!("{} {}", caja.abrio, caja.abierta_en.format("%d/%m %H:%M")));
    match (&caja.cerro, caja.cerrada_en) {
        (Some(quien), Some(cuando)) => doc.par("Cerró", format!("{quien} {}", cuando.format("%d/%m %H:%M"))),
        _ => doc.con_estilo("(corte parcial: la caja sigue abierta)", CENTRADO),
    };
    doc.separador();
    doc.par("Fondo inicial", caja.fondo_inicial.pesos());
    doc.par(format!("Ventas ({})", corte.ventas), corte.total.pesos());
    for m in &corte.por_metodo {
        doc.par(format!("  {}", m.metodo.etiqueta()), m.monto.pesos());
    }
    for m in &corte.devoluciones {
        doc.par(format!("Devoluciones {}", m.metodo.etiqueta().to_lowercase()), format!("-{}", m.monto.pesos()));
    }
    if corte.por_cajero.len() > 1 {
        doc.separador().con_estilo("Por quién vendió", NEGRITA);
        for p in &corte.por_cajero {
            doc.par(format!("  {} ({})", p.cajero, p.ventas), p.total.pesos());
        }
    }
    doc.separador();
    doc.par_con_estilo("Efectivo esperado", corte.efectivo_esperado.pesos(), NEGRITA);
    if let Some(contado) = caja.efectivo_contado {
        doc.par("Efectivo contado", contado.pesos());
    }
    if let Some(diferencia) = corte.diferencia {
        let etiqueta = match diferencia.como_crudo() {
            0 => "Cuadra exacto",
            n if n > 0 => "Sobrante",
            _ => "Faltante",
        };
        let monto = nucleo::Dinero::crudo(diferencia.como_crudo().abs());
        doc.par_con_estilo(etiqueta, monto.pesos(), NEGRITA);
    }
    doc.lineas.push(Linea::Firma("Firma de quien entrega".into()));
    doc
}

/// «$48.00/kg» a granel; «$189.00» por pieza.
pub(crate) fn precio_por(partida: &nucleo::ventas::Partida) -> String {
    match partida.unidad.as_str() {
        "kg" => format!("{}/kg", partida.precio.pesos()),
        _ => partida.precio.pesos(),
    }
}

/// Para revisar que la impresora y el ancho del papel están bien configurados.
pub fn prueba(negocio: &Negocio, columnas: usize) -> Documento {
    let mut doc = Documento::default();
    doc.encabezado(negocio);
    doc.con_estilo("PRUEBA DE IMPRESIÓN", TITULO);
    doc.texto(format!("{columnas} letras por renglón:"));
    doc.texto("1234567890".repeat(columnas.div_ceil(10)).chars().take(columnas).collect::<String>());
    doc.texto("Acentos: á é í ó ú ñ Ñ ¿? ¡!");
    doc.par("Total", "$1,234.50");
    doc.par_con_estilo("TOTAL", "$99.00", TOTAL);
    doc.espacio();
    doc.con_estilo("Si todo se ve bien, la impresora está lista.", CENTRADO);
    doc
}

fn con_prefijo(prefijo: &str, dato: &str) -> String {
    if dato.is_empty() { String::new() } else { format!("{prefijo}{dato}") }
}

fn ancho_para(columnas: usize, estilo: &Estilo) -> usize {
    if estilo.grande { columnas / 2 } else { columnas }
}

fn largo(texto: &str) -> usize {
    texto.chars().count()
}

fn alinear(texto: &str, ancho: usize, centrado: bool) -> String {
    if !centrado {
        return texto.to_owned();
    }
    let sobra = ancho.saturating_sub(largo(texto));
    format!("{}{texto}", " ".repeat(sobra / 2))
}

/// Parte el texto en renglones de a lo más `ancho` caracteres, sin cortar palabras si se puede.
fn partir(texto: &str, ancho: usize) -> Vec<String> {
    let ancho = ancho.max(1);
    let mut renglones = Vec::new();
    let mut actual = String::new();
    for palabra in texto.split_whitespace() {
        let mut letras: Vec<char> = palabra.chars().collect();
        // Una palabra más larga que el renglón se corta a la fuerza.
        while letras.len() > ancho {
            if !actual.is_empty() {
                renglones.push(std::mem::take(&mut actual));
            }
            renglones.push(letras.drain(..ancho).collect());
        }
        if letras.is_empty() {
            continue;
        }
        if !actual.is_empty() && largo(&actual) + 1 + letras.len() > ancho {
            renglones.push(std::mem::take(&mut actual));
        }
        if !actual.is_empty() {
            actual.push(' ');
        }
        actual.extend(letras);
    }
    if !actual.is_empty() || renglones.is_empty() {
        renglones.push(actual);
    }
    renglones
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parte_sin_cortar_palabras() {
        assert_eq!(partir("Tornillo para madera ocho", 12), ["Tornillo", "para madera", "ocho"]);
        assert_eq!(partir("ABCDEFGHIJ", 4), ["ABCD", "EFGH", "IJ"]);
        assert_eq!(partir("", 10), [""]);
    }

    #[test]
    fn acomoda_pares_y_centrado() {
        let mut doc = Documento::default();
        doc.par("Total", "$10.00").con_estilo("Hola", CENTRADO);
        doc.par("Una etiqueta larguísima", "$1,000.00");
        let r = doc.renglones(16);
        assert_eq!(r[0].texto, "Total     $10.00");
        assert_eq!(r[1].texto, "      Hola");
        assert_eq!(r[2].texto, "Una etiqueta");
        assert_eq!(r[3].texto, "larguísima");
        assert_eq!(r[4].texto, "       $1,000.00");
    }
}
