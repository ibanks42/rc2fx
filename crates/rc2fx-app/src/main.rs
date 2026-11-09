#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    // First: Velopack runs RC2FX with special arguments while installing,
    // updating and uninstalling, then exits. Updates apply only when the
    // user chooses "Restart to update", never at the next launch by itself.
    let mut app = velopack::VelopackApp::build().set_auto_apply_on_startup(false);
    // The same private packages folder the app downloads updates into.
    if let Some(locator) = rc2fx_app::velopack_locator() {
        app = app.set_locator(locator);
    }
    #[cfg(target_os = "windows")]
    {
        app = app.on_before_uninstall_fast_callback(|_| rc2fx_app::uninstall::offer_cleanup());
    }
    app.run();
    rc2fx_installer::payload_mode();
    rc2fx_app::run();
}
