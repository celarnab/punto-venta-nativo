//! Lo que la app recuerda entre un clic y otro.

use std::collections::BTreeMap;
use std::path::PathBuf;

use nucleo::catalogo::Producto;
use nucleo::reportes::Filtro;
use nucleo::usuarios::Usuario;
use nucleo::{Bd, Cantidad, Error, Resultado};

use crate::configuracion::Configuracion;

/// Un producto esperando a que el cajero diga cuánto pesa o qué precio lleva.
pub struct Pendiente {
    pub producto: Producto,
    pub cantidad: Option<Cantidad>,
    /// Si es para cambiar el precio de una partida que ya está en la venta.
    pub partida: Option<i64>,
}

/// Un precio adicional mientras se edita, tal como se escribe.
#[derive(Clone, Default)]
pub struct OtroPrecioTexto {
    pub nombre: String,
    pub precio: String,
    pub desde: String,
}

pub struct Estado {
    pub bd: Bd,
    pub carpeta: PathBuf,
    pub configuracion: Configuracion,
    pub sesion: Option<Usuario>,

    // Vender
    pub venta_id: Option<i64>,
    pub resultados: Vec<Producto>,
    pub resaltado: usize,
    pub pendiente: Option<Pendiente>,
    pub cobrada: Option<i64>,

    // Historial, cotizaciones y devoluciones
    pub detalle: Option<i64>,
    pub devolucion: BTreeMap<i64, Cantidad>,
    pub filtro_cotizaciones: String,

    // Productos y clientes
    pub busqueda_productos: (String, bool, bool),
    pub editor_precios: Vec<OtroPrecioTexto>,
    pub importacion: Option<PathBuf>,
    pub busqueda_clientes: String,

    // Reportes
    pub reporte: Option<Filtro>,
    pub cajeros: Vec<i64>,
}

impl Estado {
    pub fn new(bd: Bd, carpeta: PathBuf) -> Self {
        Self {
            bd,
            configuracion: Configuracion::cargar(&carpeta),
            carpeta,
            sesion: None,
            venta_id: None,
            resultados: Vec::new(),
            resaltado: 0,
            pendiente: None,
            cobrada: None,
            detalle: None,
            devolucion: BTreeMap::new(),
            filtro_cotizaciones: String::new(),
            busqueda_productos: Default::default(),
            editor_precios: Vec::new(),
            importacion: None,
            busqueda_clientes: String::new(),
            reporte: None,
            cajeros: Vec::new(),
        }
    }

    pub fn usuario(&self) -> Resultado<&Usuario> {
        self.sesion.as_ref().ok_or(Error::SinSesion)
    }

    pub fn admin(&self) -> Resultado<&Usuario> {
        Some(self.usuario()?).filter(|u| u.es_admin()).ok_or(Error::SoloAdministrador)
    }
}
