#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod store;
mod view;

use gpui_kit::*;
use std::path::PathBuf;

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
    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);
        view::bind_keys(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // Window bounds are platform geometry; all content uses rem-based sizing.
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(760.), px(480.)),
                cx,
            ))),
            window_min_size: Some(size(px(640.), px(380.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Codex Switch".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        open_window(options, cx, |window, cx| {
            cx.new(|cx| view::Switcher::new(store, window, cx))
        })
        .expect("无法打开 Codex Switch 窗口");
        cx.activate(true);
    });
    Ok(())
}
