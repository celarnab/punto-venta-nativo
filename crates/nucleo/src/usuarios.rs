//! Empleados que entran con PIN, y las autorizaciones de un administrador.

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::error::duplicado;
use crate::{Bd, Error, Resultado};

enum_texto! {
    pub enum Rol {
        Cajero = "cajero", "Cajero";
        Administrador = "administrador", "Administrador";
    }
}

enum_texto! {
    /// Lo que un administrador le puede dar a un cajero además de vender.
    pub enum Permiso {
        VerReportes = "ver_reportes", "Ver sus reportes";
        ManejarProductos = "manejar_productos", "Manejar productos";
        ManejarInventario = "manejar_inventario", "Manejar inventario";
    }
}

/// Permisos extra de un cajero. Los de un administrador siempre van en `false` (ya puede todo).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permisos {
    pub ver_reportes: bool,
    pub manejar_productos: bool,
    pub manejar_inventario: bool,
}

impl Permisos {
    pub const fn tiene(self, permiso: Permiso) -> bool {
        match permiso {
            Permiso::VerReportes => self.ver_reportes,
            Permiso::ManejarProductos => self.manejar_productos,
            Permiso::ManejarInventario => self.manejar_inventario,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Usuario {
    pub id: i64,
    pub nombre: String,
    pub rol: Rol,
    pub activo: bool,
    pub permisos: Permisos,
}

impl Usuario {
    pub fn es_admin(&self) -> bool {
        self.rol == Rol::Administrador
    }

    /// Un administrador puede todo; un cajero, lo que le hayan dado.
    pub fn puede(&self, permiso: Permiso) -> bool {
        self.es_admin() || self.permisos.tiene(permiso)
    }

    fn de_fila(fila: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: fila.get("id")?,
            nombre: fila.get("nombre")?,
            rol: fila.get("rol")?,
            activo: fila.get("activo")?,
            permisos: Permisos {
                ver_reportes: fila.get("ver_reportes")?,
                manejar_productos: fila.get("manejar_productos")?,
                manejar_inventario: fila.get("manejar_inventario")?,
            },
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct DatosUsuario {
    pub nombre: String,
    pub rol: Rol,
    pub activo: bool,
    #[serde(default)]
    pub permisos: Permisos,
}

impl DatosUsuario {
    /// Lo que se guarda: un administrador no lleva permisos extra, para que no reaparezcan si deja de serlo.
    fn permisos_a_guardar(&self, rol: Rol) -> Permisos {
        if rol == Rol::Administrador { Permisos::default() } else { self.permisos }
    }
}

/// Prueba de que un administrador puso su PIN; solo [`Bd::autorizar`] la puede crear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Autorizacion {
    pub(crate) admin_id: i64,
}

impl Bd {
    /// Si no hay nadie, la app arranca pidiendo crear al primer administrador.
    pub fn hay_usuarios(&self) -> Resultado<bool> {
        Ok(self.conn().query_row("SELECT EXISTS (SELECT 1 FROM usuarios)", [], |f| f.get(0))?)
    }

    pub fn usuarios(&self) -> Resultado<Vec<Usuario>> {
        let mut consulta = self.conn().prepare("SELECT * FROM usuarios ORDER BY NOT activo, nombre")?;
        let usuarios = consulta.query_map([], Usuario::de_fila)?.collect::<rusqlite::Result<_>>()?;
        Ok(usuarios)
    }

    pub fn usuario(&self, id: i64) -> Resultado<Usuario> {
        buscar(self.conn(), id)
    }

    /// El primer usuario siempre es administrador, para que alguien pueda configurar todo.
    pub fn crear_usuario(&mut self, datos: &DatosUsuario, pin: &str) -> Resultado<Usuario> {
        let nombre = validar_nombre(&datos.nombre)?;
        let rol = if self.hay_usuarios()? { datos.rol } else { Rol::Administrador };
        let permisos = datos.permisos_a_guardar(rol);
        let hash = cifrar(pin)?;
        self.conn()
            .execute(
                "INSERT INTO usuarios (nombre, rol, pin_hash, activo, ver_reportes, manejar_productos, manejar_inventario)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    nombre,
                    rol,
                    hash,
                    datos.activo,
                    permisos.ver_reportes,
                    permisos.manejar_productos,
                    permisos.manejar_inventario
                ],
            )
            .map_err(duplicado(|| format!("Ya existe un usuario llamado {nombre}.")))?;
        self.usuario(self.conn().last_insert_rowid())
    }

    pub fn actualizar_usuario(&mut self, id: i64, datos: &DatosUsuario) -> Resultado<Usuario> {
        let nombre = validar_nombre(&datos.nombre)?;
        let permisos = datos.permisos_a_guardar(datos.rol);
        self.transaccion(|tx| {
            tx.execute(
                "UPDATE usuarios SET nombre = ?2, rol = ?3, activo = ?4,
                    ver_reportes = ?5, manejar_productos = ?6, manejar_inventario = ?7
                 WHERE id = ?1",
                params![
                    id,
                    nombre,
                    datos.rol,
                    datos.activo,
                    permisos.ver_reportes,
                    permisos.manejar_productos,
                    permisos.manejar_inventario
                ],
            )
            .map_err(duplicado(|| format!("Ya existe un usuario llamado {nombre}.")))?;
            let admins: i64 =
                tx.query_row("SELECT count(*) FROM usuarios WHERE rol = 'administrador' AND activo", [], |f| f.get(0))?;
            if admins == 0 {
                return Err(Error::invalido("Debe quedar al menos un administrador activo."));
            }
            buscar(tx, id)
        })
    }

    pub fn cambiar_pin(&mut self, id: i64, pin: &str) -> Resultado<()> {
        let hash = cifrar(pin)?;
        match self.conn().execute("UPDATE usuarios SET pin_hash = ?2 WHERE id = ?1", params![id, hash])? {
            0 => Err(Error::NoEncontrado("Usuario")),
            _ => Ok(()),
        }
    }

    /// Inicio de sesión: el empleado elige su nombre y teclea su PIN.
    pub fn entrar(&self, id: i64, pin: &str) -> Resultado<Usuario> {
        let hash: Option<String> = self
            .conn()
            .query_row("SELECT pin_hash FROM usuarios WHERE id = ?1 AND activo", [id], |f| f.get(0))
            .optional()?;
        match hash {
            Some(hash) if coincide(pin, &hash) => self.usuario(id),
            _ => Err(Error::PinIncorrecto),
        }
    }

    /// Un administrador cualquiera teclea su PIN para permitir algo excepcional.
    pub fn autorizar(&self, pin: &str) -> Resultado<Autorizacion> {
        let mut consulta =
            self.conn().prepare("SELECT id, pin_hash FROM usuarios WHERE rol = 'administrador' AND activo")?;
        let admins = consulta.query_map([], |f| Ok((f.get::<_, i64>(0)?, f.get::<_, String>(1)?)))?;
        for admin in admins {
            let (admin_id, hash) = admin?;
            if coincide(pin, &hash) {
                return Ok(Autorizacion { admin_id });
            }
        }
        Err(Error::PinIncorrecto)
    }
}

fn buscar(conn: &Connection, id: i64) -> Resultado<Usuario> {
    conn.query_row("SELECT * FROM usuarios WHERE id = ?1", [id], Usuario::de_fila)
        .optional()?
        .ok_or(Error::NoEncontrado("Usuario"))
}

fn validar_nombre(nombre: &str) -> Resultado<&str> {
    match nombre.trim() {
        "" => Err(Error::invalido("Falta el nombre.")),
        nombre => Ok(nombre),
    }
}

fn cifrar(pin: &str) -> Resultado<String> {
    if !(4..=8).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::invalido("El PIN debe tener de 4 a 8 dígitos."));
    }
    let hash = Argon2::default()
        .hash_password(pin.as_bytes())
        .map_err(|e| Error::invalido(format!("No se pudo guardar el PIN: {e}")))?;
    Ok(hash.to_string())
}

fn coincide(pin: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|hash| Argon2::default().verify_password(pin.as_bytes(), &hash).is_ok())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn datos(nombre: &str, rol: Rol) -> DatosUsuario {
        DatosUsuario { nombre: nombre.into(), rol, activo: true, permisos: Permisos::default() }
    }

    #[test]
    fn el_primero_es_administrador() {
        let mut bd = Bd::en_memoria().unwrap();
        assert!(!bd.hay_usuarios().unwrap());
        let ana = bd.crear_usuario(&datos("Ana", Rol::Cajero), "1234").unwrap();
        assert!(ana.es_admin());
        let beto = bd.crear_usuario(&datos("Beto", Rol::Cajero), "5678").unwrap();
        assert!(!beto.es_admin());
    }

    #[test]
    fn entra_con_su_pin() {
        let mut bd = Bd::en_memoria().unwrap();
        let ana = bd.crear_usuario(&datos("Ana", Rol::Administrador), "1234").unwrap();
        assert_eq!(bd.entrar(ana.id, "1234").unwrap(), ana);
        assert!(matches!(bd.entrar(ana.id, "9999"), Err(Error::PinIncorrecto)));

        bd.cambiar_pin(ana.id, "4321").unwrap();
        assert!(bd.entrar(ana.id, "4321").is_ok());
    }

    #[test]
    fn solo_un_admin_autoriza() {
        let mut bd = Bd::en_memoria().unwrap();
        let ana = bd.crear_usuario(&datos("Ana", Rol::Administrador), "1234").unwrap();
        bd.crear_usuario(&datos("Beto", Rol::Cajero), "5678").unwrap();
        assert_eq!(bd.autorizar("1234").unwrap().admin_id, ana.id);
        assert!(bd.autorizar("5678").is_err());
    }

    #[test]
    fn valida_pin_y_nombre() {
        let mut bd = Bd::en_memoria().unwrap();
        assert!(bd.crear_usuario(&datos("Ana", Rol::Cajero), "12").is_err());
        assert!(bd.crear_usuario(&datos("Ana", Rol::Cajero), "12ab").is_err());
        assert!(bd.crear_usuario(&datos("  ", Rol::Cajero), "1234").is_err());
        bd.crear_usuario(&datos("Ana", Rol::Cajero), "1234").unwrap();
        let error = bd.crear_usuario(&datos("ana", Rol::Cajero), "1234").unwrap_err();
        assert_eq!(error.to_string(), "Ya existe un usuario llamado ana.");
    }

    #[test]
    fn siempre_queda_un_admin() {
        let mut bd = Bd::en_memoria().unwrap();
        let ana = bd.crear_usuario(&datos("Ana", Rol::Administrador), "1234").unwrap();
        assert!(bd.actualizar_usuario(ana.id, &datos("Ana", Rol::Cajero)).is_err());
        assert!(bd.usuario(ana.id).unwrap().es_admin()); // la transacción se deshizo
    }

    #[test]
    fn un_cajero_empieza_sin_permisos_extra() {
        let mut bd = Bd::en_memoria().unwrap();
        bd.crear_usuario(&datos("Ana", Rol::Administrador), "1234").unwrap();
        let beto = bd.crear_usuario(&datos("Beto", Rol::Cajero), "5678").unwrap();
        assert_eq!(beto.permisos, Permisos::default());
        assert!(Permiso::TODOS.iter().all(|&p| !beto.puede(p)));
    }

    #[test]
    fn el_administrador_puede_todo() {
        let mut bd = Bd::en_memoria().unwrap();
        let ana = bd.crear_usuario(&datos("Ana", Rol::Administrador), "1234").unwrap();
        assert!(Permiso::TODOS.iter().all(|&p| ana.puede(p)));
    }

    #[test]
    fn se_guardan_los_permisos_de_un_cajero() {
        let mut bd = Bd::en_memoria().unwrap();
        bd.crear_usuario(&datos("Ana", Rol::Administrador), "1234").unwrap();
        let reportes = Permisos { ver_reportes: true, ..Default::default() };
        let beto =
            bd.crear_usuario(&DatosUsuario { permisos: reportes, ..datos("Beto", Rol::Cajero) }, "5678").unwrap();
        assert!(beto.puede(Permiso::VerReportes));
        assert!(!beto.puede(Permiso::ManejarProductos));
        assert_eq!(bd.entrar(beto.id, "5678").unwrap().permisos, reportes);

        let inventario = Permisos { manejar_inventario: true, ..Default::default() };
        let beto = bd.actualizar_usuario(beto.id, &DatosUsuario { permisos: inventario, ..datos("Beto", Rol::Cajero) });
        let beto = beto.unwrap();
        assert!(!beto.puede(Permiso::VerReportes));
        assert!(beto.puede(Permiso::ManejarInventario));
    }

    #[test]
    fn al_volverse_administrador_se_limpian_los_permisos() {
        let mut bd = Bd::en_memoria().unwrap();
        bd.crear_usuario(&datos("Ana", Rol::Administrador), "1234").unwrap();
        let todos = Permisos { ver_reportes: true, manejar_productos: true, manejar_inventario: true };
        let beto = bd.crear_usuario(&DatosUsuario { permisos: todos, ..datos("Beto", Rol::Cajero) }, "5678").unwrap();

        let beto =
            bd.actualizar_usuario(beto.id, &DatosUsuario { permisos: todos, ..datos("Beto", Rol::Administrador) });
        assert_eq!(beto.unwrap().permisos, Permisos::default());
        // Si deja de ser administrador, no recupera solo lo que tenía.
        let beto = bd.usuarios().unwrap().into_iter().find(|u| u.nombre == "Beto").unwrap();
        let beto = bd.actualizar_usuario(beto.id, &datos("Beto", Rol::Cajero)).unwrap();
        assert!(Permiso::TODOS.iter().all(|&p| !beto.puede(p)));
    }
}
