//! Bytes ESC/POS: el idioma de casi todas las impresoras térmicas de tickets (Epson, Xprinter, 3nStar…).

use crate::documento::{Documento, Estilo};

const ESC: u8 = 0x1B;
const GS: u8 = 0x1D;

/// Cómo está configurada la impresora de tickets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opciones {
    /// Caracteres por renglón: 42 o 48 en papel de 80 mm, 32 en 58 mm.
    pub columnas: usize,
    /// Abrir el cajón de dinero conectado a la impresora.
    pub abrir_cajon: bool,
    pub cortar: bool,
}

impl Default for Opciones {
    fn default() -> Self {
        Self { columnas: 42, abrir_cajon: false, cortar: true }
    }
}

pub fn codificar(documento: &Documento, opciones: Opciones) -> Vec<u8> {
    let mut bytes = vec![ESC, b'@', ESC, b't', 2]; // reiniciar; página de códigos PC850 (acentos y ñ)
    if opciones.abrir_cajon {
        bytes.extend([ESC, b'p', 0, 25, 250]);
    }
    let mut actual: Option<Estilo> = None;
    for renglon in documento.renglones(opciones.columnas) {
        if actual != Some(renglon.estilo) {
            bytes.extend([ESC, b'E', renglon.estilo.negrita.into()]);
            bytes.extend([GS, b'!', if renglon.estilo.grande { 0x11 } else { 0x00 }]);
            actual = Some(renglon.estilo);
        }
        bytes.extend(renglon.texto.chars().map(pc850));
        bytes.push(b'\n');
    }
    bytes.extend([ESC, b'E', 0, GS, b'!', 0, ESC, b'd', 4]); // estilo normal y avanzar para poder cortar
    if opciones.cortar {
        bytes.extend([GS, b'V', 66, 0]);
    }
    bytes
}

/// Un carácter en la página de códigos 850 (la que traen casi todas): letras con acento, ñ, ¿, ¡…
fn pc850(c: char) -> u8 {
    match c {
        ' '..='~' => c as u8,
        'á' => 0xA0,
        'é' => 0x82,
        'í' => 0xA1,
        'ó' => 0xA2,
        'ú' => 0xA3,
        'ñ' => 0xA4,
        'Ñ' => 0xA5,
        'ü' => 0x81,
        'Ü' => 0x9A,
        'Á' => 0xB5,
        'É' => 0x90,
        'Í' => 0xD6,
        'Ó' => 0xE0,
        'Ú' => 0xE9,
        '¿' => 0xA8,
        '¡' => 0xAD,
        '°' | 'º' => 0xF8,
        '½' => 0xAB,
        '¼' => 0xAC,
        '¾' => 0xF3,
        '·' => 0xFA,
        '×' => b'x',
        '–' | '—' | '−' => b'-',
        '“' | '”' | '«' | '»' => b'"',
        '‘' | '’' => b'\'',
        _ => b'?',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documento::Linea;

    #[test]
    fn acentos_y_comandos() {
        let documento = Documento { lineas: vec![Linea::Texto("Año ½\"".into(), Estilo::default())] };
        let bytes = codificar(&documento, Opciones { columnas: 32, abrir_cajon: true, cortar: true });
        assert!(bytes.starts_with(&[ESC, b'@', ESC, b't', 2, ESC, b'p', 0, 25, 250]));
        let texto = [b'A', 0xA4, b'o', b' ', 0xAB, b'"', b'\n'];
        assert!(bytes.windows(texto.len()).any(|w| w == texto));
        assert!(bytes.ends_with(&[GS, b'V', 66, 0]));
    }
}
