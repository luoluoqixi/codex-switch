#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app_assets;
mod desktop;
mod native_prompt;
mod shell;
mod startup;
mod store;
mod themes;
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
            "用法：codex-switch [--codex-dir <目录>]"
        );
        let path = args
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("--codex-dir 需要目录参数"))?;
        anyhow::ensure!(args.next().is_none(), "参数过多");
        return Ok(std::path::absolute(path)?);
    }
    // Use the user's Codex directory unless an explicit directory was supplied.
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| anyhow::anyhow!("无法获取用户主目录，请使用 --codex-dir 指定目录"))?;
    Ok(PathBuf::from(home).join(".codex"))
}

fn main() -> anyhow::Result<()> {
    let store = store::Store::new(config_directory()?);
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
    });
    Ok(())
}
