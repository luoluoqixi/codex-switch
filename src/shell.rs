//! Application lifetime is independent of the main window. Native tray/menu
//! callbacks only enqueue commands; GPUI handles them on its foreground thread.
use crate::{store::Store, view::Switcher};
use gpui_kit::*;

struct AppShell {
    owner: Entity<Switcher>,
    main_window: AnyWindowHandle,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    tray: Option<tray_icon::TrayIcon>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    tray_profiles: Vec<TrayProfile>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    tray_themes: Vec<TrayTheme>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    _tray_observer: Option<Subscription>,
}
impl Global for AppShell {}

enum Command {
    Show,
    Settings,
    RestartChatGpt,
    SwitchProfile(String),
    SwitchTheme(Option<String>),
    Quit,
}

pub(crate) fn start(store: Store, cx: &mut App) {
    let launch_hidden = cfg!(any(target_os = "windows", target_os = "macos"))
        && store.start_hidden_to_tray().unwrap_or(false);
    let (main_window, owner) = open_window(main_options(cx, !launch_hidden), cx, |window, cx| {
        cx.new(|cx| Switcher::new(store, window, cx))
    })
    .expect(crate::tr!("shell.could_not_open_the_codex_switch_window"));
    attach_main_close(main_window, &owner, cx);
    cx.set_global(AppShell {
        owner,
        main_window,
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        tray: None,
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        tray_profiles: Vec::new(),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        tray_themes: Vec::new(),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        _tray_observer: None,
    });
    // On macOS NSStatusItem must be created after the app event loop starts.
    cx.defer(install_tray);
    cx.activate(!launch_hidden);
}

fn main_options(cx: &App, show: bool) -> WindowOptions {
    let display_id = crate::window_placement::current_display_id(cx);
    WindowOptions {
        show,
        focus: show,
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            display_id,
            size(px(800.), px(480.)),
            cx,
        ))),
        window_min_size: Some(size(px(640.), px(380.))),
        display_id,
        titlebar: Some(TitlebarOptions {
            title: Some(crate::app_title().into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// The handle may outlive the native window; callers validate it with update_window.
pub(crate) fn main_window_handle(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<AppShell>().map(|shell| shell.main_window)
}

fn attach_main_close(handle: AnyWindowHandle, owner: &Entity<Switcher>, cx: &mut App) {
    let owner = owner.downgrade();
    let _ = cx.update_window(handle, |_, window, cx| {
        window.on_window_should_close(cx, move |window, cx| {
            let Some(owner) = owner.upgrade() else {
                return true;
            };
            if owner.read(cx).operation_pending() {
                return false;
            }
            if owner.read(cx).exit_on_close() || !tray_available(cx) {
                owner.update(cx, |owner, cx| owner.request_exit(window, cx));
                false
            } else {
                true
            }
        });
    });
}

fn tray_available(cx: &App) -> bool {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        cx.try_global::<AppShell>()
            .is_some_and(|shell| shell.tray.is_some())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = cx;
        false
    }
}

pub(crate) fn show_main(cx: &mut App) -> Option<AnyWindowHandle> {
    let shell = cx.try_global::<AppShell>()?;
    let handle = shell.main_window;
    let owner = shell.owner.clone();
    if cx
        .update_window(handle, |_, window, _| window.activate_window())
        .is_ok()
    {
        cx.activate(true);
        return Some(handle);
    }
    let view = owner.clone();
    // Opening a window renders synchronously; never call under owner.update.
    match open_window(main_options(cx, true), cx, move |window, cx| {
        let focus = view.read(cx).main_focus();
        focus.focus(window, cx);
        view
    }) {
        Ok((handle, _)) => {
            attach_main_close(handle, &owner, cx);
            cx.global_mut::<AppShell>().main_window = handle;
            cx.activate(true);
            Some(handle)
        }
        Err(error) => {
            eprintln!(
                "{}",
                crate::tr!("shell.could_not_show_the_main_window"; error = format!("{:#}", error))
            );
            None
        }
    }
}

fn dispatch(command: Command, cx: &mut App) {
    let owner = cx.global::<AppShell>().owner.clone();
    if matches!(command, Command::Settings) {
        owner.update(cx, |owner, cx| owner.show_settings(cx));
        return;
    }
    if let Command::SwitchTheme(id) = command {
        owner.update(cx, |owner, cx| owner.select_theme_from_tray(id, cx));
        return;
    }
    if matches!(command, Command::RestartChatGpt | Command::SwitchProfile(_)) {
        let origin = main_window_handle(cx).filter(|handle| {
            cx.update_window(*handle, |_, window, _| window.is_visible())
                .unwrap_or(false)
        });
        owner.update(cx, |owner, cx| match command {
            Command::RestartChatGpt => owner.request_desktop_action_at(
                crate::desktop::DesktopApp::ChatGpt,
                crate::desktop::AppAction::Restart,
                origin,
                cx,
            ),
            Command::SwitchProfile(name) => owner.confirm_switch_at(name, None, origin, cx),
            _ => unreachable!(),
        });
        return;
    }
    if matches!(command, Command::Quit)
        && !owner.read(cx).operation_pending()
        && !owner.read(cx).exit_needs_confirmation(cx)
    {
        owner.update(cx, |owner, cx| owner.shutdown_and_quit(cx));
        return;
    }
    let Some(handle) = show_main(cx) else {
        return;
    };
    let _ = cx.update_window(handle, |_, window, cx| {
        owner.update(cx, |owner, cx| match command {
            Command::Show => {}
            Command::Settings => owner.show_settings(cx),
            Command::RestartChatGpt | Command::SwitchProfile(_) | Command::SwitchTheme(_) => {
                unreachable!()
            }
            Command::Quit => owner.request_exit(window, cx),
        });
    });
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
const SWITCH_PROFILE_PREFIX: &str = "switch-profile:";

#[cfg(any(target_os = "windows", target_os = "macos"))]
const SWITCH_THEME_PREFIX: &str = "switch-theme:";

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(PartialEq, Eq)]
struct TrayTheme {
    id: Option<String>,
    name: String,
    current: bool,
    enabled: bool,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn tray_themes(cx: &App) -> Vec<TrayTheme> {
    let owner = cx.global::<AppShell>().owner.read(cx);
    let snapshot = owner.theme_snapshot();
    let enabled = !owner.operation_pending();
    let mut themes = vec![TrayTheme {
        id: None,
        name: crate::tr!("shell.theme_off").into(),
        current: snapshot.selected.is_none(),
        enabled: enabled && snapshot.selected.is_some(),
    }];
    themes.extend(snapshot.themes.iter().map(|theme| {
        let current = snapshot.selected.as_ref() == Some(&theme.id);
        TrayTheme {
            id: Some(theme.id.clone()),
            name: theme.name.clone(),
            current,
            enabled: enabled && !current,
        }
    }));
    themes
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(PartialEq, Eq)]
struct TrayProfile {
    key: String,
    name: String,
    current: bool,
    enabled: bool,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn tray_profiles(cx: &App) -> Vec<TrayProfile> {
    let owner = cx.global::<AppShell>().owner.read(cx);
    let mut profiles: Vec<_> = owner
        .profiles()
        .iter()
        .map(|profile| TrayProfile {
            key: profile.key.clone(),
            name: profile.name.clone(),
            current: profile.is_current,
            enabled: owner.can_switch_profile(&profile.key),
        })
        .collect();
    profiles.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.key.cmp(&b.key))
    });
    profiles
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn tray_menu(
    profiles: &[TrayProfile],
    themes: &[TrayTheme],
) -> anyhow::Result<tray_icon::menu::Menu> {
    use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

    let menu = Menu::new();
    let show = MenuItem::with_id("show", crate::tr!("shell.show_main_window"), true, None);
    let switch = Submenu::new(crate::tr!("shell.switch_profile"), true);
    if profiles.is_empty() {
        switch.append(&MenuItem::new(crate::tr!("shell.no_profiles"), false, None))?;
    } else {
        for profile in profiles {
            let name = if cfg!(target_os = "windows") {
                profile.name.replace('&', "&&")
            } else {
                profile.name.clone()
            };
            switch.append(&CheckMenuItem::with_id(
                format!("{SWITCH_PROFILE_PREFIX}{}", profile.key),
                name,
                profile.enabled,
                profile.current,
                None,
            ))?;
        }
    }
    let theme_menu = Submenu::new(crate::tr!("shell.switch_theme"), true);
    for theme in themes {
        let id = theme
            .id
            .as_ref()
            .map(|id| format!("{SWITCH_THEME_PREFIX}{id}"))
            .unwrap_or_else(|| "theme-off".into());
        let name = if cfg!(target_os = "windows") {
            theme.name.replace('&', "&&")
        } else {
            theme.name.clone()
        };
        theme_menu.append(&CheckMenuItem::with_id(
            id,
            name,
            theme.enabled,
            theme.current,
            None,
        ))?;
    }
    if themes.iter().all(|theme| theme.id.is_none()) {
        theme_menu.append(&PredefinedMenuItem::separator())?;
        theme_menu.append(&MenuItem::new(crate::tr!("shell.no_themes"), false, None))?;
    }
    menu.append_items(&[
        &show,
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(
            "restart-chatgpt",
            crate::tr!("shell.restart_chatgpt"),
            true,
            None,
        ),
        &PredefinedMenuItem::separator(),
        &switch,
        &theme_menu,
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("settings", crate::tr!("shell.settings"), true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("quit", crate::tr!("shell.quit_codex_switch"), true, None),
    ])?;
    Ok(menu)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn refresh_tray_menu(cx: &mut App) {
    let profiles = tray_profiles(cx);
    let themes = tray_themes(cx);
    let shell = cx.global_mut::<AppShell>();
    if shell.tray_profiles == profiles && shell.tray_themes == themes {
        return;
    }
    if let Some(tray) = &shell.tray {
        match tray_menu(&profiles, &themes) {
            Ok(menu) => {
                tray.set_menu(Some(Box::new(menu)));
                shell.tray_profiles = profiles;
                shell.tray_themes = themes;
            }
            Err(error) => eprintln!(
                "{}",
                crate::tr!("shell.could_not_update_the_tray_menu"; error = format!("{:#}", error))
            ),
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn install_tray(cx: &mut App) {
    use tray_icon::{
        MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent, menu::MenuEvent,
    };
    let profiles = tray_profiles(cx);
    let themes = tray_themes(cx);
    let build = || -> anyhow::Result<_> {
        let menu = tray_menu(&profiles, &themes)?;
        let image = image::load_from_memory(include_bytes!("../assets/icon.png"))?
            .resize(32, 32, image::imageops::FilterType::Lanczos3)
            .into_rgba8();
        let icon = tray_icon::Icon::from_rgba(image.into_raw(), 32, 32)?;
        Ok(TrayIconBuilder::new()
            .with_id("codex-switch")
            .with_tooltip(crate::app_title())
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(true)
            .build()?)
    };
    match build() {
        Ok(tray) => {
            cx.global_mut::<AppShell>().tray = Some(tray);
            cx.global_mut::<AppShell>().tray_profiles = profiles;
            cx.global_mut::<AppShell>().tray_themes = themes;
            let owner = cx.global::<AppShell>().owner.clone();
            let observer = cx.observe(&owner, |_, cx| refresh_tray_menu(cx));
            cx.global_mut::<AppShell>()._tray_observer = Some(observer);
            let (sender, receiver) = async_channel::unbounded();
            let click_sender = sender.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if matches!(event, TrayIconEvent::Click {
                    ref id,
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } if id.0 == "codex-switch")
                {
                    // Dispatch once on release, outside the native callback and
                    // any view lease, using the same path as “显示主窗口”.
                    let _ = click_sender.try_send(Command::Show);
                }
            }));
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let command = match event.id.0.as_str() {
                    "show" => Command::Show,
                    "settings" => Command::Settings,
                    "restart-chatgpt" => Command::RestartChatGpt,
                    "quit" => Command::Quit,
                    "theme-off" => Command::SwitchTheme(None),
                    id if id.starts_with(SWITCH_THEME_PREFIX) => {
                        Command::SwitchTheme(Some(id[SWITCH_THEME_PREFIX.len()..].to_owned()))
                    }
                    id if id.starts_with(SWITCH_PROFILE_PREFIX) => {
                        Command::SwitchProfile(id[SWITCH_PROFILE_PREFIX.len()..].to_owned())
                    }
                    _ => return,
                };
                let _ = sender.try_send(command);
            }));
            cx.spawn(async move |cx| {
                while let Ok(command) = receiver.recv().await {
                    cx.update(|cx| dispatch(command, cx));
                }
            })
            .detach();
        }
        Err(error) => {
            let owner = cx.global::<AppShell>().owner.clone();
            owner.update(cx, |owner, cx| {
                owner.report_shell_error(
                    crate::tr!("shell.the_tray_could_not_be_initialized_closing_the_main"; error = format!("{:#}", error)),
                    cx,
                )
            });
            let handle = cx.global::<AppShell>().main_window;
            let visible = cx
                .update_window(handle, |_, window, _| window.is_visible())
                .unwrap_or(false);
            if !visible {
                show_main(cx);
            }
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn install_tray(_: &mut App) {}
