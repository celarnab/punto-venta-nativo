-- Tipo de negocio y lotes con caducidad (para farmacias).
--
-- Un producto puede tener varios lotes, cada uno con su número, sus fechas y la parte de la existencia que le
-- toca. `productos.existencia` sigue siendo el total; lo que no está en ningún lote es existencia «sin lote».

ALTER TABLE negocio ADD COLUMN giro TEXT NOT NULL DEFAULT 'abarrotes'
    CHECK (giro IN ('abarrotes', 'ferreteria', 'farmacia'));

CREATE TABLE lotes (
    id          INTEGER PRIMARY KEY,
    producto_id INTEGER NOT NULL REFERENCES productos ON DELETE CASCADE,
    numero      TEXT    NOT NULL COLLATE NOCASE,  -- alfanumérico, como viene en la caja
    fabricacion TEXT,                             -- opcional
    caducidad   TEXT    NOT NULL,                 -- se vende hasta ese día, inclusive
    existencia  INTEGER NOT NULL DEFAULT 0 CHECK (existencia >= 0),
    retirado_en TEXT,                             -- cuándo se sacó del inventario por caducidad
    UNIQUE (producto_id, numero)
);
CREATE INDEX lotes_caducidad ON lotes (caducidad) WHERE retirado_en IS NULL AND existencia > 0;

-- Movimientos: se agrega el tipo «caducidad», el lote que se movió y, al retirar por caducidad, el costo unitario de
-- ese momento (la pérdida no cambia si después cambia el costo del producto). SQLite no deja cambiar un CHECK, así
-- que la tabla se reconstruye con los mismos ids.
CREATE TABLE movimientos_nueva (
    id          INTEGER PRIMARY KEY,
    producto_id INTEGER NOT NULL REFERENCES productos ON DELETE CASCADE,
    tipo        TEXT    NOT NULL
                        CHECK (tipo IN ('venta', 'entrada', 'ajuste', 'cancelacion', 'devolucion', 'caducidad')),
    cantidad    INTEGER NOT NULL,  -- positivo suma, negativo resta
    referencia  TEXT    NOT NULL DEFAULT '',
    creado      TEXT    NOT NULL DEFAULT (datetime('now', 'localtime')),
    lote_id     INTEGER REFERENCES lotes ON DELETE SET NULL,
    costo       INTEGER
);
INSERT INTO movimientos_nueva (id, producto_id, tipo, cantidad, referencia, creado)
    SELECT id, producto_id, tipo, cantidad, referencia, creado FROM movimientos;
DROP TABLE movimientos;
ALTER TABLE movimientos_nueva RENAME TO movimientos;
CREATE INDEX movimientos_producto ON movimientos (producto_id, creado);
CREATE INDEX movimientos_caducidad ON movimientos (creado) WHERE tipo = 'caducidad';

-- Partidas: cada una puede llevar su lote, así que una venta puede tener el mismo producto de dos lotes. La
-- unicidad pasa de (venta, producto) a (venta, producto, lote). Se conservan los ids, que usan las devoluciones.
CREATE TABLE partidas_nueva (
    id          INTEGER PRIMARY KEY,
    venta_id    INTEGER NOT NULL REFERENCES ventas ON DELETE CASCADE,
    producto_id INTEGER NOT NULL REFERENCES productos,
    descripcion TEXT    NOT NULL,
    unidad      TEXT    NOT NULL,           -- 'pza', 'kg', 'm'…
    cantidad    INTEGER NOT NULL CHECK (cantidad > 0),
    precio      INTEGER NOT NULL,
    precio_fijo INTEGER NOT NULL DEFAULT 0, -- elegido al vender: ya no sigue a la cantidad
    costo       INTEGER NOT NULL,           -- al momento de vender
    importe     INTEGER NOT NULL,           -- cantidad × precio, redondeado al centavo
    lote_id     INTEGER REFERENCES lotes
);
INSERT INTO partidas_nueva (id, venta_id, producto_id, descripcion, unidad, cantidad, precio, precio_fijo, costo, importe)
    SELECT id, venta_id, producto_id, descripcion, unidad, cantidad, precio, precio_fijo, costo, importe FROM partidas;
DROP TABLE partidas;
ALTER TABLE partidas_nueva RENAME TO partidas;
CREATE UNIQUE INDEX partidas_unica ON partidas (venta_id, producto_id, coalesce(lote_id, 0));
