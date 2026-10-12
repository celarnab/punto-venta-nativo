/// Un `enum` que se guarda en SQLite y viaja a la interfaz como texto (`"efectivo"`),
/// y que sabe mostrarse en español (`"Efectivo"`).
macro_rules! enum_texto {
    (
        $(#[$meta:meta])*
        pub enum $nombre:ident {
            $($variante:ident = $clave:literal, $etiqueta:literal;)+
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub enum $nombre {
            $(#[serde(rename = $clave)] $variante,)+
        }

        impl $nombre {
            pub const TODOS: &[Self] = &[$(Self::$variante),+];

            pub const fn clave(self) -> &'static str {
                match self { $(Self::$variante => $clave,)+ }
            }

            pub const fn etiqueta(self) -> &'static str {
                match self { $(Self::$variante => $etiqueta,)+ }
            }
        }

        impl std::fmt::Display for $nombre {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.etiqueta())
            }
        }

        impl rusqlite::types::ToSql for $nombre {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.clave().into())
            }
        }

        impl rusqlite::types::FromSql for $nombre {
            fn column_result(valor: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                match valor.as_str()? {
                    $($clave => Ok(Self::$variante),)+
                    otro => Err(rusqlite::types::FromSqlError::Other(
                        format!("{} desconocido: {otro}", stringify!($nombre)).into(),
                    )),
                }
            }
        }
    };
}
