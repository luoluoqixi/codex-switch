mod desktop_controls;
mod dialogs;

use crate::desktop::{self, AppAction, AppStatus, DesktopApp};
use crate::store::{Profile, Settings, Snapshot, Store};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
    dialog::DialogFooter,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    scroll::Scrollbar,
};
use gpui_kit::*;
use std::{cell::Cell, path::PathBuf, time::Duration};

#[cfg(target_os = "macos")]
const COMMAND_KEYS: [&str; 3] = ["cmd-r", "cmd-b", "cmd-n"];
#[cfg(not(target_os = "macos"))]
const COMMAND_KEYS: [&str; 3] = ["ctrl-r", "ctrl-b", "ctrl-n"];
#[cfg(target_os = "macos")]
const SHORTCUTS: &str = "↑ ↓ 选择 · Enter 切换 · ⌘R 刷新 · ⌘B 保存 · ⌘N 新建";
#[cfg(not(target_os = "macos"))]
const SHORTCUTS: &str = "↑ ↓ 选择 · Enter 切换 · Ctrl+R 刷新 · Ctrl+B 保存 · Ctrl+N 新建";

actions!(
    switcher,
    [
        Refresh,
        SaveBackup,
        NewBackup,
        PreviousProfile,
        NextProfile,
        SwitchProfile
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(COMMAND_KEYS[0], Refresh, Some("Switcher")),
        KeyBinding::new("f5", Refresh, Some("Switcher")),
        KeyBinding::new(COMMAND_KEYS[1], SaveBackup, Some("Switcher")),
        KeyBinding::new(COMMAND_KEYS[2], NewBackup, Some("Switcher")),
        KeyBinding::new("up", PreviousProfile, Some("Profiles")),
        KeyBinding::new("down", NextProfile, Some("Profiles")),
        KeyBinding::new("enter", SwitchProfile, Some("Profiles")),
    ]);
}

enum Operation {
    Refresh,
    Backup(String),
    Create(String),
    Switch(String),
    Open(String),
    OpenVersion(String, String),
    Settings(Settings),
    SwitchVersion(String, String),
    DeleteVersion(String, String),
    DeleteProfile(String),
    Desktop(DesktopApp, AppAction),
    OpenInstallation(DesktopApp),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ProfileOrder {
    #[default]
    Newest,
    Oldest,
    Name,
}

impl ProfileOrder {
    fn label(self) -> &'static str {
        match self {
            Self::Newest => "最新备份优先",
            Self::Oldest => "最早备份优先",
            Self::Name => "名称 A–Z",
        }
    }

    fn apply(self, profiles: &mut [Profile]) {
        profiles.sort_by(|a, b| {
            let time = match self {
                Self::Newest => b.latest_stamp.cmp(&a.latest_stamp),
                Self::Oldest => match (a.latest_stamp, b.latest_stamp) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
                Self::Name => std::cmp::Ordering::Equal,
            };
            time.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.key.cmp(&b.key))
        });
    }
}

#[derive(Clone)]
enum Modal {
    New,
    Settings,
    Versions(String),
    Delete(String),
}

#[derive(Default)]
struct Outcome {
    message: String,
    selected: Option<String>,
    reveal: Option<PathBuf>,
}

pub(crate) struct Switcher {
    store: Store,
    snapshot: Snapshot,
    loaded: bool,
    busy: bool,
    selected: Option<String>,
    new_name: Entity<InputState>,
    profile_order: ProfileOrder,
    version_limit: Entity<InputState>,
    app_paths: [Entity<InputState>; 1],
    app_statuses: Vec<AppStatus>,
    app_error: String,
    checking_apps: bool,
    app_revision: u64,
    _app_monitor: Option<Task<()>>,
    modal: Option<Modal>,
    modal_revision: u64,
    pending_delete: Option<String>,
    dialog_error: String,
    list_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    version_scroll: UniformListScrollHandle,
    message: String,
    is_error: bool,
    _subscriptions: Vec<Subscription>,
}

impl Switcher {
    pub fn new(store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("例如：官方、工作、备用"));
        let subscription = cx.subscribe_in(&new_name, window, |this, _, event, window, cx| {
            if matches!(this.modal, Some(Modal::New))
                && matches!(event, InputEvent::PressEnter { .. })
            {
                this.create_profile(window, cx);
            }
            if matches!(event, InputEvent::Change) {
                this.dialog_error.clear();
                cx.notify();
                window.refresh();
            }
        });
        let version_limit = cx.new(|cx| InputState::new(window, cx));
        let settings_subscription =
            cx.subscribe_in(&version_limit, window, |this, _, event, window, cx| {
                if matches!(this.modal, Some(Modal::Settings))
                    && matches!(event, InputEvent::PressEnter { .. })
                {
                    this.save_settings(window, cx);
                }
                if matches!(event, InputEvent::Change) {
                    this.dialog_error.clear();
                    cx.notify();
                    window.refresh();
                }
            });
        let list_focus = cx.focus_handle().tab_stop(true);
        let app_paths = std::array::from_fn(|_| cx.new(|cx| InputState::new(window, cx)));
        let mut subscriptions = vec![subscription, settings_subscription];
        for input in &app_paths {
            subscriptions.push(
                cx.subscribe_in(input, window, |this, _, event, window, cx| {
                    if matches!(this.modal, Some(Modal::Settings))
                        && matches!(event, InputEvent::PressEnter { .. })
                    {
                        this.save_settings(window, cx);
                    }
                    if matches!(event, InputEvent::Change) {
                        this.dialog_error.clear();
                        cx.notify();
                        window.refresh();
                    }
                }),
            );
        }
        window.focus(&list_focus, cx);
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            view.update(cx, |this, cx| {
                if this.busy {
                    this.message = "正在处理操作，请等待完成后再关闭窗口。".into();
                    cx.notify();
                }
                !this.busy
            })
            .unwrap_or(true)
        });
        let mut this = Self {
            store,
            snapshot: Snapshot::default(),
            loaded: false,
            busy: false,
            selected: None,
            new_name,
            profile_order: ProfileOrder::default(),
            version_limit,
            app_paths,
            app_statuses: Vec::new(),
            app_error: String::new(),
            checking_apps: false,
            app_revision: 0,
            _app_monitor: None,
            modal: None,
            modal_revision: 0,
            pending_delete: None,
            dialog_error: String::new(),
            list_focus,
            scroll: UniformListScrollHandle::new(),
            version_scroll: UniformListScrollHandle::new(),
            message: "正在读取配置…".into(),
            is_error: false,
            _subscriptions: subscriptions,
        };
        this._app_monitor = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                if this.update(cx, |this, cx| this.refresh_apps(cx)).is_err() {
                    break;
                }
            }
        }));
        this.run(Operation::Refresh, window, cx);
        this
    }

    fn run(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.app_revision += 1;
        self.is_error = false;
        let close_on_success = matches!(
            operation,
            Operation::Create(_) | Operation::Settings(_) | Operation::DeleteProfile(_)
        );
        self.dialog_error.clear();
        self.message = match &operation {
            Operation::Refresh => "正在读取配置…".into(),
            Operation::Backup(name) => format!("正在保存到 {name}…"),
            Operation::Create(_) => "正在新建备份…".into(),
            Operation::Switch(_) => "正在备份并切换配置…".into(),
            Operation::Open(_) => "正在打开目录…".into(),
            Operation::OpenVersion(_, _) => "正在打开版本目录…".into(),
            Operation::Settings(_) => "正在保存设置并清理旧版本…".into(),
            Operation::SwitchVersion(_, _) => "正在备份并切换到所选版本…".into(),
            Operation::DeleteVersion(_, _) => "正在删除备份版本…".into(),
            Operation::DeleteProfile(_) => "正在删除备份目录…".into(),
            Operation::Desktop(app, action) => format!("正在{} {}…", action.label(), app.name()),
            Operation::OpenInstallation(_) => "正在定位 ChatGPT 应用…".into(),
        };
        let store = self.store.clone();
        let app_paths = self.snapshot.settings.app_paths();
        let job = cx.background_spawn(async move {
            let result = match operation {
                Operation::Refresh => Ok(Outcome::default()),
                Operation::Backup(name) => store.backup(&name).map(|message| Outcome {
                    message,
                    selected: Some(name),
                    ..Default::default()
                }),
                Operation::Create(name) => store.create_profile(&name).map(|message| Outcome {
                    message,
                    selected: Some(name.trim().into()),
                    ..Default::default()
                }),
                Operation::Switch(name) => store.switch(&name).map(|message| Outcome {
                    message,
                    selected: Some(name),
                    ..Default::default()
                }),
                Operation::Open(name) => store.profile_directory(&name).map(|path| Outcome {
                    reveal: Some(path),
                    ..Default::default()
                }),
                Operation::OpenVersion(name, id) => {
                    store.version_directory(&name, &id).map(|path| Outcome {
                        reveal: Some(path),
                        ..Default::default()
                    })
                }
                Operation::Settings(settings) => {
                    store.save_settings(&settings).map(|message| Outcome {
                        message,
                        ..Default::default()
                    })
                }
                Operation::SwitchVersion(name, id) => {
                    store.switch_version(&name, &id).map(|message| Outcome {
                        message,
                        selected: Some(name),
                        ..Default::default()
                    })
                }
                Operation::DeleteVersion(name, id) => {
                    store.delete_version(&name, &id).map(|message| Outcome {
                        message,
                        selected: Some(name),
                        ..Default::default()
                    })
                }
                Operation::DeleteProfile(name) => {
                    store.delete_profile(&name).map(|message| Outcome {
                        message,
                        ..Default::default()
                    })
                }
                Operation::Desktop(app, action) => {
                    desktop::perform(app, action, &app_paths).map(|message| Outcome {
                        message,
                        ..Default::default()
                    })
                }
                Operation::OpenInstallation(app) => desktop::installation_path(app, &app_paths)
                    .map(|path| Outcome {
                        reveal: Some(path),
                        ..Default::default()
                    }),
            };
            (result, store.load())
        });
        cx.spawn_in(window, async move |this, cx| {
            let (result, snapshot) = job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(outcome) => {
                        this.message = outcome.message;
                        if let Some(selected) = outcome.selected {
                            this.selected = Some(selected);
                        }
                        if let Some(path) = outcome.reveal {
                            cx.reveal_path(&path);
                        }
                        this.pending_delete = None;
                        if close_on_success {
                            this.modal = None;
                            window.close_dialog(cx);
                        }
                    }
                    Err(error) => {
                        this.is_error = true;
                        this.message = format!("{error:#}");
                        if this.modal.is_some() {
                            this.dialog_error = this.message.clone();
                        }
                    }
                }
                match snapshot {
                    Ok(mut snapshot) => {
                        this.profile_order.apply(&mut snapshot.profiles);
                        if !snapshot
                            .profiles
                            .iter()
                            .any(|p| Some(&p.key) == this.selected.as_ref())
                        {
                            this.selected = snapshot
                                .profiles
                                .iter()
                                .find(|p| p.is_current)
                                .or_else(|| snapshot.profiles.first())
                                .map(|p| p.key.clone());
                        }
                        this.snapshot = snapshot;
                        this.scroll_to_selected();
                        this.loaded = true;
                    }
                    Err(error) => {
                        this.is_error = true;
                        this.message.push_str(&format!(" 无法刷新列表：{error:#}"));
                        if this.modal.is_some() {
                            this.dialog_error = this.message.clone();
                        }
                        this.loaded = false;
                    }
                }
                cx.notify();
                window.refresh();
                this.refresh_apps(cx);
            });
        })
        .detach();
        cx.notify();
        window.refresh();
    }

    fn selected_profile(&self) -> Option<&Profile> {
        self.snapshot
            .profiles
            .iter()
            .find(|p| Some(&p.key) == self.selected.as_ref())
    }

    fn scroll_to_selected(&self) {
        if let Some(index) = self
            .snapshot
            .profiles
            .iter()
            .position(|profile| Some(&profile.key) == self.selected.as_ref())
        {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
    }

    fn render_sort_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let order = self.profile_order;
        Button::new("profile-order")
            .outline()
            .small()
            .label(order.label())
            .icon(IconName::ChevronDown)
            .accessibility_label("配置列表排序方式")
            .disabled(!self.loaded || self.busy)
            .dropdown_menu(move |mut menu, _, _| {
                for choice in [
                    ProfileOrder::Newest,
                    ProfileOrder::Oldest,
                    ProfileOrder::Name,
                ] {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(choice.label())
                            .checked(choice == order)
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.profile_order = choice;
                                    choice.apply(&mut this.snapshot.profiles);
                                    this.scroll_to_selected();
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
    }

    fn can_backup(&self) -> bool {
        self.loaded && !self.busy && (self.snapshot.has_config || self.snapshot.has_auth)
    }

    fn backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_backup() || self.modal.is_some() {
            return;
        }
        if let Some(name) = self.selected.clone() {
            self.run(Operation::Backup(name), window, cx);
        }
    }

    fn switch_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.loaded || self.modal.is_some() {
            return;
        }
        if let Some(profile) = self.selected_profile()
            && profile.problem.is_none()
            && (!profile.is_current || profile.is_dirty)
        {
            self.run(Operation::Switch(profile.key.clone()), window, cx);
        }
    }

    fn create_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_backup() || !matches!(self.modal, Some(Modal::New)) {
            return;
        }
        let name = self.new_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.dialog_error.clear();
        self.run(Operation::Create(name), window, cx);
    }

    fn show_new_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_backup() || self.modal.is_some() {
            return;
        }
        self.modal = Some(Modal::New);
        self.modal_revision += 1;
        let revision = self.modal_revision;
        self.dialog_error.clear();
        self.new_name
            .update(cx, |input, cx| input.set_value("", window, cx));
        let view = cx.entity().downgrade();
        let input = self.new_name.clone();
        let initial_focus = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            if initial_focus.replace(false) {
                input.focus_handle(cx).focus(window, cx);
            }
            let this = view.read(cx);
            let busy = this.busy;
            let error = this.dialog_error.clone();
            let empty = input.read(cx).value().trim().is_empty();
            let save_view = view.downgrade();
            let confirm_view = view.downgrade();
            let cancel_view = view.downgrade();
            let close_view = view.downgrade();
            dialog
                .title("新建备份")
                .centered()
                .overlay_closable(!busy)
                .keyboard(!busy)
                .close_button(!busy)
                .on_cancel(move |_, _, _| !busy)
                .on_ok(move |_, window, cx| {
                    let _ = confirm_view.update(cx, |this, cx| this.create_profile(window, cx));
                    false
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_sm().child("配置名称"))
                        .child(
                            Input::new(&input)
                                .id("new-backup-name")
                                .aria_label("配置名称")
                                .disabled(busy),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("新建同名目录，并保存当前 config.toml 和 auth.json。"),
                        )
                        .children(
                            (!error.is_empty()).then(|| {
                                div().text_sm().text_color(cx.theme().danger).child(error)
                            }),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel-new")
                                .label("取消")
                                .disabled(busy)
                                .on_click(move |_, window, cx| {
                                    let _ = cancel_view.update(cx, |this, cx| {
                                        this.modal = None;
                                        cx.notify();
                                    });
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("create-backup")
                                .primary()
                                .label("创建并保存")
                                .disabled(busy || empty)
                                .loading(busy)
                                .on_click(move |_, window, cx| {
                                    let _ = save_view
                                        .update(cx, |this, cx| this.create_profile(window, cx));
                                }),
                        ),
                )
                .on_close(move |_, _, cx| {
                    let view = close_view.clone();
                    cx.defer(move |cx| {
                        let _ = view.update(cx, |this, cx| {
                            if this.modal_revision == revision {
                                this.modal = None;
                                cx.notify();
                            }
                        });
                    });
                })
        });
        cx.notify();
    }

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.busy || self.modal.is_some() || self.snapshot.profiles.is_empty() {
            return;
        }
        let current = self
            .snapshot
            .profiles
            .iter()
            .position(|p| Some(&p.key) == self.selected.as_ref())
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(direction)
            .min(self.snapshot.profiles.len() - 1);
        self.selected = Some(self.snapshot.profiles[next].key.clone());
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn render_profile(&self, profile: &Profile, cx: &mut Context<Self>) -> impl IntoElement {
        let key = profile.key.clone();
        let folder_key = key.clone();
        let status = if profile.is_current {
            if profile.is_dirty {
                "使用中 · 与最新不同"
            } else {
                "使用中"
            }
        } else {
            profile.problem.as_deref().unwrap_or("可切换")
        };
        let status_color = if profile.is_current {
            cx.theme().success
        } else if profile.problem.is_some() {
            cx.theme().warning
        } else {
            cx.theme().muted_foreground
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .w_full()
            .min_w_0()
            .px_3()
            .py_0p5()
            .child(
                Button::new(SharedString::from(format!("profile:{key}")))
                    .ghost()
                    .small()
                    .selected(self.selected.as_ref() == Some(&key))
                    .flex_1()
                    .min_w_0()
                    .h_12()
                    .disabled(self.busy)
                    .accessibility_label(format!(
                        "选择 {}，{} 个版本，{}",
                        profile.name, profile.version_count, status
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .flex_1()
                                    .min_w_0()
                                    .items_start()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .w_full()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .font_semibold()
                                                    .truncate()
                                                    .child(profile.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .flex_shrink_0()
                                                    .child(format!(
                                                        "{} 个版本",
                                                        profile.version_count
                                                    )),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .w_full()
                                            .child(format!(
                                                "{} · {}",
                                                profile.summary.provider, profile.summary.model
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .items_end()
                                    .flex_shrink_0()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(status_color)
                                            .child(status.to_string()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(profile.time.clone()),
                                    ),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.selected = Some(key.clone());
                        window.focus(&this.list_focus, cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("folder:{folder_key}")))
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .accessibility_label(format!("打开 {} 的备份目录", profile.name))
                    .tooltip(format!("打开 {} 的备份目录", profile.name))
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run(Operation::Open(folder_key.clone()), window, cx)
                    })),
            )
    }
}

impl Render for Switcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let selected = self.selected_profile();
        let switch_disabled = self.busy
            || !self.loaded
            || selected.is_none_or(|p| p.problem.is_some() || (p.is_current && !p.is_dirty));
        let backup_disabled = !self.can_backup() || selected.is_none();
        let target = selected
            .map(|p| format!("保存到：{}", p.name))
            .unwrap_or_else(|| "先选择目录，或新建备份".into());
        let help = selected
            .and_then(|p| p.problem.clone())
            .unwrap_or_else(|| "切换使用最新版本，并自动备份当前文件".into());
        let current = if !self.loaded {
            "尚未读取配置".into()
        } else if !self.snapshot.has_config {
            "尚无 config.toml".into()
        } else {
            format!("当前：{}", self.snapshot.current_name)
        };
        let current_detail = if self.snapshot.has_config {
            format!(
                "{} · {}{}",
                self.snapshot.current.provider,
                self.snapshot.current.model,
                if self.snapshot.has_auth {
                    ""
                } else {
                    " · 缺少 auth.json"
                }
            )
        } else {
            "先通过 Codex 生成配置，再新建备份。".into()
        };
        let count = self.snapshot.profiles.len();
        let entity = cx.entity();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .text_sm()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .key_context("Switcher")
            .on_action(cx.listener(|this, _: &Refresh, window, cx| {
                if this.modal.is_none() {
                    this.run(Operation::Refresh, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SaveBackup, window, cx| this.backup(window, cx)))
            .on_action(
                cx.listener(|this, _: &NewBackup, window, cx| this.show_new_dialog(window, cx)),
            )
            .child(self.render_header(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(div().font_semibold().truncate().child(current))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(current_detail),
                    )
                    .children(self.snapshot.warning.clone().map(|warning| {
                        div().text_xs().text_color(cx.theme().danger).child(warning)
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .font_semibold()
                            .flex_1()
                            .child(format!("配置目录 · {count}")),
                    )
                    .child(self.render_sort_menu(cx))
                    .child(
                        Button::new("new-backup")
                            .outline()
                            .small()
                            .label("新建备份…")
                            .icon(IconName::Plus)
                            .disabled(!self.can_backup())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show_new_dialog(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .track_focus(&self.list_focus)
                    .key_context("Profiles")
                    .on_action(
                        cx.listener(|this, _: &PreviousProfile, _, cx| this.move_selection(-1, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &NextProfile, _, cx| this.move_selection(1, cx)),
                    )
                    .on_action(cx.listener(|this, _: &SwitchProfile, window, cx| {
                        this.switch_selected(window, cx)
                    }))
                    .child(if count == 0 {
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .p_4()
                            .child(div().font_semibold().child(if self.busy {
                                "正在读取配置…"
                            } else {
                                "还没有配置目录"
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("点击“新建备份…”命名并保存当前配置。"),
                            )
                            .into_any_element()
                    } else {
                        uniform_list("profiles", count, move |range, _, cx| {
                            entity.update(cx, |this, cx| {
                                range
                                    .map(|ix| {
                                        this.render_profile(&this.snapshot.profiles[ix], cx)
                                            .into_any_element()
                                    })
                                    .collect()
                            })
                        })
                        .track_scroll(&self.scroll)
                        .w_full()
                        .flex_1()
                        .min_h_0()
                        .into_any_element()
                    })
                    .child(Scrollbar::vertical(&self.scroll).viewport_from_layout()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .min_w_0()
                            .child(div().truncate().child(target))
                            .child(div().text_xs().text_color(muted).truncate().child(help)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .w_full()
                            .child(
                                Button::new("backup")
                                    .outline()
                                    .small()
                                    .label("保存备份")
                                    .disabled(backup_disabled)
                                    .tooltip(if cfg!(target_os = "macos") {
                                        "将当前文件保存到选中的配置目录（⌘B）"
                                    } else {
                                        "将当前文件保存到选中的配置目录（Ctrl+B）"
                                    })
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.backup(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new("switch")
                                    .primary()
                                    .small()
                                    .label("切换配置")
                                    .disabled(switch_disabled)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.switch_selected(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("versions")
                                    .outline()
                                    .small()
                                    .label("历史版本…")
                                    .disabled(self.busy || !self.loaded || selected.is_none())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(name) = this.selected.clone() {
                                            this.show_management_dialog(
                                                Modal::Versions(name),
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("delete-profile")
                                    .ghost()
                                    .small()
                                    .label("删除备份…")
                                    .text_color(cx.theme().danger)
                                    .disabled(self.busy || !self.loaded || selected.is_none())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(name) = this.selected.clone() {
                                            this.show_management_dialog(
                                                Modal::Delete(name),
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("feedback")
                    .role(Role::Status)
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(if self.is_error {
                        cx.theme().danger
                    } else {
                        muted
                    })
                    .child(if self.message.is_empty() {
                        SHORTCUTS.into()
                    } else {
                        self.message.clone()
                    }),
            )
    }
}
