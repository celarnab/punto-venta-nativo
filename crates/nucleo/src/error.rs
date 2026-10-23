use serde::ser::{Serialize, SerializeStruct, Serializer};

pub type Resultado<T> = Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Un dato mal capturado o una regla de negocio que no se cumple; el mensaje es para el usuario.
    #[error("{0}")]
    Invalido(String),

    #[error("{0} no encontrado.")]
    NoEncontrado(&'static str),

    /// Hay partidas por debajo del costo y nadie autorizó venderlas así.
    #[error("Por debajo del costo: {}. Se necesita autorización de un administrador.", .0.join(", "))]
    BajoCosto(Vec<String>),

    #[error("PIN incorrecto.")]
    PinIncorrecto,

    #[error("Inicia sesión para continuar.")]
    SinSesion,

    #[error("Solo un administrador puede hacer esto.")]
    SoloAdministrador,

    #[error("No tienes permiso para hacer esto. Pídeselo a un administrador.")]
    SinPermiso,

    #[error("Base de datos: {0}")]
    Bd(#[from] rusqlite::Error),

    #[error("No se pudo actualizar la base de datos: {0}")]
    Migracion(#[from] rusqlite_migration::Error),

    #[error("Excel: {0}")]
    Excel(String),
}

impl Error {
    pub fn invalido(mensaje: impl Into<String>) -> Self {
        Self::Invalido(mensaje.into())
    }

    /// Para que la interfaz distinga los casos que requieren una acción (p. ej. pedir un PIN).
    pub const fn tipo(&self) -> &'static str {
        match self {
            Self::Invalido(_) => "invalido",
            Self::NoEncontrado(_) => "no_encontrado",
            Self::BajoCosto(_) => "bajo_costo",
            Self::PinIncorrecto => "pin_incorrecto",
            Self::SinSesion => "sin_sesion",
            Self::SoloAdministrador => "solo_administrador",
            Self::SinPermiso => "sin_permiso",
            Self::Bd(_) | Self::Migracion(_) | Self::Excel(_) => "interno",
        }
    }
}

/// Viaja a la interfaz como `{ "tipo": "bajo_costo", "mensaje": "…" }`.
impl Serialize for Error {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut error = serializer.serialize_struct("Error", 2)?;
        error.serialize_field("tipo", self.tipo())?;
        error.serialize_field("mensaje", &self.to_string())?;
        error.end()
    }
}

impl From<rust_xlsxwriter::XlsxError> for Error {
    fn from(error: rust_xlsxwriter::XlsxError) -> Self {
        Self::Excel(error.to_string())
    }
}

/// Traduce la violación de un `UNIQUE` a un mensaje legible; cualquier otro error pasa tal cual.
pub(crate) fn duplicado(mensaje: impl FnOnce() -> String) -> impl FnOnce(rusqlite::Error) -> Error {
    move |error| match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::ConstraintViolation) if error.to_string().contains("UNIQUE") => {
            Error::Invalido(mensaje())
        }
        _ => Error::Bd(error),
    }
}
