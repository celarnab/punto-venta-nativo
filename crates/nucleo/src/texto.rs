use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// `"Plomería ÑANDÚ"` → `"plomeria nandu"`: sin acentos ni mayúsculas, para buscar.
pub fn normalizar(texto: &str) -> String {
    texto.nfkd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

/// Las palabras de una búsqueda, ya normalizadas.
pub fn palabras(consulta: &str) -> Vec<String> {
    normalizar(consulta).split_whitespace().map(str::to_owned).collect()
}

/// Las palabras como arreglo JSON, para filtrar en SQL con `json_each` sin armar la consulta a mano.
pub(crate) fn palabras_json(consulta: &str) -> String {
    serde_json::to_string(&palabras(consulta)).expect("una lista de textos siempre se serializa")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quita_acentos_y_mayusculas() {
        assert_eq!(normalizar("Plomería ÑANDÚ"), "plomeria nandu");
    }

    #[test]
    fn separa_palabras() {
        assert_eq!(palabras("  Tubo   PVC "), ["tubo", "pvc"]);
    }
}
