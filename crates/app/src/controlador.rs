//! Atiende cada acción de la interfaz: lee lo que hace falta del núcleo, actúa y vuelve a pintar.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use chrono::{Datelike, Days, Local, Months, NaiveDate};
use impresion::{escpos, sistema};
use nucleo::catalogo::{DatosFamilia, DatosProducto, OtroPrecio};
use nucleo::clientes::DatosCliente;
use nucleo::cobro::{Cobro, Metodo};
use nucleo::devoluciones::ADevolver;
use nucleo::negocio::Negocio;
use nucleo::reportes::{Filtro, Seccion};
use nucleo::texto::normalizar;
use nucleo::usuarios::{Autorizacion, DatosUsuario, Rol};
use nucleo::ventas::Estado as EstadoVenta;
use nucleo::{Cantidad, Dinero, Error, Porcentaje, Resultado, exportacion, importacion};
use slint::{ComponentHandle, Model, SharedString, Timer};

use crate::estado::{Estado, OtroPrecioTexto, Pendiente};
use crate::{
    Api, App, ClienteUi, CobroUi, EditorProductoUi, ImpresoraUi, NegocioUi, PrecioUi, RespuestaUi, imprimir, respaldos,
    vista,
};

type Compartido = Rc<RefCell<Estado>>;

/// Conecta un callback de la interfaz: `conectar!(api, ui, estado, on_x, |app, e, args…| cuerpo)`.
macro_rules! conectar {
    ($api:ident, $ui:ident, $estado:ident, $on:ident, |$app:ident, $e:ident $(, $arg:ident)*| $cuerpo:expr) => {{
        let ui = $ui.as_weak();
        let estado = $estado.clone();
        $api.$on(move |$($arg),*| {
            let $app = ui.upgrade().expect("la ventana sigue abierta");
            #[allow(unused_mut)]
            let mut $e = estado.borrow_mut();
            $cuerpo
        });
    }};
}

pub fn conectar(app: &App, estado: Compartido) {
    let api = app.global::<Api>();
    api.on_pesos(|monto| Dinero::redondeando(monto as f64).pesos().into());
    {
        let e = estado.borrow();
        refrescar_inicio(app, &e);
    }

    // — Sesión —
    conectar!(api, app, estado, on_configurar, |app, e, negocio, nombre, pin| {
        match configurar(&mut e, &negocio, &nombre, &pin) {
            Ok(()) => {
                refrescar_inicio(&app, &e);
                entrar_como(&app, &mut e);
                SharedString::new()
            }
            Err(error) => error.to_string().into(),
        }
    });
    conectar!(api, app, estado, on_entrar, |app, e, id, pin| {
        match e.bd.entrar(id.into(), &pin) {
            Ok(usuario) => {
                e.sesion = Some(usuario);
                entrar_como(&app, &mut e);
                true
            }
            Err(_) => false,
        }
    });
    conectar!(api, app, estado, on_salir, |app, e| {
        e.sesion = None;
        e.venta_id = None;
        app.global::<Api>().set_hay_sesion(false);
        refrescar_inicio(&app, &e); // la lista de quién puede entrar siempre al día
    });
    conectar!(api, app, estado, on_cambiar_mi_pin, |app, e, pin| {
        let resultado = e.usuario().map(|u| u.id).and_then(|id| e.bd.cambiar_pin(id, &pin));
        texto_de(&app, resultado.map(|()| "PIN actualizado."))
    });

    // — Vender —
    conectar!(api, app, estado, on_abrir_caja, |app, e, fondo| {
        let resultado = leer::<Dinero>(&fondo)
            .and_then(|fondo| Ok((e.usuario()?.id, fondo)))
            .and_then(|(id, fondo)| e.bd.abrir_caja(id, fondo));
        let mensaje = texto_de(&app, resultado.map(|_| "Caja abierta. ¡Buenas ventas!"));
        refrescar_venta(&app, &mut e);
        mensaje
    });
    conectar!(api, app, estado, on_buscar, |app, e, texto| {
        let (_, buscado) = separar(&texto);
        e.resultados =
            if buscado.is_empty() { Vec::new() } else { e.bd.buscar_productos(buscado, true, 8).unwrap_or_default() };
        e.resaltado = 0;
        let api = app.global::<Api>();
        api.set_resaltado(0);
        api.set_resultados(vista::modelo(e.resultados.iter().map(vista::producto)));
    });
    conectar!(api, app, estado, on_confirmar_busqueda, |app, e, texto| {
        let (cantidad, buscado) = separar(&texto);
        if buscado.is_empty() {
            return false;
        }
        let resaltado = app.global::<Api>().get_resaltado() as usize;
        let exacto = e.bd.producto_por_codigo(buscado).ok().flatten();
        match exacto.or_else(|| e.resultados.get(resaltado).cloned()) {
            Some(producto) => {
                elegir_producto(&app, &mut e, producto, cantidad);
                true
            }
            None => {
                avisar(&app, format!("No encontré «{buscado}»."), true);
                false
            }
        }
    });
    conectar!(api, app, estado, on_elegir_resultado, |app, e, indice, texto| {
        let (cantidad, _) = separar(&texto);
        match e.resultados.get(indice as usize).cloned() {
            Some(producto) => {
                elegir_producto(&app, &mut e, producto, cantidad);
                true
            }
            None => false,
        }
    });
    conectar!(api, app, estado, on_agregar_peso, |app, e, texto| {
        match leer::<Cantidad>(&texto).and_then(positiva) {
            Ok(peso) => {
                let Some(mut pendiente) = e.pendiente.take() else { return };
                pendiente.cantidad = Some(peso);
                app.global::<Api>().set_pide_peso(false);
                if pendiente.producto.precios.is_empty() {
                    agregar(&app, &mut e, pendiente.producto.id, peso, None);
                } else {
                    preguntar_precio(&app, &mut e, pendiente);
                }
            }
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });
    conectar!(api, app, estado, on_elegir_precio, |app, e, precio_id| {
        let Some(pendiente) = e.pendiente.take() else { return };
        app.global::<Api>().set_pide_precio(false);
        let elegido = (precio_id != 0).then_some(precio_id.into());
        match pendiente.partida {
            Some(partida) => {
                let resultado = venta_actual(&e).and_then(|venta| e.bd.elegir_precio(venta, partida, elegido));
                mostrar_venta(&app, &mut e, resultado);
            }
            None => {
                let cantidad = pendiente.cantidad.unwrap_or(Cantidad::UNO);
                agregar(&app, &mut e, pendiente.producto.id, cantidad, elegido);
            }
        }
    });
    conectar!(api, app, estado, on_cancelar_pendiente, |app, e| {
        e.pendiente = None;
        let api = app.global::<Api>();
        api.set_pide_precio(false);
        api.set_pide_peso(false);
    });
    conectar!(api, app, estado, on_cambiar_cantidad, |app, e, partida, texto| {
        let resultado = leer::<Cantidad>(&texto)
            .and_then(|cantidad| Ok((venta_actual(&e)?, cantidad)))
            .and_then(|(venta, cantidad)| e.bd.cambiar_cantidad(venta, partida.into(), cantidad));
        mostrar_venta(&app, &mut e, resultado);
    });
    conectar!(api, app, estado, on_sumar, |app, e, partida, signo| {
        let resultado = venta_actual(&e).and_then(|id| {
            let venta = e.bd.venta(id)?;
            let partida =
                venta.partidas.iter().find(|p| p.id == i64::from(partida)).ok_or(Error::NoEncontrado("Producto"))?;
            // A granel se suma de 100 en 100 gramos.
            let paso = if partida.unidad == "kg" { Cantidad::crudo(100) } else { Cantidad::UNO };
            let cantidad = if signo < 0 { -paso } else { paso };
            e.bd.agregar(id, partida.producto_id, cantidad, None)
        });
        mostrar_venta(&app, &mut e, resultado);
    });
    conectar!(api, app, estado, on_quitar, |app, e, partida| {
        let resultado = venta_actual(&e).and_then(|venta| e.bd.quitar(venta, partida.into()));
        mostrar_venta(&app, &mut e, resultado);
    });
    conectar!(api, app, estado, on_cambiar_precio, |app, e, partida| {
        let resultado = venta_actual(&e).and_then(|id| {
            let venta = e.bd.venta(id)?;
            let partida =
                venta.partidas.iter().find(|p| p.id == i64::from(partida)).ok_or(Error::NoEncontrado("Producto"))?;
            Ok((partida.id, e.bd.producto(partida.producto_id)?))
        });
        match resultado {
            Ok((_, producto)) if producto.precios.is_empty() => {
                avisar(&app, "Este producto tiene un solo precio.", true)
            }
            Ok((partida, producto)) => {
                let cantidad = venta_actual(&e)
                    .and_then(|v| e.bd.venta(v))
                    .ok()
                    .and_then(|v| v.partidas.iter().find(|p| p.id == partida).map(|p| p.cantidad));
                preguntar_precio(&app, &mut e, Pendiente { producto, cantidad, partida: Some(partida) })
            }
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });
    conectar!(api, app, estado, on_aplicar_descuento, |app, e, texto| {
        let resultado = leer::<Dinero>(if texto.trim().is_empty() { "0" } else { &texto })
            .and_then(|descuento| Ok((venta_actual(&e)?, descuento)))
            .and_then(|(venta, descuento)| e.bd.aplicar_descuento(venta, descuento));
        mostrar_venta(&app, &mut e, resultado);
    });
    conectar!(api, app, estado, on_buscar_clientes, |app, e, texto| {
        let clientes = e.bd.buscar_clientes(&texto, 50).unwrap_or_default();
        app.global::<Api>().set_clientes(vista::modelo(clientes.iter().map(vista::cliente)));
    });
    conectar!(api, app, estado, on_asignar_cliente, |app, e, cliente| {
        let cliente = (cliente != 0).then_some(cliente.into());
        let resultado = venta_actual(&e).and_then(|venta| e.bd.asignar_cliente(venta, cliente));
        mostrar_venta(&app, &mut e, resultado);
    });
    conectar!(api, app, estado, on_crear_cliente_rapido, |app, e, nombre, telefono| {
        let datos = DatosCliente { nombre: nombre.into(), telefono: telefono.into(), ..Default::default() };
        let resultado = e.bd.crear_cliente(&datos).and_then(|cliente| {
            let venta = venta_actual(&e)?;
            e.bd.asignar_cliente(venta, Some(cliente.id))
        });
        mostrar_venta(&app, &mut e, resultado);
    });
    conectar!(api, app, estado, on_pausar, |app, e, nota| {
        let resultado = venta_actual(&e).and_then(|venta| e.bd.pausar(venta, &nota));
        let hecho = resultado.is_ok();
        if hecho {
            e.venta_id = None;
            avisar(&app, "Venta en pausa. La encuentras en «En pausa».", false);
            refrescar_venta(&app, &mut e);
        } else if let Err(error) = resultado {
            avisar(&app, error.to_string(), true);
        }
        hecho
    });
    conectar!(api, app, estado, on_tirar, |app, e| {
        if let Err(error) = venta_actual(&e).and_then(|venta| e.bd.descartar(venta)) {
            avisar(&app, error.to_string(), true);
        }
        e.venta_id = None;
        refrescar_venta(&app, &mut e);
    });
    conectar!(api, app, estado, on_cotizar, |app, e, dias, nota| {
        let resultado = dias
            .trim()
            .parse::<u32>()
            .map_err(|_| Error::invalido("Los días de vigencia deben ser un número entero."))
            .and_then(|dias| Ok((venta_actual(&e)?, dias)))
            .and_then(|(venta, dias)| e.bd.cotizar(venta, dias, &nota));
        match resultado {
            Ok(cotizacion) => {
                let folio = cotizacion.folio_cotizacion.unwrap_or_default();
                avisar(&app, format!("Cotización {folio} guardada. Está en «Cotizaciones» para imprimirla."), false);
                e.venta_id = None;
                refrescar_venta(&app, &mut e);
                true
            }
            Err(error) => {
                avisar(&app, error.to_string(), true);
                false
            }
        }
    });
    conectar!(api, app, estado, on_cobrar, |app, e, lineas, pin| cobrar(&app, &mut e, lineas, &pin));
    conectar!(api, app, estado, on_imprimir_cobrada, |app, e| {
        if let Some(id) = e.cobrada {
            imprimir_ticket(&app, &e, id, false);
        }
    });
    conectar!(api, app, estado, on_cerrar_cobrada, |app, e| {
        e.cobrada = None;
        app.global::<Api>().set_mostrando_cobrada(false);
    });

    // — En pausa —
    conectar!(api, app, estado, on_cargar_pausadas, |app, e| refrescar_pausadas(&app, &e));
    conectar!(api, app, estado, on_retomar, |app, e, id| {
        match e.usuario().map(|u| u.id).and_then(|cajero| e.bd.retomar(id.into(), cajero)) {
            Ok(venta) => {
                e.venta_id = Some(venta.id);
                app.global::<Api>().set_pantalla("venta".into());
                refrescar_venta(&app, &mut e);
            }
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });
    conectar!(api, app, estado, on_descartar, |app, e, id| {
        if let Err(error) = e.bd.descartar(id.into()) {
            avisar(&app, error.to_string(), true);
        }
        refrescar_pausadas(&app, &e);
    });

    // — Historial y devoluciones —
    conectar!(api, app, estado, on_cargar_historial, |app, e, fecha| {
        let dia = if fecha.trim().is_empty() { Ok(vista::hoy()) } else { leer_fecha(&fecha) };
        match dia {
            Ok(dia) => refrescar_historial(&app, &e, dia),
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });
    conectar!(api, app, estado, on_buscar_folio, |app, e, folio| {
        match folio.trim().parse::<i64>().map_err(|_| Error::invalido("Escribe el número de folio.")) {
            Ok(folio) => match e.bd.venta_por_folio(folio) {
                Ok(venta) => mostrar_detalle(&app, &mut e, venta.id),
                Err(_) => avisar(&app, format!("No hay una venta con folio {folio}."), true),
            },
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });
    conectar!(api, app, estado, on_ver_venta, |app, e, id| mostrar_detalle(&app, &mut e, id.into()));
    conectar!(api, app, estado, on_ver_cotizacion, |app, e, id| mostrar_detalle(&app, &mut e, id.into()));
    conectar!(api, app, estado, on_cerrar_detalle, |app, e| {
        e.detalle = None;
        app.global::<Api>().set_mostrando_detalle(false);
    });
    conectar!(api, app, estado, on_reimprimir, |app, e, id| imprimir_ticket(&app, &e, id.into(), false));
    conectar!(api, app, estado, on_cancelar_venta, |app, e, id, pin| {
        match autorizado(&e, &pin).and_then(|permiso| e.bd.cancelar(id.into(), permiso)) {
            Ok(venta) => {
                avisar(&app, format!("Venta {} cancelada.", venta.folio.unwrap_or_default()), false);
                mostrar_detalle(&app, &mut e, id.into());
                let dia = leer_fecha(&app.global::<Api>().get_historial_fecha()).unwrap_or(vista::hoy());
                refrescar_historial(&app, &e, dia);
                RespuestaUi { ok: true, ..Default::default() }
            }
            Err(error) => fallo(error),
        }
    });
    conectar!(api, app, estado, on_empezar_devolucion, |app, e| {
        e.devolucion.clear();
        refrescar_devolucion(&app, &e, true);
    });
    conectar!(api, app, estado, on_poner_devolucion, |app, e, partida, texto| {
        match texto.trim() {
            "" => e.devolucion.remove(&partida.into()),
            texto => match texto.parse::<Cantidad>() {
                Ok(cantidad) => e.devolucion.insert(partida.into(), cantidad),
                Err(_) => None,
            },
        };
        refrescar_devolucion(&app, &e, false);
    });
    conectar!(api, app, estado, on_devolver_todo, |app, e| {
        if let Some(venta) = e.detalle.and_then(|id| e.bd.venta(id).ok()) {
            e.devolucion = venta
                .partidas
                .iter()
                .map(|p| (p.id, p.cantidad - p.devuelto))
                .filter(|(_, c)| c.es_positivo())
                .collect();
        }
        refrescar_devolucion(&app, &e, true);
    });
    conectar!(api, app, estado, on_devolver, |app, e, metodo, motivo, pin| devolver(
        &app, &mut e, metodo, &motivo, &pin
    ));
    conectar!(api, app, estado, on_imprimir_devolucion, |app, e, id| {
        let resultado = e.bd.comprobante_devolucion(id.into()).map_err(|e| e.to_string()).and_then(|comprobante| {
            imprimir::documento(&e.configuracion, &e.carpeta, "devolucion", &impresion::devolucion(&comprobante), false)
        });
        if let Err(error) = resultado {
            avisar(&app, error, true);
        }
    });

    // — Cotizaciones —
    conectar!(api, app, estado, on_cargar_cotizaciones, |app, e, filtro| {
        e.filtro_cotizaciones = filtro.to_string();
        refrescar_cotizaciones(&app, &e);
    });
    conectar!(api, app, estado, on_convertir_cotizacion, |app, e, id| {
        match e.usuario().map(|u| u.id).and_then(|cajero| e.bd.convertir_cotizacion(id.into(), cajero)) {
            Ok(venta) => {
                e.venta_id = Some(venta.id);
                e.detalle = None;
                let api = app.global::<Api>();
                api.set_mostrando_detalle(false);
                api.set_pantalla("venta".into());
                refrescar_venta(&app, &mut e);
                avisar(&app, "Cotización lista para cobrar. Lo que tenías en pantalla quedó en pausa.", false);
            }
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });
    conectar!(api, app, estado, on_borrar_cotizacion, |app, e, id| {
        if let Err(error) = e.bd.borrar_cotizacion(id.into()) {
            avisar(&app, error.to_string(), true);
        }
        e.detalle = None;
        app.global::<Api>().set_mostrando_detalle(false);
        refrescar_cotizaciones(&app, &e);
    });
    conectar!(api, app, estado, on_cotizacion_ticket, |app, e, id| imprimir_ticket(&app, &e, id.into(), false));
    conectar!(api, app, estado, on_cotizacion_pdf, |app, e, id| {
        let resultado = e.bd.ticket(id.into()).map_err(|e| e.to_string()).and_then(|ticket| {
            let nombre = format!("cotizacion-{}", ticket.venta.folio_cotizacion.unwrap_or_default());
            imprimir::abrir_pdf(&e.configuracion, &e.carpeta, &nombre, &impresion::pdf::cotizacion(&ticket))
        });
        if let Err(error) = resultado {
            avisar(&app, error, true);
        }
    });
    conectar!(api, app, estado, on_cotizacion_excel, |app, e, id| {
        let Ok(ticket) = e.bd.ticket(id.into()) else { return };
        let nombre = format!("cotizacion-{}.xlsx", ticket.venta.folio_cotizacion.unwrap_or_default());
        if let Some(ruta) = guardar_como("Excel", "xlsx", &nombre) {
            let resultado =
                exportacion::cotizacion_a_excel(&ticket.negocio, &ticket.venta).and_then(|b| escribir(&ruta, b));
            texto_de(&app, resultado.map(|()| "Cotización exportada."));
        }
    });

    // — Caja —
    conectar!(api, app, estado, on_cargar_caja, |app, e| refrescar_caja(&app, &e));
    conectar!(api, app, estado, on_cerrar_caja, |app, e, contado| {
        let resultado = leer::<Dinero>(&contado).and_then(|contado| {
            let caja = e.bd.caja_abierta()?.ok_or(Error::invalido("La caja ya está cerrada."))?;
            let usuario = e.usuario()?.id;
            e.bd.cerrar_caja(caja.id, usuario, contado)
        });
        match resultado {
            Ok(corte) => {
                if let Ok(negocio) = e.bd.negocio() {
                    let doc = impresion::corte(&negocio, &corte);
                    if let Err(error) = imprimir::documento(&e.configuracion, &e.carpeta, "corte", &doc, false) {
                        avisar(&app, error, true);
                    }
                }
                refrescar_caja(&app, &e);
                app.global::<Api>().set_caja_abierta(false);
                SharedString::new()
            }
            Err(error) => error.to_string().into(),
        }
    });
    conectar!(api, app, estado, on_imprimir_corte, |app, e, id| {
        let resultado = e.bd.corte(id.into()).and_then(|corte| Ok((e.bd.negocio()?, corte)));
        match resultado {
            Ok((negocio, corte)) => {
                let doc = impresion::corte(&negocio, &corte);
                if let Err(error) = imprimir::documento(&e.configuracion, &e.carpeta, "corte", &doc, false) {
                    avisar(&app, error, true);
                }
            }
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });

    // — Productos —
    conectar!(api, app, estado, on_buscar_productos, |app, e, texto, inactivos, surtir| {
        e.busqueda_productos = (texto.into(), inactivos, surtir);
        refrescar_productos(&app, &e);
    });
    conectar!(api, app, estado, on_editar_producto, |app, e, id| abrir_editor(&app, &mut e, id.into()));
    conectar!(api, app, estado, on_cerrar_editor, |app, e| {
        e.editor_precios.clear();
        app.global::<Api>().set_editando(false);
    });
    conectar!(api, app, estado, on_guardar_producto, |app, e, editor| {
        let resultado =
            e.admin().map(|_| ()).and_then(|()| datos_producto(&e, &editor)).and_then(|datos| match editor.id {
                0 => e.bd.crear_producto(&datos),
                id => e.bd.actualizar_producto(id.into(), &datos),
            });
        let mensaje = texto_de(&app, resultado.map(|p| format!("{} guardado.", p.nombre)));
        refrescar_productos(&app, &e);
        mensaje
    });
    conectar!(api, app, estado, on_borrar_producto, |app, e, id| {
        let resultado = e.admin().map(|_| ()).and_then(|()| e.bd.borrar_producto(id.into()));
        let mensaje = texto_de(&app, resultado.map(|()| "Producto borrado."));
        refrescar_productos(&app, &e);
        mensaje
    });
    conectar!(api, app, estado, on_agregar_otro_precio, |app, e| {
        e.editor_precios.push(OtroPrecioTexto::default());
        mostrar_otros_precios(&app, &e);
    });
    conectar!(api, app, estado, on_quitar_otro_precio, |app, e, renglon| {
        if (renglon as usize) < e.editor_precios.len() {
            e.editor_precios.remove(renglon as usize);
        }
        mostrar_otros_precios(&app, &e);
    });
    conectar!(api, app, estado, on_editar_otro_precio, |_app, e, renglon, campo, valor| {
        if let Some(precio) = e.editor_precios.get_mut(renglon as usize) {
            let destino = match campo {
                0 => &mut precio.nombre,
                1 => &mut precio.precio,
                _ => &mut precio.desde,
            };
            *destino = valor.into();
        }
    });
    conectar!(api, app, estado, on_crear_familia, |app, e, nombre, alerta| {
        let resultado = e.admin().map(|_| ()).and_then(|()| {
            let alerta = opcional::<Cantidad>(&alerta)?;
            e.bd.crear_familia(&DatosFamilia { nombre: nombre.into(), alerta_stock: alerta })
        });
        match resultado {
            Ok(familia) => {
                refrescar_familias(&app, &e);
                let familias = e.bd.familias().unwrap_or_default();
                familias.iter().position(|f| f.id == familia.id).map_or(-1, |i| i as i32 + 1)
            }
            Err(error) => {
                avisar(&app, error.to_string(), true);
                -1
            }
        }
    });
    conectar!(api, app, estado, on_renombrar_familia, |app, e, id, nombre, alerta| {
        let resultado = e.admin().map(|_| ()).and_then(|()| {
            let alerta = opcional::<Cantidad>(&alerta)?;
            e.bd.actualizar_familia(id.into(), &DatosFamilia { nombre: nombre.into(), alerta_stock: alerta })
        });
        let mensaje = texto_de(&app, resultado.map(|f| format!("Familia «{}» guardada.", f.nombre)));
        refrescar_familias(&app, &e);
        mensaje
    });
    conectar!(api, app, estado, on_borrar_familia, |app, e, id| {
        if let Err(error) = e.admin().map(|_| ()).and_then(|()| e.bd.borrar_familia(id.into())) {
            avisar(&app, error.to_string(), true);
        }
        refrescar_familias(&app, &e);
    });
    conectar!(api, app, estado, on_registrar_entrada, |app, e, id, cantidad, referencia| {
        let resultado = e
            .admin()
            .map(|_| ())
            .and_then(|()| leer::<Cantidad>(&cantidad))
            .and_then(|cantidad| e.bd.registrar_entrada(id.into(), cantidad, &referencia));
        let ok = resultado.is_ok();
        let mensaje = texto_de(&app, resultado.map(|_| "Entrada registrada."));
        if ok {
            actualizar_existencia_editor(&app, &e, id.into());
        }
        mensaje
    });
    conectar!(api, app, estado, on_ajustar_existencia, |app, e, id, contada, motivo| {
        let resultado = e
            .admin()
            .map(|_| ())
            .and_then(|()| leer::<Cantidad>(&contada))
            .and_then(|contada| e.bd.ajustar_existencia(id.into(), contada, &motivo));
        let ok = resultado.is_ok();
        let mensaje = texto_de(&app, resultado.map(|_| "Existencia ajustada."));
        if ok {
            actualizar_existencia_editor(&app, &e, id.into());
        }
        mensaje
    });
    conectar!(api, app, estado, on_elegir_importacion, |app, e| {
        if e.admin().is_err() {
            return;
        }
        let elegido = rfd::FileDialog::new()
            .set_title("Importar productos")
            .add_filter("Excel", &["xlsx", "xlsm", "xls", "ods"])
            .pick_file();
        let Some(ruta) = elegido else { return };
        let vista = importacion::leer_excel(&ruta).and_then(|renglones| e.bd.previsualizar_importacion(&renglones));
        match vista {
            Ok(vista) => {
                let (filas, resumen) = vista::importacion(&vista);
                let api = app.global::<Api>();
                api.set_importacion(filas);
                api.set_importacion_resumen(resumen.into());
                api.set_importando(true);
                e.importacion = Some(ruta);
            }
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });
    conectar!(api, app, estado, on_confirmar_importacion, |app, e| {
        let Some(ruta) = e.importacion.take() else { return };
        let resultado = importacion::leer_excel(&ruta).and_then(|renglones| e.bd.importar(&renglones));
        texto_de(
            &app,
            resultado.map(|v| format!("Listo: {} productos nuevos y {} actualizados.", v.crear, v.actualizar)),
        );
        app.global::<Api>().set_importando(false);
        refrescar_productos(&app, &e);
    });
    conectar!(api, app, estado, on_cerrar_importacion, |app, e| {
        e.importacion = None;
        app.global::<Api>().set_importando(false);
    });
    conectar!(api, app, estado, on_exportar_productos, |app, e| {
        let nombre = format!("productos-{}.xlsx", vista::hoy());
        if let Some(ruta) = guardar_como("Excel", "xlsx", &nombre) {
            let resultado =
                e.bd.buscar_productos("", false, 100_000)
                    .and_then(|productos| exportacion::productos_a_excel(&productos))
                    .and_then(|bytes| escribir(&ruta, bytes));
            texto_de(&app, resultado.map(|()| "Catálogo exportado."));
        }
    });

    // — Clientes —
    conectar!(api, app, estado, on_buscar_lista_clientes, |app, e, texto| {
        e.busqueda_clientes = texto.into();
        refrescar_clientes(&app, &e);
    });
    conectar!(api, app, estado, on_guardar_cliente, |app, e, cliente| {
        let datos = datos_cliente(&cliente);
        let resultado = match cliente.id {
            0 => e.bd.crear_cliente(&datos),
            id => e.bd.actualizar_cliente(id.into(), &datos),
        };
        let mensaje = texto_de(&app, resultado.map(|c| format!("{} guardado.", c.datos.nombre)));
        refrescar_clientes(&app, &e);
        mensaje
    });

    // — Reportes —
    conectar!(api, app, estado, on_cargar_reporte, |app, e, periodo, desde, hasta, cajero| {
        let resultado = rango(periodo, &desde, &hasta).and_then(|(desde, hasta)| {
            let cajero_id = e.cajeros.get(cajero as usize).copied().filter(|&id| id != 0);
            let filtro = Filtro { desde, hasta, cajero_id, cliente_id: None };
            let reporte = e.bd.reporte(&filtro, Seccion::TODOS)?;
            e.reporte = Some(filtro);
            Ok(reporte)
        });
        match resultado {
            Ok(reporte) => app.global::<Api>().set_reporte(vista::reporte(&reporte)),
            Err(error) => avisar(&app, error.to_string(), true),
        }
    });
    conectar!(api, app, estado, on_exportar_reporte, |app, e| {
        let Some(filtro) = e.reporte else { return };
        let nombre = format!("reporte-{}-al-{}.xlsx", filtro.desde, filtro.hasta);
        if let Some(ruta) = guardar_como("Excel", "xlsx", &nombre) {
            let resultado =
                e.bd.reporte(&filtro, Seccion::TODOS)
                    .and_then(|r| exportacion::reporte_a_excel(&r))
                    .and_then(|bytes| escribir(&ruta, bytes));
            texto_de(&app, resultado.map(|()| "Reporte exportado."));
        }
    });

    // — Ajustes —
    conectar!(api, app, estado, on_cargar_ajustes, |app, e| refrescar_ajustes(&app, &e));
    conectar!(api, app, estado, on_guardar_negocio, |app, e, negocio| {
        let resultado = e.admin().map(|_| ()).and_then(|()| {
            let datos = Negocio {
                nombre: negocio.nombre.into(),
                direccion: negocio.direccion.into(),
                telefono: negocio.telefono.into(),
                rfc: negocio.rfc.into(),
                tasa_iva: leer::<Porcentaje>(&negocio.iva)?,
                pie_ticket: negocio.pie.into(),
            };
            e.bd.guardar_negocio(&datos)
        });
        let ok = resultado.is_ok();
        let mensaje = texto_de(&app, resultado.map(|_| "Datos del negocio guardados."));
        if ok {
            refrescar_inicio(&app, &e);
        }
        mensaje
    });
    conectar!(api, app, estado, on_guardar_usuario, |app, e, id, nombre, admin, activo, pin| {
        let datos = DatosUsuario {
            nombre: nombre.into(),
            rol: if admin { Rol::Administrador } else { Rol::Cajero },
            activo: id == 0 || activo,
        };
        let resultado = e.admin().map(|_| ()).and_then(|()| match id {
            0 => e.bd.crear_usuario(&datos, &pin).map(drop),
            id => e.bd.actualizar_usuario(id.into(), &datos).and_then(|_| match pin.trim() {
                "" => Ok(()),
                pin => e.bd.cambiar_pin(id.into(), pin),
            }),
        });
        let mensaje = texto_de(&app, resultado.map(|()| "Usuario guardado."));
        refrescar_ajustes(&app, &e);
        mensaje
    });
    conectar!(api, app, estado, on_guardar_impresora, |app, e, impresora| {
        e.configuracion.impresora = Some(impresora.nombre.to_string()).filter(|n| !n.is_empty());
        e.configuracion.columnas = impresora.columnas.max(24) as usize;
        e.configuracion.abrir_cajon = impresora.abrir_cajon;
        e.configuracion.automatico = impresora.automatico;
        if let Err(error) = e.configuracion.guardar(&e.carpeta) {
            avisar(&app, format!("No se pudo guardar la configuración: {error}"), true);
        }
    });
    conectar!(api, app, estado, on_probar_impresora, |app, e| {
        let resultado = e.bd.negocio().map_err(|e| e.to_string()).and_then(|negocio| {
            let doc = impresion::prueba(&negocio, e.configuracion.columnas);
            imprimir::documento(&e.configuracion, &e.carpeta, "prueba", &doc, true)
        });
        match resultado {
            Ok(()) => avisar(&app, "Ticket de prueba enviado.", false),
            Err(error) => avisar(&app, error, true),
        }
    });
    conectar!(api, app, estado, on_respaldar, |app, e| {
        let nombre = format!("respaldo-punto-venta-{}.sqlite3", vista::hoy());
        if let Some(ruta) = guardar_como("Respaldo", "sqlite3", &nombre) {
            texto_de(&app, e.bd.respaldar(&ruta).map(|()| "Respaldo guardado."));
        }
    });
    conectar!(api, app, estado, on_restaurar, |app, e| {
        if e.admin().is_err() {
            return SharedString::from("Solo un administrador puede restaurar.");
        }
        let elegido = rfd::FileDialog::new()
            .set_title("Restaurar base de datos")
            .add_filter("Base de datos", &["sqlite3", "db"])
            .pick_file();
        let Some(origen) = elegido else { return SharedString::new() };
        let previo = e
            .carpeta
            .join("respaldos")
            .join(format!("antes-de-restaurar-{}.sqlite3", Local::now().format("%Y%m%d-%H%M%S")));
        match e.bd.restaurar(&origen, &previo) {
            Ok(()) => {
                // Los usuarios ahora son los del respaldo: hay que volver a entrar.
                e.sesion = None;
                e.venta_id = None;
                app.global::<Api>().set_hay_sesion(false);
                refrescar_inicio(&app, &e);
                avisar(&app, "Base restaurada. Lo anterior quedó guardado en la carpeta de respaldos.", false);
                SharedString::new()
            }
            Err(error) => error.to_string().into(),
        }
    });
    conectar!(api, app, estado, on_abrir_carpeta_datos, |app, e| {
        if let Err(error) = sistema::abrir(&e.carpeta) {
            avisar(&app, error, true);
        }
    });
}

// — Sesión —

fn configurar(e: &mut Estado, negocio: &str, nombre: &str, pin: &str) -> Resultado<()> {
    if e.bd.hay_usuarios()? {
        return Err(Error::invalido("La aplicación ya está configurada."));
    }
    let actual = e.bd.negocio()?;
    e.bd.guardar_negocio(&Negocio { nombre: negocio.into(), ..actual })?;
    let admin =
        e.bd.crear_usuario(&DatosUsuario { nombre: nombre.into(), rol: Rol::Administrador, activo: true }, pin)?;
    e.sesion = Some(admin);
    Ok(())
}

fn refrescar_inicio(app: &App, e: &Estado) {
    let api = app.global::<Api>();
    api.set_hay_usuarios(e.bd.hay_usuarios().unwrap_or(true));
    let usuarios = e.bd.usuarios().unwrap_or_default();
    api.set_usuarios_login(vista::modelo(usuarios.iter().filter(|u| u.activo).map(vista::usuario)));
    api.set_negocio_nombre(e.bd.negocio().map(|n| n.nombre).unwrap_or_default().into());
    api.set_fecha_larga(fecha_larga(vista::hoy()).into());
}

fn entrar_como(app: &App, e: &mut Estado) {
    let Some(usuario) = e.sesion.clone() else { return };
    let api = app.global::<Api>();
    api.set_usuario(usuario.nombre.into());
    api.set_es_admin(usuario.rol == Rol::Administrador);
    api.set_pantalla("venta".into());
    api.set_hay_sesion(true);
    e.venta_id = None;
    refrescar_venta(app, e);
    refrescar_cajeros(app, e);
}

// — Vender —

/// «3*martillo» → (3, «martillo»); «0.5 * clavo» → (0.5, «clavo»); «martillo» → (nada, «martillo»).
fn separar(texto: &str) -> (Option<Cantidad>, &str) {
    match texto.split_once('*') {
        Some((cuantos, resto)) => match cuantos.trim().parse::<Cantidad>() {
            Ok(cantidad) if cantidad.es_positivo() => (Some(cantidad), resto.trim()),
            _ => (None, texto.trim()),
        },
        None => (None, texto.trim()),
    }
}

fn venta_actual(e: &Estado) -> Resultado<i64> {
    e.venta_id.ok_or(Error::NoEncontrado("Venta"))
}

fn refrescar_venta(app: &App, e: &mut Estado) {
    let Ok(cajero) = e.usuario().map(|u| u.id) else { return };
    let venta = match e.venta_id {
        Some(id) => e.bd.venta(id).ok().filter(|v| v.estado == EstadoVenta::Abierta),
        None => None,
    };
    let venta = match venta {
        Some(venta) => Ok(venta),
        None => e.bd.venta_en_curso(cajero),
    };
    match venta {
        Ok(venta) => {
            e.venta_id = Some(venta.id);
            app.global::<Api>().set_venta(vista::venta(&venta, &[], |_| None));
        }
        Err(error) => avisar(app, error.to_string(), true),
    }
    app.global::<Api>().set_caja_abierta(e.bd.caja_abierta().map(|c| c.is_some()).unwrap_or(true));
    limpiar_resultados(app, e);
}

fn limpiar_resultados(app: &App, e: &mut Estado) {
    e.resultados.clear();
    app.global::<Api>().set_resultados(vista::modelo([]));
}

fn mostrar_venta(app: &App, e: &mut Estado, resultado: Resultado<nucleo::ventas::Venta>) {
    match resultado {
        Ok(venta) => app.global::<Api>().set_venta(vista::venta(&venta, &[], |_| None)),
        Err(error) => avisar(app, error.to_string(), true),
    }
    limpiar_resultados(app, e);
}

/// Se eligió un producto: si es a granel y no se dijo el peso, se pregunta; si tiene varios precios,
/// se pregunta cuál; si no, se agrega directo.
fn elegir_producto(app: &App, e: &mut Estado, producto: nucleo::catalogo::Producto, cantidad: Option<Cantidad>) {
    limpiar_resultados(app, e);
    if producto.granel && cantidad.is_none() {
        let api = app.global::<Api>();
        api.set_producto_pendiente(vista::producto(&producto));
        api.set_pide_peso(true);
        e.pendiente = Some(Pendiente { producto, cantidad, partida: None });
    } else if !producto.precios.is_empty() {
        preguntar_precio(app, e, Pendiente { producto, cantidad, partida: None });
    } else {
        agregar(app, e, producto.id, cantidad.unwrap_or(Cantidad::UNO), None);
    }
}

fn preguntar_precio(app: &App, e: &mut Estado, pendiente: Pendiente) {
    let producto = &pendiente.producto;
    let unidad = if producto.granel { "/kg" } else { "" };
    let normal = PrecioUi {
        id: 0,
        nombre: "Normal".into(),
        precio: format!("{}{unidad}", producto.precio.pesos()).into(),
        desde: SharedString::new(),
    };
    let otros = producto.precios.iter().map(|p| PrecioUi {
        id: p.id as i32,
        nombre: p.nombre.as_str().into(),
        precio: format!("{}{unidad}", p.precio.pesos()).into(),
        desde: p.cantidad_minima.map(|c| format!("desde {c}")).unwrap_or_default().into(),
    });
    // Se propone el mayoreo que alcanza la cantidad; si no, el normal. El cajero decide.
    let sugerido = pendiente
        .cantidad
        .and_then(|cantidad| producto.sugerido(cantidad))
        .and_then(|s| producto.precios.iter().position(|p| p.id == s.id))
        .map_or(0, |i| i as i32 + 1);
    let api = app.global::<Api>();
    api.set_producto_pendiente(vista::producto(producto));
    api.set_opciones_precio(vista::modelo(std::iter::once(normal).chain(otros)));
    api.set_precio_sugerido(sugerido);
    api.set_pide_precio(true);
    e.pendiente = Some(pendiente);
}

fn agregar(app: &App, e: &mut Estado, producto: i64, cantidad: Cantidad, precio: Option<i64>) {
    let resultado = venta_actual(e).and_then(|venta| e.bd.agregar(venta, producto, cantidad, precio));
    mostrar_venta(app, e, resultado);
}

fn cobrar(app: &App, e: &mut Estado, lineas: slint::ModelRc<CobroUi>, pin: &str) -> RespuestaUi {
    const METODOS: [Metodo; 3] = [Metodo::Efectivo, Metodo::Tarjeta, Metodo::Transferencia];
    let cobros: Resultado<Vec<Cobro>> = lineas
        .iter()
        .filter(|l| !l.monto.trim().is_empty() && l.monto.trim() != "0")
        .map(|l| Ok(Cobro { metodo: METODOS[l.metodo.clamp(0, 2) as usize], recibido: leer(&l.monto)? }))
        .collect();
    let cobros = match cobros {
        Ok(cobros) => cobros,
        Err(error) => return fallo(error),
    };
    let autorizacion = match pin {
        "" => None,
        pin => match e.bd.autorizar(pin) {
            Ok(permiso) => Some(permiso),
            Err(error) => return fallo(error),
        },
    };
    let resultado = venta_actual(e).and_then(|venta| e.bd.cobrar(venta, &cobros, autorizacion));
    match resultado {
        Ok(venta) => {
            let api = app.global::<Api>();
            api.set_cobrada(vista::venta(&venta, &[], |_| None));
            api.set_cambio(vista::pesos(venta.cambio()));
            api.set_mostrando_cobrada(true);
            e.cobrada = Some(venta.id);
            let en_efectivo = venta.pagos.iter().any(|p| p.metodo == Metodo::Efectivo);
            if e.configuracion.automatico {
                imprimir_ticket(app, e, venta.id, en_efectivo);
            } else if en_efectivo && e.configuracion.abrir_cajon {
                abrir_cajon(app, e);
            }
            e.venta_id = None;
            refrescar_venta(app, e);
            RespuestaUi { ok: true, ..Default::default() }
        }
        Err(error) => fallo(error),
    }
}

fn abrir_cajon(app: &App, e: &Estado) {
    if let Some(impresora) = &e.configuracion.impresora
        && let Err(error) = sistema::imprimir_crudo(impresora, &escpos::abrir_cajon())
    {
        avisar(app, error, true);
    }
}

fn imprimir_ticket(app: &App, e: &Estado, venta: i64, cajon: bool) {
    let resultado = e.bd.ticket(venta).map_err(|e| e.to_string()).and_then(|ticket| {
        let nombre = match ticket.venta.folio {
            Some(folio) => format!("ticket-{folio}"),
            None => format!("cotizacion-{}", ticket.venta.folio_cotizacion.unwrap_or_default()),
        };
        imprimir::documento(&e.configuracion, &e.carpeta, &nombre, &impresion::ticket(&ticket), cajon)
    });
    if let Err(error) = resultado {
        avisar(app, error, true);
    }
}

// — En pausa, historial, cotizaciones —

fn refrescar_pausadas(app: &App, e: &Estado) {
    let ventas = e.bd.ventas_pausadas().unwrap_or_default();
    app.global::<Api>().set_pausadas(vista::modelo(ventas.iter().map(|v| vista::venta(v, &[], |_| None))));
}

fn refrescar_historial(app: &App, e: &Estado, dia: NaiveDate) {
    let ventas = e.bd.historial(dia, dia).unwrap_or_default();
    let pagadas: Vec<_> = ventas.iter().filter(|v| v.estado == EstadoVenta::Pagada).collect();
    let total: Dinero = pagadas.iter().map(|v| v.totales.total).sum();
    let api = app.global::<Api>();
    api.set_historial_fecha(dia.to_string().into());
    api.set_historial_resumen(format!("{} · {} cobradas · {}", fecha_larga(dia), pagadas.len(), total.pesos()).into());
    api.set_historial(vista::modelo(ventas.iter().map(|v| vista::venta(v, &[], |_| None))));
}

fn refrescar_cotizaciones(app: &App, e: &Estado) {
    let palabras: Vec<String> = normalizar(&e.filtro_cotizaciones).split_whitespace().map(String::from).collect();
    let cotizaciones = e.bd.cotizaciones(300).unwrap_or_default();
    let visibles = cotizaciones.iter().filter(|c| {
        let texto = normalizar(&format!(
            "{} {} {}",
            c.folio_cotizacion.unwrap_or_default(),
            c.cliente.as_deref().unwrap_or_default(),
            c.nota
        ));
        palabras.iter().all(|p| texto.contains(p.as_str()))
    });
    app.global::<Api>().set_cotizaciones(vista::modelo(visibles.map(|c| vista::venta(c, &[], |_| None))));
}

fn mostrar_detalle(app: &App, e: &mut Estado, id: i64) {
    e.detalle = Some(id);
    e.devolucion.clear();
    refrescar_devolucion(app, e, true);
    app.global::<Api>().set_mostrando_detalle(true);
}

/// Vuelve a pintar el detalle con lo capturado para devolver. `modelo`: también las partidas (si se
/// pintaran en cada tecla, el campo que se está escribiendo perdería el cursor).
fn refrescar_devolucion(app: &App, e: &Estado, modelo: bool) {
    let Some(venta) = e.detalle.and_then(|id| e.bd.venta(id).ok()) else { return };
    let api = app.global::<Api>();
    if modelo {
        let devoluciones = e.bd.devoluciones_de(venta.id).unwrap_or_default();
        api.set_detalle(vista::venta(&venta, &devoluciones, |partida| e.devolucion.get(&partida).copied()));
    }
    let bruto: Dinero = venta.partidas.iter().map(|p| p.importe).sum();
    let importe: Dinero = venta.partidas.iter().filter_map(|p| e.devolucion.get(&p.id).map(|&c| c * p.precio)).sum();
    let estimado = if bruto.es_positivo() { venta.totales.total.proporcion(importe, bruto) } else { Dinero::CERO };
    api.set_devolucion_estimado(vista::pesos(estimado));
}

fn devolver(app: &App, e: &mut Estado, metodo: i32, motivo: &str, pin: &str) -> RespuestaUi {
    const METODOS: [Metodo; 3] = [Metodo::Efectivo, Metodo::Tarjeta, Metodo::Transferencia];
    let Some(venta) = e.detalle else { return fallo(Error::NoEncontrado("Venta")) };
    let lineas: Vec<ADevolver> =
        e.devolucion.iter().map(|(&partida_id, &cantidad)| ADevolver { partida_id, cantidad }).collect();
    let metodo = METODOS[metodo.clamp(0, 2) as usize];
    let resultado = autorizado(e, pin).and_then(|permiso| {
        let cajero = e.usuario()?.id;
        e.bd.devolver(venta, cajero, &lineas, metodo, motivo, permiso)
    });
    match resultado {
        Ok(devolucion) => {
            if let Ok(comprobante) = e.bd.comprobante_devolucion(devolucion.id) {
                let doc = impresion::devolucion(&comprobante);
                let cajon = metodo == Metodo::Efectivo;
                if let Err(error) = imprimir::documento(&e.configuracion, &e.carpeta, "devolucion", &doc, cajon) {
                    avisar(app, error, true);
                }
            }
            avisar(app, format!("Devolución registrada: se regresan {}.", devolucion.reembolso.pesos()), false);
            e.devolucion.clear();
            refrescar_devolucion(app, e, true);
            let dia = leer_fecha(&app.global::<Api>().get_historial_fecha()).unwrap_or(vista::hoy());
            refrescar_historial(app, e, dia);
            RespuestaUi { ok: true, ..Default::default() }
        }
        Err(error) => fallo(error),
    }
}

// — Caja —

fn refrescar_caja(app: &App, e: &Estado) {
    let api = app.global::<Api>();
    let abierta = e.bd.caja_abierta().ok().flatten();
    api.set_corte(match abierta.and_then(|caja| e.bd.corte(caja.id).ok()) {
        Some(corte) => vista::corte(&corte),
        None => vista::caja_cerrada(),
    });
    let anteriores = e.bd.cajas(60).unwrap_or_default();
    let cortes = anteriores.iter().filter(|c| c.cerrada_en.is_some()).filter_map(|c| e.bd.corte(c.id).ok());
    api.set_cortes(vista::modelo(cortes.map(|c| vista::corte(&c))));
}

// — Productos —

fn refrescar_productos(app: &App, e: &Estado) {
    let (texto, inactivos, surtir) = &e.busqueda_productos;
    let productos = if *surtir { e.bd.productos_en_alerta() } else { e.bd.buscar_productos(texto, !inactivos, 500) };
    app.global::<Api>().set_productos(vista::modelo(productos.unwrap_or_default().iter().map(vista::producto)));
}

fn refrescar_familias(app: &App, e: &Estado) {
    let familias = e.bd.familias().unwrap_or_default();
    let api = app.global::<Api>();
    api.set_familias(vista::modelo(familias.iter().map(vista::familia)));
    let nombres = std::iter::once("Sin familia".into()).chain(familias.iter().map(|f| f.nombre.as_str().into()));
    api.set_familias_nombres(vista::modelo(nombres));
}

fn abrir_editor(app: &App, e: &mut Estado, id: i64) {
    if let Err(error) = e.admin() {
        return avisar(app, error.to_string(), true);
    }
    refrescar_familias(app, e);
    let familias = e.bd.familias().unwrap_or_default();
    let editor = if id == 0 {
        e.editor_precios.clear();
        EditorProductoUi { activo: true, unidad: "pza".into(), ..Default::default() }
    } else {
        let Ok(p) = e.bd.producto(id) else { return };
        e.editor_precios = p
            .precios
            .iter()
            .map(|o| OtroPrecioTexto {
                nombre: o.nombre.clone(),
                precio: o.precio.to_string(),
                desde: o.cantidad_minima.map(|c| c.to_string()).unwrap_or_default(),
            })
            .collect();
        let texto_de = |c: Option<Cantidad>| c.map(|c| c.to_string()).unwrap_or_default();
        EditorProductoUi {
            id: p.id as i32,
            codigo: p.codigo.as_str().into(),
            nombre: p.nombre.as_str().into(),
            familia_indice: p
                .familia_id
                .and_then(|f| familias.iter().position(|x| x.id == f))
                .map_or(0, |i| i as i32 + 1),
            granel: p.granel,
            unidad: p.unidad.as_str().into(),
            precio: p.precio.to_string().into(),
            costo: p.costo.to_string().into(),
            stock_minimo: texto_de(p.stock_minimo).into(),
            stock_maximo: texto_de(p.stock_maximo).into(),
            alerta: texto_de(p.alerta_stock).into(),
            activo: p.activo,
            existencia: p.existencia.to_string().into(),
            precios: vista::modelo([]),
        }
    };
    let movimientos = if id == 0 { Vec::new() } else { e.bd.movimientos(id, 30).unwrap_or_default() };
    let api = app.global::<Api>();
    api.set_movimientos(vista::modelo(movimientos.iter().map(vista::movimiento)));
    api.set_editor(editor);
    mostrar_otros_precios(app, e);
    api.set_editando(true);
}

fn mostrar_otros_precios(app: &App, e: &Estado) {
    let api = app.global::<Api>();
    let mut editor = api.get_editor();
    editor.precios = vista::modelo(e.editor_precios.iter().map(|p| PrecioUi {
        id: 0,
        nombre: p.nombre.as_str().into(),
        precio: p.precio.as_str().into(),
        desde: p.desde.as_str().into(),
    }));
    api.set_editor(editor);
}

fn actualizar_existencia_editor(app: &App, e: &Estado, id: i64) {
    let api = app.global::<Api>();
    if let Ok(producto) = e.bd.producto(id) {
        let mut editor = api.get_editor();
        editor.existencia = producto.existencia.to_string().into();
        api.set_editor(editor);
    }
    let movimientos = e.bd.movimientos(id, 30).unwrap_or_default();
    api.set_movimientos(vista::modelo(movimientos.iter().map(vista::movimiento)));
    refrescar_productos(app, e);
}

fn datos_producto(e: &Estado, editor: &EditorProductoUi) -> Resultado<DatosProducto> {
    let familias = e.bd.familias()?;
    let familia_id = usize::try_from(editor.familia_indice - 1).ok().and_then(|i| familias.get(i)).map(|f| f.id);
    let precios = e
        .editor_precios
        .iter()
        .filter(|p| !(p.nombre.trim().is_empty() && p.precio.trim().is_empty()))
        .map(|p| {
            Ok(OtroPrecio {
                id: 0,
                nombre: p.nombre.trim().to_owned(),
                precio: leer(&p.precio)?,
                cantidad_minima: opcional(&p.desde)?,
            })
        })
        .collect::<Resultado<_>>()?;
    Ok(DatosProducto {
        codigo: editor.codigo.to_string(),
        nombre: editor.nombre.to_string(),
        familia_id,
        granel: editor.granel,
        unidad: editor.unidad.to_string(),
        precio: leer(if editor.precio.trim().is_empty() { "0" } else { &editor.precio })?,
        costo: leer(if editor.costo.trim().is_empty() { "0" } else { &editor.costo })?,
        stock_minimo: opcional(&editor.stock_minimo)?,
        stock_maximo: opcional(&editor.stock_maximo)?,
        alerta_stock: opcional(&editor.alerta)?,
        activo: editor.activo,
        precios,
    })
}

// — Clientes —

fn refrescar_clientes(app: &App, e: &Estado) {
    let clientes = e.bd.buscar_clientes(&e.busqueda_clientes, 500).unwrap_or_default();
    app.global::<Api>().set_lista_clientes(vista::modelo(clientes.iter().map(vista::cliente)));
}

fn datos_cliente(c: &ClienteUi) -> DatosCliente {
    DatosCliente {
        nombre: c.nombre.to_string(),
        telefono: c.telefono.to_string(),
        correo: c.correo.to_string(),
        rfc: c.rfc.to_string(),
        notas: c.notas.to_string(),
    }
}

// — Reportes —

/// Hoy, ayer, esta semana, este mes, mes pasado, este año u otro periodo (`desde` y `hasta`).
fn rango(periodo: i32, desde: &str, hasta: &str) -> Resultado<(NaiveDate, NaiveDate)> {
    let hoy = vista::hoy();
    let inicio_mes = hoy.with_day(1).expect("todo mes tiene día 1");
    Ok(match periodo {
        0 => (hoy, hoy),
        1 => {
            let ayer = hoy - Days::new(1);
            (ayer, ayer)
        }
        2 => (hoy - Days::new(hoy.weekday().num_days_from_monday().into()), hoy),
        3 => (inicio_mes, hoy),
        4 => (inicio_mes - Months::new(1), inicio_mes - Days::new(1)),
        5 => (NaiveDate::from_ymd_opt(hoy.year(), 1, 1).expect("1 de enero"), hoy),
        _ => (leer_fecha(desde)?, leer_fecha(hasta)?),
    })
}

// — Ajustes —

fn refrescar_ajustes(app: &App, e: &Estado) {
    let api = app.global::<Api>();
    if let Ok(n) = e.bd.negocio() {
        api.set_negocio(NegocioUi {
            nombre: n.nombre.into(),
            direccion: n.direccion.into(),
            telefono: n.telefono.into(),
            rfc: n.rfc.into(),
            iva: n.tasa_iva.to_string().into(),
            pie: n.pie_ticket.into(),
        });
    }
    let usuarios = e.bd.usuarios().unwrap_or_default();
    api.set_usuarios(vista::modelo(usuarios.iter().map(vista::usuario)));

    let impresoras = sistema::impresoras();
    let elegida = e.configuracion.impresora.as_ref().and_then(|n| impresoras.iter().position(|i| i == n));
    let nombres = std::iter::once("Sin impresora térmica: tickets en PDF".into())
        .chain(impresoras.iter().map(|i| i.as_str().into()));
    api.set_impresoras(vista::modelo(nombres));
    api.set_impresora_indice(elegida.map_or(0, |i| i as i32 + 1));
    api.set_impresora(ImpresoraUi {
        nombre: e.configuracion.impresora.clone().unwrap_or_default().into(),
        columnas: e.configuracion.columnas as i32,
        abrir_cajon: e.configuracion.abrir_cajon,
        automatico: e.configuracion.automatico,
    });
    api.set_carpeta_datos(e.carpeta.display().to_string().into());
}

/// Para el filtro de reportes: «Todos» y luego cada usuario.
fn refrescar_cajeros(app: &App, e: &mut Estado) {
    let usuarios = e.bd.usuarios().unwrap_or_default();
    e.cajeros = std::iter::once(0).chain(usuarios.iter().map(|u| u.id)).collect();
    let nombres =
        std::iter::once("Todos los empleados".into()).chain(usuarios.iter().map(|u| u.nombre.as_str().into()));
    app.global::<Api>().set_cajeros_filtro(vista::modelo(nombres));
}

// — Utilidades —

fn autorizado(e: &Estado, pin: &str) -> Resultado<Autorizacion> {
    e.bd.autorizar(pin)
}

/// Un problema de PIN (o que falte autorización) pide el PIN de un administrador; cualquier otro error
/// se muestra tal cual.
fn fallo(error: Error) -> RespuestaUi {
    let autorizar = matches!(error, Error::PinIncorrecto | Error::BajoCosto(_));
    RespuestaUi { ok: false, autorizar, mensaje: error.to_string().into() }
}

/// Muestra el éxito como aviso y devuelve el error como texto (vacío si salió bien).
fn texto_de<T: ToString>(app: &App, resultado: Resultado<T>) -> SharedString {
    match resultado {
        Ok(mensaje) => {
            avisar(app, mensaje.to_string(), false);
            SharedString::new()
        }
        Err(error) => {
            let texto = error.to_string();
            avisar(app, texto.clone(), true);
            texto.into()
        }
    }
}

pub fn avisar(app: &App, texto: impl Into<String>, error: bool) {
    let texto: SharedString = texto.into().into();
    let api = app.global::<Api>();
    api.set_aviso(texto.clone());
    api.set_aviso_error(error);
    let ui = app.as_weak();
    Timer::single_shot(Duration::from_secs(if error { 6 } else { 3 }), move || {
        if let Some(app) = ui.upgrade() {
            let api = app.global::<Api>();
            if api.get_aviso() == texto {
                api.set_aviso(SharedString::new());
            }
        }
    });
}

fn leer<T: std::str::FromStr<Err = Error>>(texto: &str) -> Resultado<T> {
    texto.parse()
}

fn opcional<T: std::str::FromStr<Err = Error>>(texto: &str) -> Resultado<Option<T>> {
    if texto.trim().is_empty() { Ok(None) } else { texto.parse().map(Some) }
}

fn positiva(cantidad: Cantidad) -> Resultado<Cantidad> {
    if cantidad.es_positivo() { Ok(cantidad) } else { Err(Error::invalido("La cantidad debe ser mayor que cero.")) }
}

fn leer_fecha(texto: &str) -> Resultado<NaiveDate> {
    let texto = texto.trim();
    NaiveDate::parse_from_str(texto, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(texto, "%d/%m/%Y"))
        .map_err(|_| Error::invalido(format!("«{texto}» no es una fecha (usa AAAA-MM-DD o DD/MM/AAAA).")))
}

fn fecha_larga(dia: NaiveDate) -> String {
    const DIAS: [&str; 7] = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
    const MESES: [&str; 12] = [
        "enero",
        "febrero",
        "marzo",
        "abril",
        "mayo",
        "junio",
        "julio",
        "agosto",
        "septiembre",
        "octubre",
        "noviembre",
        "diciembre",
    ];
    let dia_semana = DIAS[dia.weekday().num_days_from_monday() as usize];
    format!("{dia_semana} {} de {} de {}", dia.day(), MESES[dia.month0() as usize], dia.year())
}

fn guardar_como(tipo: &str, extension: &str, nombre: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter(tipo, &[extension]).set_file_name(nombre).save_file()
}

fn escribir(ruta: &std::path::Path, bytes: Vec<u8>) -> Resultado<()> {
    std::fs::write(ruta, bytes).map_err(|e| Error::invalido(format!("No se pudo guardar {}: {e}", ruta.display())))
}

/// Para el arranque: respaldo diario sin estorbar si falla.
pub fn respaldo_diario(e: &Estado) {
    if let Err(error) = respaldos::diario(&e.bd, &e.carpeta.join("respaldos")) {
        eprintln!("No se pudo hacer el respaldo diario: {error}");
    }
}
