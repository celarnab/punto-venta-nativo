//! Fotos de cada pantalla con datos de ejemplo, dibujadas sin ventana (para revisar la interfaz).

use std::cell::RefCell;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nucleo::catalogo::{DatosFamilia, DatosProducto, OtroPrecio};
use nucleo::clientes::DatosCliente;
use nucleo::cobro::{Cobro, Metodo};
use nucleo::devoluciones::ADevolver;
use nucleo::usuarios::{DatosUsuario, Rol};
use nucleo::{Bd, Cantidad, Dinero};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Rgb8Pixel};

use crate::estado::Estado;
use crate::{Api, App, CobroUi, controlador};

const ANCHO: u32 = 1366;
const ALTO: u32 = 768;

struct SinVentana(Rc<MinimalSoftwareWindow>);

impl Platform for SinVentana {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

pub fn ejecutar(carpeta: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&carpeta)?;
    let ventana = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(SinVentana(ventana.clone()))).map_err(|e| e.to_string())?;

    let (bd, ids) = ejemplo()?;
    let temporal = std::env::temp_dir().join("punto-venta-captura");
    let _ = std::fs::remove_file(temporal.join("negocios.json")); // la lista de ejemplo, siempre la misma
    let mut negocios = nucleo::negocios::Negocios::cargar(&temporal);
    negocios.renombrar("principal", "Ferretería La Tuerca");
    let farmacia = negocios.agregar();
    negocios.renombrar(&farmacia.id, "Farmacia Vida Sana");
    let mut estado = Estado::new(bd, temporal, negocios);
    estado.configuracion = crate::configuracion::Configuracion { sin_ventanas: true, ..Default::default() };
    let app = App::new()?;
    ventana.set_size(slint::PhysicalSize::new(ANCHO, ALTO));
    app.show()?;
    controlador::conectar(&app, Rc::new(RefCell::new(estado)));
    let api = app.global::<Api>();
    let foto = |nombre: &str| guardar(&ventana, &carpeta.join(format!("{nombre}.png")));

    foto("01-entrar")?;
    api.invoke_entrar(ids.vianet as i32, "1234".into());
    for codigo in ["7501234567890", "7501234567890", "CAB-12"] {
        api.invoke_confirmar_busqueda(codigo.into());
    }
    api.invoke_confirmar_busqueda("0.750*CLA-2".into());
    foto("02-venta")?;
    api.invoke_buscar("torn".into());
    foto("03-busqueda")?;
    api.invoke_confirmar_busqueda("150*TOR-832".into());
    foto("04-precio")?;
    api.invoke_elegir_precio(0); // el normal, aunque 150 alcance el mayoreo: queda la sugerencia
    foto("04b-sugerencia")?;
    api.invoke_confirmar_busqueda("CLA-2".into());
    foto("05-peso")?;
    api.invoke_cancelar_pendiente();
    let cobro = [CobroUi { metodo: 0, monto: "1000".into() }];
    let respuesta = api.invoke_cobrar(slint::ModelRc::new(slint::VecModel::from(cobro.to_vec())), "".into());
    assert!(respuesta.ok, "no se pudo cobrar: {}", respuesta.mensaje);
    foto("05b-cobrada")?;
    api.invoke_cerrar_cobrada();

    for (pantalla, nombre) in [
        ("pausadas", "06-pausadas"),
        ("historial", "07-historial"),
        ("cotizaciones", "09-cotizaciones"),
        ("caja", "10-caja"),
        ("productos", "11-productos"),
        ("clientes", "13-clientes"),
        ("reportes", "14-reportes"),
        ("ajustes", "15-ajustes"),
    ] {
        api.set_pantalla(pantalla.into());
        foto(nombre)?;
        match pantalla {
            "historial" => {
                api.invoke_ver_venta(ids.venta_con_devolucion as i32);
                foto("08-detalle")?;
                api.invoke_empezar_devolucion();
                api.invoke_devolver_todo();
                let respuesta = api.invoke_devolver(0, "Prueba".into(), "1234".into());
                assert!(respuesta.ok, "no se pudo devolver: {}", respuesta.mensaje);
                foto("08b-devuelta")?;
                api.invoke_cerrar_detalle();
            }
            "ajustes" => {
                clic(&app, 1058.0, 74.0); // «Nuevo usuario»
                foto("16-usuario")?;
            }
            "caja" => {
                clic(&app, 775.0, 195.0); // «Efectivo contado»
                escribir(&app, "1400");
                clic(&app, 775.0, 235.0); // «Cerrar caja y hacer corte»
                foto("10b-corte")?;
            }
            "productos" => {
                api.invoke_editar_producto(ids.tornillo as i32);
                foto("12-editor")?;
                api.invoke_cerrar_editor();
            }
            _ => {}
        }
    }
    // Varios negocios: la lista desde la entrada y la configuración de uno nuevo (al final, porque cambiar de
    // negocio cierra la base de ejemplo).
    api.invoke_salir();
    api.set_eligiendo_negocio(true);
    foto("17-negocios")?;
    api.invoke_nuevo_negocio();
    foto("18-negocio-nuevo")?;
    api.invoke_cancelar_negocio_nuevo();
    println!("Capturas en {}", carpeta.display());
    Ok(())
}

fn clic(app: &App, x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let position = slint::LogicalPosition::new(x, y);
    app.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    app.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

fn escribir(app: &App, texto: &str) {
    use slint::platform::WindowEvent;
    for letra in texto.chars() {
        app.window().dispatch_event(WindowEvent::KeyPressed { text: letra.into() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: letra.into() });
    }
}

/// Dos pasadas: la primera crea la pantalla (y dispara sus `init`), la segunda pinta lo que cargaron.
fn guardar(ventana: &MinimalSoftwareWindow, ruta: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut pixeles = vec![Rgb8Pixel::default(); (ANCHO * ALTO) as usize];
    for _ in 0..2 {
        slint::platform::update_timers_and_animations();
        ventana.request_redraw();
        ventana.draw_if_needed(|renderer| {
            renderer.render(&mut pixeles, ANCHO as usize);
        });
    }
    let mut png = png::Encoder::new(BufWriter::new(File::create(ruta)?), ANCHO, ALTO);
    png.set_color(png::ColorType::Rgb);
    png.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = pixeles.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    png.write_header()?.write_image_data(&bytes)?;
    Ok(())
}

struct Ids {
    vianet: i64,
    tornillo: i64,
    venta_con_devolucion: i64,
}

/// Una ferretería con un poco de todo: precios de mayoreo, granel, ventas, una devolución, una
/// cotización y una venta en pausa.
fn ejemplo() -> nucleo::Resultado<(Bd, Ids)> {
    let d = |t: &str| t.parse::<Dinero>().expect("monto de ejemplo");
    let c = |t: &str| t.parse::<Cantidad>().expect("cantidad de ejemplo");
    let mut bd = Bd::en_memoria()?;
    let mut negocio = bd.negocio()?;
    negocio.nombre = "Ferretería La Tuerca".into();
    negocio.direccion = "Av. Juárez 123, Centro".into();
    negocio.telefono = "555 123 4567".into();
    bd.guardar_negocio(&negocio)?;

    let vianet =
        bd.crear_usuario(&DatosUsuario { nombre: "Vianet".into(), rol: Rol::Administrador, activo: true }, "1234")?;
    let jorge = bd.crear_usuario(&DatosUsuario { nombre: "Jorge".into(), rol: Rol::Cajero, activo: true }, "5678")?;
    let familia = |bd: &mut Bd, nombre: &str| {
        bd.crear_familia(&DatosFamilia { nombre: nombre.into(), alerta_stock: Some(c("5")) })
    };
    let herramientas = familia(&mut bd, "Herramientas")?.id;
    let tornilleria = familia(&mut bd, "Tornillería")?.id;
    let electricidad = familia(&mut bd, "Electricidad")?.id;

    let producto = |codigo: &str, nombre: &str, familia: i64, precio: &str, granel: bool, unidad: &str| DatosProducto {
        codigo: codigo.into(),
        nombre: nombre.into(),
        familia_id: Some(familia),
        precio: d(precio),
        costo: d(precio).proporcion(d("64"), d("100")),
        activo: true,
        granel,
        unidad: unidad.into(),
        ..Default::default()
    };
    let martillo =
        bd.crear_producto(&producto("7501234567890", "Martillo de uña 16 oz", herramientas, "189", false, "pza"))?;
    let mut tornillo = producto("TOR-832", "Tornillo para madera 8 × 1¼\"", tornilleria, "1.50", false, "pza");
    tornillo.codigo_barras = "7501206609026".into();
    tornillo.precios = vec![
        OtroPrecio { id: 0, nombre: "Mayoreo".into(), precio: d("1.25"), cantidad_minima: Some(c("100")) },
        OtroPrecio { id: 0, nombre: "Contratista".into(), precio: d("1.10"), cantidad_minima: None },
    ];
    let tornillo = bd.crear_producto(&tornillo)?;
    let clavo = bd.crear_producto(&producto("CLA-2", "Clavo estándar 2\"", tornilleria, "48", true, "kg"))?;
    let cable = bd.crear_producto(&producto("CAB-12", "Cable THW calibre 12", electricidad, "14.50", false, "m"))?;
    let otros = [
        ("7501234567891", "Martillo de uña 20 oz", herramientas, "239"),
        ("7501234567892", "Desarmador de cruz #2", herramientas, "49"),
        ("7501234567893", "Pinza de electricista 8\"", herramientas, "189"),
        ("TOR-1012", "Tornillo para madera 10 × 2\"", tornilleria, "2.40"),
        ("7501234567894", "Cinta de aislar 19 mm", electricidad, "22"),
        ("7501234567895", "Foco LED 12 W", electricidad, "49"),
    ];
    for (codigo, nombre, familia, precio) in otros {
        let p = bd.crear_producto(&producto(codigo, nombre, familia, precio, false, "pza"))?;
        bd.registrar_entrada(p.id, c("24"), "Inventario inicial")?;
    }
    for (id, cantidad) in [(martillo.id, "14"), (tornillo.id, "1240"), (clavo.id, "18.5"), (cable.id, "250")] {
        bd.registrar_entrada(id, c(cantidad), "Inventario inicial")?;
    }
    let cliente = bd.crear_cliente(&DatosCliente {
        nombre: "Construcciones Pérez".into(),
        telefono: "555 222 3344".into(),
        ..Default::default()
    })?;
    bd.crear_cliente(&DatosCliente {
        nombre: "José Hernández".into(),
        telefono: "555 987 6543".into(),
        ..Default::default()
    })?;

    bd.abrir_caja(vianet.id, d("500"))?;
    let efectivo = |monto: Dinero| [Cobro { metodo: Metodo::Efectivo, recibido: monto }];
    let vender = |bd: &mut Bd, cajero: i64, lineas: &[(i64, &str)]| -> nucleo::Resultado<nucleo::ventas::Venta> {
        let venta = bd.venta_en_curso(cajero)?.id;
        for &(producto, cantidad) in lineas {
            bd.agregar(venta, producto, c(cantidad), None)?;
        }
        let total = bd.venta(venta)?.totales.total;
        bd.cobrar(venta, &efectivo(total), None)
    };
    let con_devolucion = vender(&mut bd, jorge.id, &[(martillo.id, "2"), (tornillo.id, "50")])?;
    vender(&mut bd, vianet.id, &[(cable.id, "12.5"), (clavo.id, "1.250")])?;
    vender(&mut bd, jorge.id, &[(tornillo.id, "200")])?;
    vender(&mut bd, vianet.id, &[(martillo.id, "1")])?;
    let permiso = bd.autorizar("1234")?;
    let un_martillo = ADevolver { partida_id: con_devolucion.partidas[0].id, cantidad: c("1") };
    bd.devolver(con_devolucion.id, vianet.id, &[un_martillo], Metodo::Efectivo, "No le quedó", permiso)?;

    let cotizacion = bd.venta_en_curso(jorge.id)?.id;
    bd.agregar(cotizacion, martillo.id, c("6"), None)?;
    bd.agregar(cotizacion, cable.id, c("100"), None)?;
    bd.asignar_cliente(cotizacion, Some(cliente.id))?;
    bd.cotizar(cotizacion, 15, "Obra de la calle 5")?;
    let pausada = bd.venta_en_curso(jorge.id)?.id;
    bd.agregar(pausada, tornillo.id, c("40"), None)?;
    bd.pausar(pausada, "Señor de la camioneta roja")?;

    Ok((bd, Ids { vianet: vianet.id, tornillo: tornillo.id, venta_con_devolucion: con_devolucion.id }))
}
