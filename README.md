# Punto de Venta (nativo)

Punto de venta de escritorio, 100 % Rust, para Windows 10/11, macOS y Linux. Es una sola app por
negocio y cada computadora guarda sus propios datos. La interfaz está hecha con **Slint** y no usa
navegador ni web view.

## Qué hace

- **Vender** con escáner o teclado. `3*martillo` agrega 3 piezas y `0.5*clavo` agrega medio kilo; F2 busca, F4 pausa,
  F6 cotiza y F12 cobra.
  - Si el producto tiene **varios precios**, al agregarlo pregunta cuál (Normal, Mayoreo, Contratista…), con ↑↓,
    Enter o el número. El mayoreo nunca se aplica solo: si la cantidad lo alcanza, se **sugiere** (preseleccionado al
    agregar, o como «¿Mayoreo $1.25?» en la partida) y el cajero decide.
  - Si es **a granel**, pregunta el peso.
- **Por unidad o a granel**: cada producto se vende de una sola forma. A granel es por kilo, con decimales, y la
  existencia se cuenta en kg.
- **Cobro** en efectivo, tarjeta o transferencia, o combinados. El cambio sale del efectivo. Vender bajo costo pide
  el PIN de un administrador.
- **En pausa**, con la fecha y hora en que se pausó («Hoy 10:15», «Ayer…», «jue 08/10 (hace 3 días)»).
- **Ventas y devoluciones**: buscar por día o folio, reimprimir, cancelar, y hacer devoluciones parciales o totales
  con PIN de administrador. El reembolso descuenta la parte proporcional del descuento de la venta.
- **Cotizaciones**: se guardan desde Vender. Se imprimen como ticket, en PDF hoja carta o en Excel, y se convierten
  en venta respetando sus precios mientras estén vigentes.
- **Caja compartida** por computadora: fondo, corte parcial, cierre con faltante o sobrante, desglose por empleado,
  y corte impreso.
- **Productos**: alta, edición y **borrado**. Lo borrado se conserva en el historial y su código queda libre.
  **Familias** se crean desde el mismo editor o se administran aparte. Hay entradas, conteos físicos y avisos de
  surtido, e **importar/exportar Excel** con vista previa.
- **Clientes**, **reportes** (por día, semana o mes, familia, producto, empleado, cliente y forma de pago, menos las
  devoluciones; se exportan a Excel) y **usuarios con PIN** (cajero o administrador).
- **Impresión**: los tickets van **directo a la impresora térmica** en ESC/POS, sin diálogos, y pueden abrir el
  cajón de dinero. Sin impresora térmica salen en PDF. La cotización en hoja carta siempre es PDF.
- **Respaldos**: uno automático cada día (se conservan 30), manual cuando se quiera, y **restaurar una base completa**
  (de un respaldo o de otra computadora). Antes de restaurar se valida el archivo y se guarda una copia de lo actual.

## Estructura

```
crates/nucleo/      reglas de negocio y SQLite (cargo test -p nucleo)
crates/impresion/   documentos → ESC/POS y PDF; envío crudo a impresoras (CUPS / winspool)
crates/app/         la interfaz en Slint (ui/*.slint) y su controlador en Rust
```

Los montos son enteros (centavos y milésimas), así que no hay errores de redondeo. La base de datos está en la
carpeta de datos de la app y se ve en *Ajustes → Respaldos*. El esquema crece **solo con migraciones nuevas**; nunca
se edita una que ya existe.

## Desarrollo

Requisitos: [Rust](https://rustup.rs). En Linux, además:

```sh
sudo apt install libfontconfig1-dev libxkbcommon-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

```sh
cargo run -p punto-venta                       # la app
cargo test -p nucleo -p impresion              # pruebas
cargo run -p impresion --example muestra -- muestras/   # ejemplos de ticket, cotización y corte (PDF, ESC/POS)
cargo run -p punto-venta --features captura -- --captura capturas/   # fotos de cada pantalla con datos de ejemplo
```

## Instaladores

```sh
cargo install cargo-packager --locked
cargo build --release -p punto-venta
cd crates/app && cargo packager --release      # el instalador de este sistema, en target/instaladores/
```

Para obtener los de los tres sistemas a la vez: GitHub → *Actions* → *Instaladores* → *Run workflow*. También se
generan al subir una etiqueta (`git tag v0.1.0 && git push --tags`), que deja un borrador de release.

Los instaladores no van firmados. La primera vez:

- **Windows**: SmartScreen → «Más información» → «Ejecutar de todas formas».
- **macOS**: arrastra la app a Aplicaciones e intenta abrirla una vez. Luego ve a *Configuración del Sistema →
  Privacidad y seguridad → «Abrir de todas formas»*.
