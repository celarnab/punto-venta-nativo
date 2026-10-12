use std::path::Path;

use rusqlite::backup::Progress;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, MAIN_DB, OpenFlags, Transaction};
use rusqlite_migration::{M, Migrations};

use crate::texto::normalizar;
use crate::{Error, Resultado};

/// El esquema crece solo con migraciones nuevas: una vez que una base la aplicó, una migración ya no se toca.
const PASOS: [&str; 1] = [include_str!("../migraciones/001_inicial.sql")];

/// Tablas que toda base de esta app tiene; sirven para reconocer un archivo antes de restaurarlo.
const TABLAS: [&str; 6] = ["negocio", "usuarios", "productos", "ventas", "partidas", "cajas"];

fn migraciones() -> Migrations<'static> {
    // Al terminar cada paso se revisa que ninguna llave foránea haya quedado rota.
    Migrations::new(PASOS.iter().map(|sql| M::up(sql).foreign_key_check()).collect())
}

/// La base de datos del negocio: un solo archivo SQLite.
pub struct Bd {
    conn: Connection,
}

impl Bd {
    /// Abre (o crea) la base y la pone al día con el esquema actual.
    pub fn abrir(ruta: impl AsRef<Path>) -> Resultado<Self> {
        Self::preparar(Connection::open(ruta)?)
    }

    pub fn en_memoria() -> Resultado<Self> {
        Self::preparar(Connection::open_in_memory()?)
    }

    fn preparar(mut conn: Connection) -> Resultado<Self> {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
        conn.create_scalar_function(
            "normalizar",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| Ok(normalizar(&ctx.get::<String>(0)?)),
        )?;
        // Con las llaves foráneas apagadas: reconstruir una tabla no debe borrar en cascada a sus vecinas.
        conn.pragma_update(None, "foreign_keys", false)?;
        migraciones().to_latest(&mut conn)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        Ok(Self { conn })
    }

    /// Copia consistente de la base en `destino`, aun con la app en uso.
    pub fn respaldar(&self, destino: impl AsRef<Path>) -> Resultado<()> {
        let destino = destino.as_ref();
        if destino.exists() {
            std::fs::remove_file(destino)
                .map_err(|e| Error::invalido(format!("No se pudo reemplazar el respaldo: {e}")))?;
        }
        self.conn.execute("VACUUM INTO ?1", [destino.to_string_lossy()])?;
        Ok(())
    }

    /// Reemplaza todos los datos con los de `origen`: un respaldo o la base de otra computadora.
    /// Antes revisa que el archivo sea una base sana de esta app y guarda lo actual en `respaldo_previo`.
    pub fn restaurar(&mut self, origen: impl AsRef<Path>, respaldo_previo: impl AsRef<Path>) -> Resultado<()> {
        let origen = origen.as_ref();
        revisar_respaldo(origen)?;
        self.respaldar(respaldo_previo)?;
        self.conn.restore(MAIN_DB, origen, None::<fn(Progress)>)?;
        // Si viene de una versión anterior de la app, se pone al día como cualquier base.
        self.conn.pragma_update(None, "foreign_keys", false)?;
        migraciones().to_latest(&mut self.conn)?;
        self.conn.pragma_update(None, "foreign_keys", true)?;
        Ok(())
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Ejecuta `f` en una transacción: o se aplica todo, o nada.
    pub(crate) fn transaccion<T>(&mut self, f: impl FnOnce(&Transaction) -> Resultado<T>) -> Resultado<T> {
        let tx = self.conn.transaction()?;
        let resultado = f(&tx)?;
        tx.commit()?;
        Ok(resultado)
    }
}

/// ¿Es `ruta` una base de datos sana de esta app, de esta versión o de una anterior?
fn revisar_respaldo(ruta: &Path) -> Resultado<()> {
    let no_es = || Error::invalido("El archivo no es una base de datos de este punto de venta.");
    let conn = Connection::open_with_flags(ruta, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| no_es())?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |f| f.get(0)).map_err(|_| no_es())?;
    if version == 0 {
        return Err(no_es());
    }
    if version > PASOS.len() as i64 {
        return Err(Error::invalido("El respaldo es de una versión más nueva de la app: actualízala primero."));
    }
    for tabla in TABLAS {
        let existe: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [tabla],
            |f| f.get(0),
        )?;
        if !existe {
            return Err(no_es());
        }
    }
    let integridad: String = conn.query_row("PRAGMA integrity_check", [], |f| f.get(0))?;
    if integridad != "ok" {
        return Err(Error::invalido(format!("El respaldo está dañado: {integridad}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn las_migraciones_son_validas() {
        migraciones().validate().unwrap();
    }

    #[test]
    fn restaura_un_respaldo_y_guarda_lo_que_habia() {
        let dir = tempfile::tempdir().unwrap();
        let mut otra = Bd::abrir(dir.path().join("otra.sqlite3")).unwrap();
        let mut negocio = otra.negocio().unwrap();
        negocio.nombre = "Farmacia del Centro".into();
        otra.guardar_negocio(&negocio).unwrap();
        otra.respaldar(dir.path().join("respaldo.sqlite3")).unwrap();

        let mut bd = Bd::abrir(dir.path().join("pos.sqlite3")).unwrap();
        bd.restaurar(dir.path().join("respaldo.sqlite3"), dir.path().join("antes.sqlite3")).unwrap();
        assert_eq!(bd.negocio().unwrap().nombre, "Farmacia del Centro");
        assert_eq!(Bd::abrir(dir.path().join("antes.sqlite3")).unwrap().negocio().unwrap().nombre, "Mi negocio");
    }

    #[test]
    fn no_restaura_cualquier_archivo() {
        let dir = tempfile::tempdir().unwrap();
        let mut bd = Bd::abrir(dir.path().join("pos.sqlite3")).unwrap();
        let antes = dir.path().join("antes.sqlite3");

        let texto = dir.path().join("notas.txt");
        std::fs::write(&texto, "hola").unwrap();
        assert!(bd.restaurar(&texto, &antes).is_err());

        let ajena = dir.path().join("ajena.sqlite3");
        Connection::open(&ajena).unwrap().execute_batch("CREATE TABLE x (y); PRAGMA user_version = 1;").unwrap();
        let error = bd.restaurar(&ajena, &antes).unwrap_err();
        assert_eq!(error.to_string(), "El archivo no es una base de datos de este punto de venta.");

        let futura = dir.path().join("futura.sqlite3");
        Bd::abrir(&futura).unwrap().conn().pragma_update(None, "user_version", 99).unwrap();
        assert!(bd.restaurar(&futura, &antes).unwrap_err().to_string().contains("más nueva"));
        assert!(!antes.exists()); // nada se tocó
    }

    #[test]
    fn respalda_en_un_archivo_que_se_puede_abrir() {
        let dir = tempfile::tempdir().unwrap();
        let bd = Bd::abrir(dir.path().join("pos.sqlite3")).unwrap();
        let copia = dir.path().join("respaldo.sqlite3");
        bd.respaldar(&copia).unwrap();
        bd.respaldar(&copia).unwrap(); // reemplaza el anterior
        Bd::abrir(&copia).unwrap();
    }
}
