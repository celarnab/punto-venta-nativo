-- Código de barras, aparte del código del producto: el escáner y la búsqueda encuentran el producto por
-- cualquiera de los dos. Es opcional; la app cuida que no choque con el código de otro producto.
ALTER TABLE productos ADD COLUMN codigo_barras TEXT COLLATE NOCASE;
CREATE UNIQUE INDEX productos_codigo_barras ON productos (codigo_barras)
    WHERE borrado_en IS NULL AND codigo_barras IS NOT NULL;
