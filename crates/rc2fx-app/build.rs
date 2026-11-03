fn main() {
    // GPUI's debug render/layout path exceeds Windows' default 1 MiB main
    // stack when the full EQ editor is visible. Reserve 8 MiB of virtual
    // address space; pages are committed on demand by Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let flag = match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => "/STACK:8388608",
            Ok("gnu") => "-Wl,--stack,8388608",
            _ => return,
        };
        println!("cargo:rustc-link-arg-bin=rc2fx={flag}");
    }
}
