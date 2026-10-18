fn main() {
    // Windows 7: las DLL que llegaron con Windows 8 se cargan hasta que se usan, y ahí `src/win7.rs` pone en su
    // lugar lo que sí existe (ver ese módulo).
    if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("win7") {
        for dll in ["combase.dll", "api-ms-win-core-winrt-error-l1-1-0.dll"] {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    let configuracion = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", configuracion).expect("la interfaz no compila");
}
