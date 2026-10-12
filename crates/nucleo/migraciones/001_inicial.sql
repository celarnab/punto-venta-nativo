-- Esquema completo del punto de venta. Cambios futuros: siempre en una migración nueva (002, 003…),
-- nunca editando esta, porque las bases ya creadas no la vuelven a aplicar.
--
-- Montos en centavos y cantidades en milésimas (enteros): exactos y sumables.
-- Fechas en hora local, texto ISO ('2026-10-11 18:30:00').

CREATE TABLE negocio (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    nombre     TEXT    NOT NULL DEFAULT 'Mi negocio',
    direccion  TEXT    NOT NULL DEFAULT '',
    telefono   TEXT    NOT NULL DEFAULT '',
    rfc        TEXT    NOT NULL DEFAULT '',
    tasa_iva   INTEGER NOT NULL DEFAULT 1600,  -- centésimas de punto: 16 %
    pie_ticket TEXT    NOT NULL DEFAULT '¡Gracias por su compra!'
);
INSERT INTO negocio (id) VALUES (1);

CREATE TABLE usuarios (
    id       INTEGER PRIMARY KEY,
    nombre   TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    rol      TEXT    NOT NULL CHECK (rol IN ('cajero', 'administrador')),
    pin_hash TEXT    NOT NULL,
    activo   INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE familias (
    id           INTEGER PRIMARY KEY,
    nombre       TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    alerta_stock INTEGER
);

-- Un producto se vende por unidad (pza, m, caja…) o a granel (por kilo), nunca de las dos formas.
-- Los borrados se conservan para el historial, pero ya no aparecen ni ocupan su código.
CREATE TABLE productos (
    id           INTEGER PRIMARY KEY,
    codigo       TEXT    NOT NULL COLLATE NOCASE,
    nombre       TEXT    NOT NULL,
    familia_id   INTEGER REFERENCES familias ON DELETE SET NULL,
    granel       INTEGER NOT NULL DEFAULT 0,
    unidad       TEXT    NOT NULL DEFAULT 'pza',  -- 'kg' si es a granel
    precio       INTEGER NOT NULL DEFAULT 0 CHECK (precio >= 0),  -- por unidad o por kilo
    costo        INTEGER NOT NULL DEFAULT 0 CHECK (costo >= 0),
    existencia   INTEGER NOT NULL DEFAULT 0,
    stock_minimo INTEGER,
    stock_maximo INTEGER,
    alerta_stock INTEGER,
    activo       INTEGER NOT NULL DEFAULT 1,
    borrado_en   TEXT,
    CHECK (stock_minimo IS NULL OR stock_maximo IS NULL OR stock_minimo <= stock_maximo),
    CHECK (NOT granel OR unidad = 'kg')
);
CREATE UNIQUE INDEX productos_codigo ON productos (codigo) WHERE borrado_en IS NULL;
CREATE INDEX productos_nombre ON productos (nombre);
CREATE INDEX productos_familia ON productos (familia_id);

-- Otros precios: con `cantidad_minima` se aplican solos (mayoreo); sin ella, se eligen al vender.
CREATE TABLE precios (
    id              INTEGER PRIMARY KEY,
    producto_id     INTEGER NOT NULL REFERENCES productos ON DELETE CASCADE,
    nombre          TEXT    NOT NULL,
    precio          INTEGER NOT NULL CHECK (precio >= 0),
    cantidad_minima INTEGER CHECK (cantidad_minima IS NULL OR cantidad_minima > 0)
);
CREATE INDEX precios_producto ON precios (producto_id);

CREATE TABLE movimientos (
    id          INTEGER PRIMARY KEY,
    producto_id INTEGER NOT NULL REFERENCES productos ON DELETE CASCADE,
    tipo        TEXT    NOT NULL CHECK (tipo IN ('venta', 'entrada', 'ajuste', 'cancelacion', 'devolucion')),
    cantidad    INTEGER NOT NULL,  -- positivo suma, negativo resta
    referencia  TEXT    NOT NULL DEFAULT '',
    creado      TEXT    NOT NULL DEFAULT (datetime('now', 'localtime'))
);
CREATE INDEX movimientos_producto ON movimientos (producto_id, creado);

CREATE TABLE clientes (
    id       INTEGER PRIMARY KEY,
    nombre   TEXT NOT NULL,
    telefono TEXT NOT NULL DEFAULT '',
    correo   TEXT NOT NULL DEFAULT '',
    rfc      TEXT NOT NULL DEFAULT '',
    notas    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX clientes_nombre ON clientes (nombre);

-- Una sola caja por computadora, compartida por todos los que venden en ella.
CREATE TABLE cajas (
    id               INTEGER PRIMARY KEY,
    abrio_id         INTEGER NOT NULL REFERENCES usuarios,
    cerro_id         INTEGER REFERENCES usuarios,
    abierta_en       TEXT    NOT NULL DEFAULT (datetime('now', 'localtime')),
    cerrada_en       TEXT,
    fondo_inicial    INTEGER NOT NULL DEFAULT 0,
    efectivo_contado INTEGER
);
-- Todas las abiertas indexan el mismo valor, así que solo puede haber una.
CREATE UNIQUE INDEX una_caja_abierta ON cajas ((cerrada_en IS NULL)) WHERE cerrada_en IS NULL;

CREATE TABLE ventas (
    id                     INTEGER PRIMARY KEY,
    folio                  INTEGER UNIQUE,
    estado                 TEXT    NOT NULL DEFAULT 'abierta'
                                   CHECK (estado IN ('abierta', 'pausada', 'pagada', 'cancelada', 'cotizacion')),
    cajero_id              INTEGER NOT NULL REFERENCES usuarios,
    cliente_id             INTEGER REFERENCES clientes ON DELETE SET NULL,
    caja_id                INTEGER REFERENCES cajas,
    nota                   TEXT    NOT NULL DEFAULT '',
    descuento              INTEGER NOT NULL DEFAULT 0,
    subtotal               INTEGER NOT NULL DEFAULT 0,
    iva                    INTEGER NOT NULL DEFAULT 0,
    total                  INTEGER NOT NULL DEFAULT 0,
    creada                 TEXT    NOT NULL DEFAULT (datetime('now', 'localtime')),
    pausada_en             TEXT,     -- la última vez que se puso en pausa
    pagada_en              TEXT,
    autorizo_bajo_costo_id INTEGER REFERENCES usuarios,
    cancelo_id             INTEGER REFERENCES usuarios,  -- administrador que canceló una venta pagada
    -- Cotizaciones: su propia numeración y hasta cuándo se respetan los precios.
    folio_cotizacion       INTEGER UNIQUE,
    vigencia               TEXT,
    cotizacion_id          INTEGER REFERENCES ventas ON DELETE SET NULL  -- la cotización de la que salió
);
-- Cada cajero tiene a lo sumo una venta en curso; las demás esperan en pausa.
CREATE UNIQUE INDEX una_venta_abierta ON ventas (cajero_id) WHERE estado = 'abierta';
CREATE INDEX ventas_pagadas ON ventas (pagada_en) WHERE estado = 'pagada';

CREATE TABLE partidas (
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
    UNIQUE (venta_id, producto_id)
);

CREATE TABLE pagos (
    id       INTEGER PRIMARY KEY,
    venta_id INTEGER NOT NULL REFERENCES ventas ON DELETE CASCADE,
    metodo   TEXT    NOT NULL CHECK (metodo IN ('efectivo', 'tarjeta', 'transferencia')),
    recibido INTEGER NOT NULL,
    monto    INTEGER NOT NULL  -- aplicado a la venta, sin el cambio
);
CREATE INDEX pagos_venta ON pagos (venta_id);

-- Mercancía que el cliente regresa de una venta cobrada; puede ser solo una parte.
CREATE TABLE devoluciones (
    id          INTEGER PRIMARY KEY,
    venta_id    INTEGER NOT NULL REFERENCES ventas,
    creada      TEXT    NOT NULL DEFAULT (datetime('now', 'localtime')),
    cajero_id   INTEGER NOT NULL REFERENCES usuarios,
    caja_id     INTEGER NOT NULL REFERENCES cajas,
    autorizo_id INTEGER NOT NULL REFERENCES usuarios,
    metodo      TEXT    NOT NULL CHECK (metodo IN ('efectivo', 'tarjeta', 'transferencia')),
    motivo      TEXT    NOT NULL DEFAULT '',
    reembolso   INTEGER NOT NULL  -- lo que se regresó al cliente
);
CREATE INDEX devoluciones_venta ON devoluciones (venta_id);
CREATE INDEX devoluciones_fecha ON devoluciones (creada);

CREATE TABLE partidas_devueltas (
    id            INTEGER PRIMARY KEY,
    devolucion_id INTEGER NOT NULL REFERENCES devoluciones ON DELETE CASCADE,
    partida_id    INTEGER NOT NULL REFERENCES partidas,
    cantidad      INTEGER NOT NULL CHECK (cantidad > 0),
    importe       INTEGER NOT NULL  -- cantidad × precio de la partida, antes del descuento de la venta
);
CREATE INDEX partidas_devueltas_partida ON partidas_devueltas (partida_id);
