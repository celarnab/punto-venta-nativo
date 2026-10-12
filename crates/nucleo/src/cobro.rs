//! Reglas de cobro, sin base de datos: fáciles de razonar y de probar.

use serde::{Deserialize, Serialize};

use crate::{Dinero, Error, Resultado};

enum_texto! {
    pub enum Metodo {
        Efectivo = "efectivo", "Efectivo";
        Tarjeta = "tarjeta", "Tarjeta";
        Transferencia = "transferencia", "Transferencia";
    }
}

/// Lo que el cliente entrega con un método de pago.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cobro {
    pub metodo: Metodo,
    pub recibido: Dinero,
}

/// Un cobro ya aplicado a la venta.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Aplicado {
    pub metodo: Metodo,
    pub recibido: Dinero,
    /// Lo que cuenta para la venta: lo recibido menos el cambio.
    pub monto: Dinero,
}

impl Aplicado {
    pub fn cambio(&self) -> Dinero {
        self.recibido - self.monto
    }
}

/// Aplica los cobros al total; el cambio solo puede salir del efectivo.
pub fn repartir(cobros: &[Cobro], total: Dinero) -> Resultado<Vec<Aplicado>> {
    if cobros.iter().any(|c| !c.recibido.es_positivo()) {
        return Err(Error::invalido("Los montos deben ser positivos."));
    }
    let suma = |metodo: Option<Metodo>| -> Dinero {
        cobros.iter().filter(|c| metodo.is_none_or(|m| c.metodo == m)).map(|c| c.recibido).sum()
    };

    let mut cambio = suma(None) - total;
    if cambio < Dinero::CERO {
        return Err(Error::invalido(format!("Faltan {} por cobrar.", (-cambio).pesos())));
    }
    if cambio > suma(Some(Metodo::Efectivo)) {
        return Err(Error::invalido("Solo se puede dar cambio sobre pagos en efectivo."));
    }

    Ok(cobros
        .iter()
        .map(|&Cobro { metodo, recibido }| {
            let devuelto = if metodo == Metodo::Efectivo { cambio.min(recibido) } else { Dinero::CERO };
            cambio -= devuelto;
            Aplicado { metodo, recibido, monto: recibido - devuelto }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::Metodo::*;
    use super::*;

    fn cobro(metodo: Metodo, recibido: &str) -> Cobro {
        Cobro { metodo, recibido: recibido.parse().unwrap() }
    }

    fn montos(aplicados: &[Aplicado]) -> Vec<String> {
        aplicados.iter().map(|a| a.monto.to_string()).collect()
    }

    fn total(texto: &str) -> Dinero {
        texto.parse().unwrap()
    }

    #[test]
    fn efectivo_con_cambio() {
        let aplicados = repartir(&[cobro(Efectivo, "200")], total("150")).unwrap();
        assert_eq!(montos(&aplicados), ["150.00"]);
        assert_eq!(aplicados[0].cambio(), total("50"));
    }

    #[test]
    fn mixto_da_el_cambio_del_efectivo() {
        let aplicados = repartir(&[cobro(Tarjeta, "100"), cobro(Efectivo, "100")], total("150")).unwrap();
        assert_eq!(montos(&aplicados), ["100.00", "50.00"]);
    }

    #[test]
    fn falta_dinero() {
        let error = repartir(&[cobro(Efectivo, "1000")], total("1234.5")).unwrap_err();
        assert_eq!(error.to_string(), "Faltan $234.50 por cobrar.");
    }

    #[test]
    fn no_hay_cambio_con_tarjeta() {
        assert!(repartir(&[cobro(Tarjeta, "200")], total("150")).is_err());
    }

    #[test]
    fn montos_positivos() {
        assert!(repartir(&[cobro(Efectivo, "200"), cobro(Tarjeta, "0")], total("150")).is_err());
    }
}
