//! Los negocios de esta computadora: cada uno con su carpeta (base de datos, respaldos y configuración de la
//! impresora) y cuál se abrió la última vez. La lista vive en `negocios.json`, en la carpeta de datos de la app.
//!
//! El primer negocio vive en la carpeta de datos misma, como cuando solo había uno: una instalación de antes
//! sigue abriendo el suyo sin mover nada. Los demás, en `negocios/negocio-N`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Resultado};

/// El archivo de la base de datos dentro de la carpeta de cada negocio.
pub const BASE_DE_DATOS: &str = "punto-venta.sqlite3";
const ARCHIVO: &str = "negocios.json";
const SUBCARPETA: &str = "negocios";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registro {
    pub id: String,
    /// Copia del nombre que tiene en su base, para listarlo sin abrirla. Vacío = aún no se configura.
    pub nombre: String,
    /// Relativa a la carpeta de datos; «.» es la carpeta de datos misma.
    carpeta: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Negocios {
    #[serde(skip)]
    raiz: PathBuf,
    actual: String,
    lista: Vec<Registro>,
}

impl Negocios {
    /// La lista de `raiz`. Si no hay (la primera vez, o una instalación de antes), un solo negocio: el de la raíz.
    pub fn cargar(raiz: impl Into<PathBuf>) -> Self {
        let raiz = raiz.into();
        let leida = fs::read_to_string(raiz.join(ARCHIVO))
            .ok()
            .and_then(|texto| serde_json::from_str::<Self>(&texto).ok())
            .filter(|negocios| !negocios.lista.is_empty());
        let mut negocios = leida.unwrap_or_else(|| Self {
            raiz: PathBuf::new(),
            actual: "principal".into(),
            lista: vec![Registro { id: "principal".into(), nombre: String::new(), carpeta: ".".into() }],
        });
        negocios.raiz = raiz;
        if !negocios.lista.iter().any(|r| r.id == negocios.actual) {
            negocios.actual = negocios.lista[0].id.clone();
        }
        negocios
    }

    pub fn guardar(&self) -> Resultado<()> {
        let texto = serde_json::to_string_pretty(self).expect("una lista de negocios siempre se serializa");
        fs::create_dir_all(&self.raiz)
            .and_then(|()| fs::write(self.raiz.join(ARCHIVO), texto))
            .map_err(|e| Error::invalido(format!("No se pudo guardar la lista de negocios: {e}")))
    }

    pub fn lista(&self) -> &[Registro] {
        &self.lista
    }

    pub fn actual(&self) -> &Registro {
        self.lista.iter().find(|r| r.id == self.actual).unwrap_or(&self.lista[0])
    }

    /// La carpeta del negocio, o `None` si no está en la lista.
    pub fn carpeta(&self, id: &str) -> Option<PathBuf> {
        self.lista.iter().find(|r| r.id == id).map(|r| self.raiz.join(&r.carpeta))
    }

    /// Lo marca como el que se abre la próxima vez.
    pub fn elegir(&mut self, id: &str) -> Resultado<()> {
        if !self.lista.iter().any(|r| r.id == id) {
            return Err(Error::NoEncontrado("Negocio"));
        }
        self.actual = id.into();
        Ok(())
    }

    pub fn renombrar(&mut self, id: &str, nombre: &str) {
        if let Some(registro) = self.lista.iter_mut().find(|r| r.id == id) {
            registro.nombre = nombre.trim().into();
        }
    }

    /// Un negocio nuevo, sin configurar, en una carpeta que aún no existe.
    pub fn agregar(&mut self) -> Registro {
        let libre = (1..)
            .map(|n| format!("negocio-{n}"))
            .find(|id| !self.lista.iter().any(|r| &r.id == id) && !self.raiz.join(SUBCARPETA).join(id).exists())
            .expect("siempre hay un número libre");
        let registro =
            Registro { id: libre.clone(), nombre: String::new(), carpeta: Path::new(SUBCARPETA).join(&libre) };
        self.lista.push(registro.clone());
        registro
    }

    /// Lo quita de la lista y borra su carpeta: para un negocio nuevo que al final no se configuró. Su base ya
    /// debe estar cerrada. Nunca toca la carpeta de datos misma ni algo fuera de `negocios/`.
    pub fn descartar(&mut self, id: &str) -> Resultado<()> {
        let Some(i) = self.lista.iter().position(|r| r.id == id) else {
            return Err(Error::NoEncontrado("Negocio"));
        };
        if self.lista.len() == 1 || !self.lista[i].carpeta.starts_with(SUBCARPETA) {
            return Err(Error::invalido("Este negocio no se puede descartar."));
        }
        let carpeta = self.raiz.join(&self.lista[i].carpeta);
        if carpeta.exists() {
            fs::remove_dir_all(&carpeta)
                .map_err(|e| Error::invalido(format!("No se pudo borrar {}: {e}", carpeta.display())))?;
        }
        self.lista.remove(i);
        if self.actual == id {
            self.actual = self.lista[0].id.clone();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_lista_el_unico_es_el_de_la_carpeta_de_datos() {
        let dir = tempfile::tempdir().unwrap();
        let negocios = Negocios::cargar(dir.path());
        assert_eq!(negocios.lista().len(), 1);
        assert_eq!(negocios.carpeta(&negocios.actual().id).unwrap(), dir.path().join("."));
    }

    #[test]
    fn agregar_elegir_y_volver_a_cargar() {
        let dir = tempfile::tempdir().unwrap();
        let mut negocios = Negocios::cargar(dir.path());
        negocios.renombrar("principal", " Ferretería Ferremax ");
        let farmacia = negocios.agregar();
        assert_eq!(negocios.carpeta(&farmacia.id).unwrap(), dir.path().join("negocios").join("negocio-1"));
        negocios.renombrar(&farmacia.id, "Farmacia Vida Sana");
        negocios.elegir(&farmacia.id).unwrap();
        assert!(negocios.elegir("otro").is_err());
        negocios.guardar().unwrap();

        let leidos = Negocios::cargar(dir.path());
        let nombres: Vec<_> = leidos.lista().iter().map(|r| r.nombre.as_str()).collect();
        assert_eq!(nombres, ["Ferretería Ferremax", "Farmacia Vida Sana"]);
        assert_eq!(leidos.actual().id, "negocio-1");
    }

    #[test]
    fn no_reusa_una_carpeta_que_ya_existe() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("negocios").join("negocio-1")).unwrap();
        let mut negocios = Negocios::cargar(dir.path());
        assert_eq!(negocios.agregar().id, "negocio-2");
        assert_eq!(negocios.agregar().id, "negocio-3");
    }

    #[test]
    fn descartar_borra_solo_su_carpeta() {
        let dir = tempfile::tempdir().unwrap();
        let mut negocios = Negocios::cargar(dir.path());
        assert!(negocios.descartar("principal").is_err()); // el único, y vive en la raíz

        let nuevo = negocios.agregar();
        let carpeta = negocios.carpeta(&nuevo.id).unwrap();
        fs::create_dir_all(&carpeta).unwrap();
        fs::write(carpeta.join(BASE_DE_DATOS), "").unwrap();
        negocios.elegir(&nuevo.id).unwrap();
        negocios.descartar(&nuevo.id).unwrap();
        assert!(!carpeta.exists());
        assert!(dir.path().exists());
        assert_eq!((negocios.lista().len(), negocios.actual().id.as_str()), (1, "principal"));
        assert!(negocios.descartar("principal").is_err());
    }

    #[test]
    fn una_lista_danada_o_con_actual_inexistente_no_impide_arrancar() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(ARCHIVO), "{ esto no es json").unwrap();
        assert_eq!(Negocios::cargar(dir.path()).actual().id, "principal");

        let json = r#"{ "actual": "borrado", "lista": [ { "id": "a", "nombre": "A", "carpeta": "." } ] }"#;
        fs::write(dir.path().join(ARCHIVO), json).unwrap();
        assert_eq!(Negocios::cargar(dir.path()).actual().id, "a");
    }
}
