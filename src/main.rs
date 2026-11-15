#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app_assets;
mod desktop;
mod i18n;
mod native_prompt;
mod shell;
mod single_instance;
mod startup;
mod store;
mod themes;
mod update;
mod view;
mod window_placement;

use gpui_kit::*;
use std::path::PathBuf;

pub(crate) const APP_NAME: &str = "Codex Switch";
pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const APP_REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

pub(crate) fn app_title() -> String {
    format!("{APP_NAME} - {APP_VERSION}")
}

fn config_directory() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    if let Some(arg) = args.next() {
        anyhow::ensure!(
            arg == "--codex-dir",
            crate::tr!("main.usage_codex_switch_codex_dir_directory")
        );
        let path = args.next().map(PathBuf::from).ok_or_else(|| {
            anyhow::anyhow!(crate::tr!("main.codex_dir_requires_a_directory_argument"))
        })?;
        anyhow::ensure!(args.next().is_none(), crate::tr!("main.too_many_arguments"));
        return Ok(std::path::absolute(path)?);
    }
    // Use the user's Codex directory unless an explicit directory was supplied.
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| {
            anyhow::anyhow!(crate::tr!(
                "main.could_not_find_your_home_directory_specify_a_directory"
            ))
        })?;
    Ok(PathBuf::from(home).join(".codex"))
}

fn main() -> anyhow::Result<()> {
    i18n::set_language(i18n::Language::detect());
    let Some(instance) = single_instance::SingleInstance::acquire()? else {
        return Ok(());
    };
    let activation_requests = instance.activation_requests();
    let store = store::Store::new(config_directory()?);
    i18n::set_language(store.initialize_language()?);
    // Windows otherwise quits when its last window closes, even if a tray icon
    // and application-owned state are still alive. Debug and release share this.
    let app = application()
        .with_assets(app_assets::AppAssets)
        .with_quit_mode(QuitMode::Explicit);
    app.on_reopen(|cx| {
        shell::show_main(cx);
    });
    app.run(move |cx| {
        init(cx);
        #[cfg(target_os = "macos")]
        app_assets::set_macos_application_icon();
        view::bind_keys(cx);
        shell::start(store, cx);
        cx.spawn(async move |cx| {
            while activation_requests.recv().await.is_ok() {
                cx.update(|cx| {
                    shell::show_main(cx);
                });
            }
        })
        .detach();
    });
    drop(instance);
    Ok(())
}
