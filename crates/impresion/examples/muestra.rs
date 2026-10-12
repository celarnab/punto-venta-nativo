//! Genera ejemplos de todo lo que se imprime, para revisarlos sin impresora:
//! `cargo run -p impresion --example muestra -- carpeta/`

use std::path::PathBuf;

use impresion::escpos::{self, Opciones};
use impresion::{corte, devolucion, pdf, ticket};
use nucleo::catalogo::{DatosProducto, OtroPrecio};
use nucleo::cobro::{Cobro, Metodo};
use nucleo::devoluciones::ADevolver;
use nucleo::usuarios::{DatosUsuario, Rol};
use nucleo::{Bd, Cantidad, Dinero};

fn main() {
    let carpeta = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&carpeta).unwrap();
    let d = |t: &str| t.parse::<Dinero>().unwrap();
    let c = |t: &str| t.parse::<Cantidad>().unwrap();

    let mut bd = Bd::en_memoria().unwrap();
    let mut negocio = bd.negocio().unwrap();
    negocio.nombre = "Ferretería La Tuerca".into();
    negocio.direccion = "Av. Juárez 123, Centro".into();
    negocio.telefono = "555 123 4567".into();
    negocio.rfc = "TUE010101AB1".into();
    bd.guardar_negocio(&negocio).unwrap();
    let vianet = bd
        .crear_usuario(&DatosUsuario { nombre: "Vianet".into(), rol: Rol::Administrador, activo: true }, "1234")
        .unwrap();
    bd.abrir_caja(vianet.id, d("500")).unwrap();

    let producto = |codigo: &str, nombre: &str, precio: &str, granel: bool| DatosProducto {
        codigo: codigo.into(),
        nombre: nombre.into(),
        precio: d(precio),
        costo: d(precio).proporcion(d("65"), d("100")),
        activo: true,
        granel,
        ..Default::default()
    };
    let martillo = bd.crear_producto(&producto("7501234567890", "Martillo de uña 16 oz", "189", false)).unwrap();
    let mut con_precios = producto("TOR-832", "Tornillo para madera 8 × 1¼\"", "1.50", false);
    con_precios.precios =
        vec![OtroPrecio { id: 0, nombre: "Contratista".into(), precio: d("1.20"), cantidad_minima: None }];
    let tornillo = bd.crear_producto(&con_precios).unwrap();
    let clavo = bd.crear_producto(&producto("CLA-2", "Clavo estándar 2\" (a granel)", "48", true)).unwrap();
    let contratista = tornillo.precios[0].id;

    let venta = bd.venta_en_curso(vianet.id).unwrap().id;
    bd.agregar(venta, martillo.id, c("2"), None).unwrap();
    bd.agregar(venta, tornillo.id, c("150"), Some(contratista)).unwrap();
    bd.agregar(venta, clavo.id, c("0.750"), None).unwrap();
    let pagada = bd.cobrar(venta, &[Cobro { metodo: Metodo::Efectivo, recibido: d("1000") }], None).unwrap();
    let ticket_venta = bd.ticket(pagada.id).unwrap();

    let cotizacion = bd.venta_en_curso(vianet.id).unwrap().id;
    bd.agregar(cotizacion, martillo.id, c("6"), None).unwrap();
    bd.agregar(cotizacion, clavo.id, c("5"), None).unwrap();
    let cotizada = bd.cotizar(cotizacion, 15, "Obra de la calle 5").unwrap();
    let ticket_cotizacion = bd.ticket(cotizada.id).unwrap();

    let permiso = bd.autorizar("1234").unwrap();
    let un_martillo = ADevolver { partida_id: pagada.partidas[0].id, cantidad: c("1") };
    let devuelta = bd.devolver(pagada.id, vianet.id, &[un_martillo], Metodo::Efectivo, "No le quedó", permiso).unwrap();
    let comprobante = bd.comprobante_devolucion(devuelta.id).unwrap();

    let caja = bd.caja_abierta().unwrap().unwrap();
    let cortada = bd.cerrar_caja(caja.id, vianet.id, d("1000")).unwrap();

    let documentos = [
        ("ticket", ticket(&ticket_venta)),
        ("cotizacion-ticket", ticket(&ticket_cotizacion)),
        ("devolucion", devolucion(&comprobante)),
        ("corte", corte(&negocio, &cortada)),
    ];
    for (nombre, documento) in &documentos {
        std::fs::write(carpeta.join(format!("{nombre}.pdf")), pdf::rollo(documento, 42)).unwrap();
        std::fs::write(carpeta.join(format!("{nombre}.escpos")), escpos::codificar(documento, Opciones::default()))
            .unwrap();
        let texto: Vec<String> = documento.renglones(42).into_iter().map(|r| r.texto).collect();
        std::fs::write(carpeta.join(format!("{nombre}.txt")), texto.join("\n")).unwrap();
    }
    std::fs::write(carpeta.join("cotizacion-hoja.pdf"), pdf::cotizacion(&ticket_cotizacion)).unwrap();
    println!("Ejemplos en {}", carpeta.display());
}
