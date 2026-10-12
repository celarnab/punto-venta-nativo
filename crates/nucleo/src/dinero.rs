//! Números en punto fijo: exactos, sin sorpresas de `f64`, y sumables directo en SQLite.
//!
//! | Tipo          | Guarda            | Ejemplo              |
//! |---------------|-------------------|----------------------|
//! | [`Dinero`]    | centavos          | `$12.50` → `1250`    |
//! | [`Cantidad`]  | milésimas         | `1.5 m`  → `1500`    |
//! | [`Porcentaje`]| centésimas de %   | `16 %`   → `1600`    |

use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::Error;

macro_rules! punto_fijo {
    ($(#[$meta:meta])* $nombre:ident, decimales = $decimales:literal, recorta_ceros = $recorta:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $nombre(i64);

        impl $nombre {
            pub const DECIMALES: u32 = $decimales;
            pub const ESCALA: i64 = 10_i64.pow($decimales);
            pub const CERO: Self = Self(0);

            /// A partir del entero guardado (centavos, milésimas…).
            pub const fn crudo(valor: i64) -> Self {
                Self(valor)
            }

            pub const fn entero(unidades: i64) -> Self {
                Self(unidades * Self::ESCALA)
            }

            pub const fn como_crudo(self) -> i64 {
                self.0
            }

            pub const fn es_positivo(self) -> bool {
                self.0 > 0
            }

            /// Redondea un `f64` (p. ej. una celda de Excel) a los decimales del tipo.
            pub fn redondeando(valor: f64) -> Self {
                Self((valor * Self::ESCALA as f64).round() as i64)
            }

            pub fn como_f64(self) -> f64 {
                self.0 as f64 / Self::ESCALA as f64
            }
        }

        impl Add for $nombre {
            type Output = Self;
            fn add(self, otro: Self) -> Self {
                Self(self.0 + otro.0)
            }
        }

        impl Sub for $nombre {
            type Output = Self;
            fn sub(self, otro: Self) -> Self {
                Self(self.0 - otro.0)
            }
        }

        impl Neg for $nombre {
            type Output = Self;
            fn neg(self) -> Self {
                Self(-self.0)
            }
        }

        impl AddAssign for $nombre {
            fn add_assign(&mut self, otro: Self) {
                self.0 += otro.0;
            }
        }

        impl SubAssign for $nombre {
            fn sub_assign(&mut self, otro: Self) {
                self.0 -= otro.0;
            }
        }

        impl Sum for $nombre {
            fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
                Self(iter.map(|x| x.0).sum())
            }
        }

        impl FromStr for $nombre {
            type Err = Error;
            fn from_str(texto: &str) -> Result<Self, Error> {
                leer(texto, $decimales).map(Self)
            }
        }

        impl fmt::Display for $nombre {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let texto = escribir(self.0, $decimales);
                if $recorta && texto.contains('.') {
                    f.write_str(texto.trim_end_matches('0').trim_end_matches('.'))
                } else {
                    f.write_str(&texto)
                }
            }
        }

        /// Viaja a la interfaz como texto (`"12.50"`) para no perder exactitud en JavaScript.
        impl Serialize for $nombre {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        /// Acepta texto (`"12.5"`, `"$1,200"`) o un número.
        impl<'de> Deserialize<'de> for $nombre {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                deserializer.deserialize_any(Visitante::<$decimales>).map(Self)
            }
        }

        impl ToSql for $nombre {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(self.0.into())
            }
        }

        impl FromSql for $nombre {
            fn column_result(valor: ValueRef<'_>) -> FromSqlResult<Self> {
                valor.as_i64().map(Self)
            }
        }
    };
}

punto_fijo!(
    /// Pesos con centavos.
    Dinero, decimales = 2, recorta_ceros = false
);
punto_fijo!(
    /// Unidades vendidas o en existencia, con hasta tres decimales (1.5 m, 0.250 kg).
    Cantidad, decimales = 3, recorta_ceros = true
);
punto_fijo!(
    /// Un porcentaje con dos decimales, como la tasa de IVA.
    Porcentaje, decimales = 2, recorta_ceros = true
);

impl Cantidad {
    pub const UNO: Self = Self::entero(1);
}

impl Dinero {
    /// `$1,234.50`: para tickets y mensajes.
    pub fn pesos(self) -> String {
        let texto = escribir(self.0.abs(), Self::DECIMALES);
        let (enteros, centavos) = texto.split_once('.').expect("Dinero siempre lleva centavos");
        let signo = if self.0 < 0 { "-" } else { "" };
        format!("{signo}${}.{centavos}", agrupar_miles(enteros))
    }

    /// La parte de `self` que corresponde a `parte` de `todo`: p. ej. cuánto del descuento de una venta
    /// le toca a lo que se devuelve. `todo` > 0.
    pub fn proporcion(self, parte: Self, todo: Self) -> Self {
        Self(dividir(self.0 as i128 * parte.0 as i128, todo.0 as i128))
    }

    /// Precio con IVA incluido → la parte sin IVA.
    pub fn sin_iva(self, tasa: Porcentaje) -> Self {
        let cien = Porcentaje::entero(100).0 as i128;
        Self(dividir(self.0 as i128 * cien, cien + tasa.0 as i128))
    }
}

/// Importe de una partida: cantidad × precio unitario, redondeado al centavo.
impl Mul<Dinero> for Cantidad {
    type Output = Dinero;
    fn mul(self, precio: Dinero) -> Dinero {
        Dinero(dividir(self.0 as i128 * precio.0 as i128, Self::ESCALA as i128))
    }
}

/// Unidades de existencia que mueve una partida: lo vendido × unidades por unidad vendida
/// (p. ej. 0.5 kg × 120 piezas/kg = 60 piezas).
impl Mul for Cantidad {
    type Output = Cantidad;
    fn mul(self, factor: Cantidad) -> Cantidad {
        Cantidad(dividir(self.0 as i128 * factor.0 as i128, Self::ESCALA as i128))
    }
}

/// Divide redondeando la mitad lejos del cero, como se redondea un ticket. `divisor` > 0.
fn dividir(dividendo: i128, divisor: i128) -> i64 {
    let cociente = (dividendo.abs() + divisor / 2) / divisor;
    let cociente = if dividendo < 0 { -cociente } else { cociente };
    i64::try_from(cociente).expect("monto fuera de rango")
}

fn escribir(valor: i64, decimales: u32) -> String {
    let escala = 10_u64.pow(decimales);
    let signo = if valor < 0 { "-" } else { "" };
    let absoluto = valor.unsigned_abs();
    let ancho = decimales as usize;
    format!("{signo}{}.{:0ancho$}", absoluto / escala, absoluto % escala)
}

fn agrupar_miles(digitos: &str) -> String {
    let mut agrupado = String::with_capacity(digitos.len() + digitos.len() / 3);
    for (i, c) in digitos.chars().enumerate() {
        if i > 0 && (digitos.len() - i).is_multiple_of(3) {
            agrupado.push(',');
        }
        agrupado.push(c);
    }
    agrupado
}

/// `"-$1,234.5"` → `-123450` (con dos decimales). Rechaza más decimales de los que caben.
fn leer(texto: &str, decimales: u32) -> Result<i64, Error> {
    let invalido = || Error::invalido(format!("«{}» no es un número válido.", texto.trim()));
    let limpio: String = texto.chars().filter(|c| !matches!(c, '$' | ',' | '%') && !c.is_whitespace()).collect();
    let (negativo, digitos) = match limpio.strip_prefix('-') {
        Some(resto) => (true, resto),
        None => (false, limpio.as_str()),
    };
    let (enteros, fraccion) = digitos.split_once('.').unwrap_or((digitos, ""));
    let solo_digitos = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if enteros.len() + fraccion.len() == 0 || !solo_digitos(enteros) || !solo_digitos(fraccion) {
        return Err(invalido());
    }
    if fraccion.len() > decimales as usize {
        return Err(Error::invalido(format!("«{}» tiene demasiados decimales (máximo {decimales}).", texto.trim())));
    }

    let enteros: i64 = if enteros.is_empty() { 0 } else { enteros.parse().map_err(|_| invalido())? };
    let fraccion: i64 = format!("{fraccion:0<ancho$}", ancho = decimales as usize).parse().unwrap_or(0);
    let valor =
        enteros.checked_mul(10_i64.pow(decimales)).and_then(|v| v.checked_add(fraccion)).ok_or_else(invalido)?;
    Ok(if negativo { -valor } else { valor })
}

struct Visitante<const DECIMALES: u32>;

impl<const DECIMALES: u32> de::Visitor<'_> for Visitante<DECIMALES> {
    type Value = i64;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "un número con hasta {DECIMALES} decimales")
    }

    fn visit_str<E: de::Error>(self, texto: &str) -> Result<i64, E> {
        leer(texto, DECIMALES).map_err(E::custom)
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> Result<i64, E> {
        n.checked_mul(10_i64.pow(DECIMALES)).ok_or_else(|| E::custom("número fuera de rango"))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<i64, E> {
        i64::try_from(n).map_err(E::custom).and_then(|n| self.visit_i64(n))
    }

    fn visit_f64<E: de::Error>(self, n: f64) -> Result<i64, E> {
        self.visit_str(&n.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(texto: &str) -> Dinero {
        texto.parse().unwrap()
    }

    fn c(texto: &str) -> Cantidad {
        texto.parse().unwrap()
    }

    #[test]
    fn lee_formatos_comunes() {
        assert_eq!(d("12.5"), Dinero::crudo(1250));
        assert_eq!(d("$1,234.56"), Dinero::crudo(123_456));
        assert_eq!(d("-.5"), Dinero::crudo(-50));
        assert_eq!(d(" 7 "), Dinero::entero(7));
        assert_eq!(c("0.250"), Cantidad::crudo(250));
        assert_eq!("16".parse::<Porcentaje>().unwrap(), Porcentaje::crudo(1600));
    }

    #[test]
    fn rechaza_basura_y_decimales_de_mas() {
        for malo in ["", "-", "abc", "1.2.3", "1e5", "12.345"] {
            assert!(malo.parse::<Dinero>().is_err(), "{malo:?}");
        }
    }

    #[test]
    fn se_escribe() {
        assert_eq!(d("12.5").to_string(), "12.50");
        assert_eq!(d("-0.05").to_string(), "-0.05");
        assert_eq!(c("1.500").to_string(), "1.5");
        assert_eq!(c("3").to_string(), "3");
        assert_eq!(d("1234567.8").pesos(), "$1,234,567.80");
        assert_eq!(d("-999").pesos(), "-$999.00");
    }

    #[test]
    fn importe_redondea_al_centavo() {
        assert_eq!(c("1.5") * d("10.01"), d("15.02")); // 15.015 → 15.02
        assert_eq!(c("0.333") * d("1"), d("0.33"));
        assert_eq!(c("2") * d("-3.5"), d("-7"));
    }

    #[test]
    fn convierte_y_reparte() {
        assert_eq!(c("0.5") * c("120"), c("60"));
        assert_eq!(c("0.333") * c("3"), c("0.999"));
        assert_eq!(d("90").proporcion(d("50"), d("100")), d("45")); // la mitad del total con descuento
        assert_eq!(d("10").proporcion(d("1"), d("3")), d("3.33"));
    }

    #[test]
    fn desglosa_iva_incluido() {
        let tasa = Porcentaje::entero(16);
        assert_eq!(d("116").sin_iva(tasa), d("100"));
        assert_eq!(d("10").sin_iva(tasa), d("8.62"));
        assert_eq!(d("10").sin_iva(Porcentaje::CERO), d("10"));
    }

    #[test]
    fn viaja_como_texto() {
        assert_eq!(serde_json::to_string(&d("3.1")).unwrap(), r#""3.10""#);
        assert_eq!(serde_json::from_str::<Dinero>(r#""$1,000""#).unwrap(), d("1000"));
        assert_eq!(serde_json::from_str::<Cantidad>("2").unwrap(), c("2"));
        assert_eq!(serde_json::from_str::<Cantidad>("0.5").unwrap(), c("0.5"));
        assert!(serde_json::from_str::<Dinero>(r#""1.999""#).is_err());
    }
}
