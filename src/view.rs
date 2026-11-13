mod about_window;
mod auxiliary_window;
mod config_editor;
mod desktop_controls;
mod dialogs;
mod history_window;
mod new_backup_window;
mod settings_window;
mod theme_controls;

use crate::desktop::{self, AppAction, AppStatus, DesktopApp};
use crate::store::{Profile, Settings, Snapshot, Store};
use crate::themes::{ThemeService, ThemeSnapshot};
use auxiliary_window::WindowKind;
use gpui_kit::assets::IconName;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    scroll::Scrollbar,
};
use gpui_kit::*;
use std::{collections::BTreeSet, path::PathBuf, time::Duration};
use theme_controls::ThemeChoice;

#[cfg(target_os = "macos")]
const COMMAND_KEYS: [&str; 3] = ["cmd-r", "cmd-b", "cmd-n"];
#[cfg(not(target_os = "macos"))]
const COMMAND_KEYS: [&str; 3] = ["ctrl-r", "ctrl-b", "ctrl-n"];

actions!(
    switcher,
    [
        Refresh,
        SaveBackup,
        NewBackup,
        PreviousProfile,
        NextProfile,
        SwitchProfile,
        SaveConfigFile,
        CloseAuxiliary
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
        KeyBinding::new("escape", CloseAuxiliary, Some("AuxiliaryWindow")),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-s"
            } else {
                "ctrl-s"
            },
            SaveConfigFile,
            Some("ConfigEditor"),
        ),
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
    DeleteProfiles(Vec<String>),
    Desktop(DesktopApp, AppAction),
    OpenInstallation(DesktopApp),
    ReadConfig,
    SaveConfig(String, String),
    SelectTheme(Option<String>),
    ImportThemes(Vec<PathBuf>),
    ThemeTransparency(u8),
    OpenThemes,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ProfileOrder {
    Newest,
    Oldest,
    #[default]
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

struct OperationFinished {
    origin: Option<AnyWindowHandle>,
    success: bool,
    close_window: bool,
    message: String,
}

impl EventEmitter<OperationFinished> for Switcher {}

#[derive(Default)]
struct Outcome {
    message: String,
    selected: Option<String>,
    reveal: Option<PathBuf>,
    open_path: Option<PathBuf>,
    config: Option<String>,
}

pub(crate) struct Switcher {
    store: Store,
    snapshot: Snapshot,
    theme_service: ThemeService,
    themes: ThemeSnapshot,
    theme_select: Entity<SelectState<Vec<ThemeChoice>>>,
    checking_themes: bool,
    theme_select_synced: bool,
    last_theme_error: Option<String>,
    pending_theme_error: Option<String>,
    _theme_subscription: Subscription,
    loaded: bool,
    busy: bool,
    selected: Option<String>,
    profile_order: ProfileOrder,
    auxiliary_windows: Vec<(WindowKind, AnyWindowHandle)>,
    app_statuses: Vec<AppStatus>,
    app_error: String,
    checking_apps: bool,
    app_revision: u64,
    _app_monitor: Option<Task<()>>,
    prompt_open: bool,
    multi_select: bool,
    checked_profiles: BTreeSet<String>,
    editor_window: Option<WeakEntity<auxiliary_window::AuxiliaryWindow>>,
    list_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    message: String,
    is_error: bool,
}

impl Switcher {
    pub(crate) fn operation_pending(&self) -> bool {
        self.busy || self.prompt_open
    }

    pub(crate) fn exit_on_close(&self) -> bool {
        self.snapshot.settings.exit_on_close
    }

    pub(crate) fn main_focus(&self) -> FocusHandle {
        self.list_focus.clone()
    }

    pub(crate) fn profiles(&self) -> &[Profile] {
        &self.snapshot.profiles
    }

    pub(crate) fn show_settings(&mut self, cx: &mut Context<Self>) {
        if self.loaded && !self.operation_pending() {
            self.show_auxiliary_window(WindowKind::Settings, None, cx);
        }
    }

    pub(crate) fn report_shell_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.message = message;
        self.is_error = true;
        cx.notify();
    }

    pub fn new(store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list_focus = cx.focus_handle().tab_stop(true);
        window.focus(&list_focus, cx);
        let theme_select = cx.new(|cx| {
            SelectState::new(
                vec![ThemeChoice::off()],
                Some(gpui_kit::component::IndexPath::new(0)),
                window,
                cx,
            )
        });
        let theme_subscription = cx.subscribe_in(
            &theme_select,
            window,
            |this, _, event: &SelectEvent<Vec<ThemeChoice>>, window, cx| {
                let SelectEvent::Confirm(value) = event;
                let selected = value.as_ref().filter(|value| !value.is_empty()).cloned();
                if selected != this.themes.selected && !this.operation_pending() {
                    this.theme_select_synced = false;
                    this.run(Operation::SelectTheme(selected), window, cx);
                }
            },
        );
        let theme_service = ThemeService::new(store.root().join("backups/theme-state.json"));
        let mut this = Self {
            store,
            snapshot: Snapshot::default(),
            theme_service,
            themes: ThemeSnapshot::default(),
            theme_select,
            checking_themes: false,
            theme_select_synced: false,
            last_theme_error: None,
            pending_theme_error: None,
            _theme_subscription: theme_subscription,
            loaded: false,
            busy: false,
            selected: None,
            profile_order: ProfileOrder::default(),
            auxiliary_windows: Vec::new(),
            app_statuses: Vec::new(),
            app_error: String::new(),
            checking_apps: false,
            app_revision: 0,
            _app_monitor: None,
            prompt_open: false,
            multi_select: false,
            checked_profiles: BTreeSet::new(),
            editor_window: None,
            list_focus,
            scroll: UniformListScrollHandle::new(),
            message: "正在读取配置…".into(),
            is_error: false,
        };
        this._app_monitor = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                if this
                    .update(cx, |this, cx| {
                        this.refresh_apps(cx);
                        this.refresh_themes(cx);
                        if let Some(error) = this.theme_service.take_error() {
                            this.show_theme_error(error, cx);
                        }
                        if !this.prompt_open
                            && let Some(error) = this.pending_theme_error.take()
                        {
                            this.show_theme_error(error, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        this.run(Operation::Refresh, window, cx);
        this
    }

    fn run(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        self.run_with_origin(operation, Some(window.window_handle()), cx);
    }

    fn run_with_origin(
        &mut self,
        operation: Operation,
        origin: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.prompt_open {
            return;
        }
        self.busy = true;
        if matches!(
            operation,
            Operation::Refresh
                | Operation::Settings(_)
                | Operation::SelectTheme(_)
                | Operation::ImportThemes(_)
                | Operation::ThemeTransparency(_)
                | Operation::OpenThemes
                | Operation::Desktop(_, AppAction::Open | AppAction::Restart)
        ) {
            self.last_theme_error = None;
            self.pending_theme_error = None;
        }
        self.app_revision += 1;
        self.is_error = false;
        let close_auxiliary = matches!(
            operation,
            Operation::SaveConfig(_, _) | Operation::Create(_)
        );
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
            Operation::DeleteProfiles(_) => "正在删除备份目录…".into(),
            Operation::Desktop(app, action) => format!("正在{} {}…", action.label(), app.name()),
            Operation::OpenInstallation(_) => "正在定位 ChatGPT 应用…".into(),
            Operation::ReadConfig => "正在读取 config.toml…".into(),
            Operation::SaveConfig(_, _) => "正在保存 config.toml…".into(),
            Operation::SelectTheme(_) => "正在切换主题…".into(),
            Operation::ImportThemes(_) => "正在导入主题…".into(),
            Operation::ThemeTransparency(_) => "正在调整主题透明度…".into(),
            Operation::OpenThemes => "正在打开主题目录…".into(),
        };
        let store = self.store.clone();
        let app_paths = self.snapshot.settings.app_paths();
        let theme_service = self.theme_service.clone();
        let theme_directory = self.snapshot.settings.theme_directory.clone();
        let theme_operation = matches!(
            operation,
            Operation::SelectTheme(_)
                | Operation::ImportThemes(_)
                | Operation::ThemeTransparency(_)
                | Operation::OpenThemes
        );
        let themed_launch = self.themes.selected.is_some()
            && matches!(
                operation,
                Operation::Desktop(_, AppAction::Open | AppAction::Restart)
            );
        let job = cx.background_spawn(async move {
            let result =
                match operation {
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
                        open_path: Some(path),
                        ..Default::default()
                    }),
                    Operation::OpenVersion(name, id) => {
                        store.version_directory(&name, &id).map(|path| Outcome {
                            open_path: Some(path),
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
                    Operation::DeleteProfiles(names) => {
                        store.delete_profiles(&names).map(|message| Outcome {
                            message,
                            ..Default::default()
                        })
                    }
                    Operation::Desktop(app, action) => desktop::perform(app, action, &app_paths)
                        .map(|message| Outcome {
                            message,
                            ..Default::default()
                        }),
                    Operation::OpenInstallation(app) => desktop::installation_path(app, &app_paths)
                        .map(|path| Outcome {
                            reveal: Some(path),
                            ..Default::default()
                        }),
                    Operation::ReadConfig => store.read_config().map(|config| Outcome {
                        config: Some(config),
                        ..Default::default()
                    }),
                    Operation::SaveConfig(original, edited) => store
                        .save_config(&original, &edited)
                        .map(|message| Outcome {
                            message,
                            ..Default::default()
                        }),
                    Operation::SelectTheme(id) => {
                        theme_service
                            .select(&theme_directory, id)
                            .map(|message| Outcome {
                                message,
                                ..Default::default()
                            })
                    }
                    Operation::ImportThemes(paths) => theme_service
                        .import_files(&theme_directory, &paths)
                        .map(|message| Outcome {
                            message,
                            ..Default::default()
                        }),
                    Operation::ThemeTransparency(value) => theme_service
                        .set_transparency(value)
                        .map(|message| Outcome {
                            message,
                            ..Default::default()
                        }),
                    Operation::OpenThemes => {
                        theme_service.directory(&theme_directory).and_then(|path| {
                            std::fs::create_dir_all(&path)?;
                            Ok(Outcome {
                                open_path: Some(path),
                                ..Default::default()
                            })
                        })
                    }
                };
            (result, store.load())
        });
        // Tray actions can outlive all windows. Complete against the retained
        // application state instead of binding this task to a native window.
        cx.spawn(async move |this, cx| {
            let (result, snapshot) = job.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                let success = result.is_ok();
                match result {
                    Ok(outcome) => {
                        this.message = outcome.message;
                        if let Some(selected) = outcome.selected {
                            this.selected = Some(selected);
                        }
                        if let Some(path) = outcome.reveal {
                            cx.reveal_path(&path);
                        }
                        if let Some(path) = outcome.open_path {
                            cx.open_with_system(&path);
                        }
                        if let Some(config) = outcome.config {
                            this.show_auxiliary_window(WindowKind::Editor, Some(config), cx);
                        }
                    }
                    Err(error) => {
                        this.is_error = true;
                        this.message = format!("{error:#}");
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
                        this.checked_profiles
                            .retain(|name| snapshot.profiles.iter().any(|p| &p.key == name));
                        this.snapshot = snapshot;
                        this.scroll_to_selected();
                        this.loaded = true;
                    }
                    Err(error) => {
                        this.is_error = true;
                        this.message.push_str(&format!(" 无法刷新列表：{error:#}"));
                        this.loaded = false;
                    }
                }
                cx.notify();
                cx.refresh_windows();
                this.refresh_apps(cx);
                this.refresh_themes(cx);
                if (theme_operation || themed_launch) && !success {
                    this.show_theme_error(this.message.clone(), cx);
                }
                cx.emit(OperationFinished {
                    origin,
                    success,
                    close_window: success && close_auxiliary,
                    message: this.message.clone(),
                });
            });
        })
        .detach();
        cx.notify();
        cx.refresh_windows();
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
            .disabled(!self.loaded || self.operation_pending())
            .dropdown_menu(move |mut menu, _, _| {
                for choice in [
                    ProfileOrder::Name,
                    ProfileOrder::Newest,
                    ProfileOrder::Oldest,
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
        self.loaded
            && !self.busy
            && !self.prompt_open
            && (self.snapshot.has_config || self.snapshot.has_auth)
    }

    fn backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_backup() || self.multi_select {
            return;
        }
        if let Some(name) = self.selected.clone() {
            self.request_backup(name, window, cx);
        }
    }

    pub(crate) fn can_switch_profile(&self, name: &str) -> bool {
        self.loaded
            && !self.operation_pending()
            && self.snapshot.profiles.iter().any(|profile| {
                profile.key == name
                    && profile.problem.is_none()
                    && (!profile.is_current || profile.is_dirty)
            })
    }

    fn switch_to(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_switch_profile(name) {
            self.confirm_switch(name.to_string(), None, window, cx);
        }
    }

    fn switch_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.multi_select {
            return;
        }
        if let Some(name) = self.selected.clone() {
            self.switch_to(&name, window, cx);
        }
    }

    fn show_new_window(&mut self, cx: &mut Context<Self>) {
        if self.can_backup() && !self.prompt_open {
            self.show_auxiliary_window(WindowKind::NewBackup, None, cx);
        }
    }

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.busy || self.prompt_open || self.snapshot.profiles.is_empty() {
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

    fn toggle_checked(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.operation_pending() {
            return;
        }
        if !self.checked_profiles.remove(name) {
            self.checked_profiles.insert(name.to_string());
        }
        cx.notify();
    }

    fn render_profile(&self, profile: &Profile, cx: &mut Context<Self>) -> impl IntoElement {
        let key = profile.key.clone();
        let folder_key = key.clone();
        let checkbox_key = key.clone();
        let context_key = key.clone();
        let view = cx.entity().downgrade();
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
            .id(SharedString::from(format!("profile-row:{key}")))
            .flex()
            .items_center()
            .gap_1()
            .w_full()
            .min_w_0()
            .px_3()
            .py_0p5()
            .children(self.multi_select.then(|| {
                Checkbox::new(SharedString::from(format!("check-profile:{checkbox_key}")))
                    .checked(self.checked_profiles.contains(&checkbox_key))
                    .accessibility_label(format!("选择备份目录 {}", profile.name))
                    .disabled(self.operation_pending())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_checked(&checkbox_key, cx)),
                    )
            }))
            .child(
                Button::new(SharedString::from(format!("profile:{key}")))
                    .ghost()
                    .small()
                    .selected(if self.multi_select {
                        self.checked_profiles.contains(&key)
                    } else {
                        self.selected.as_ref() == Some(&key)
                    })
                    .flex_1()
                    .min_w_0()
                    .h_12()
                    .disabled(self.operation_pending())
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
                    .tooltip(if self.multi_select {
                        "勾选或取消选择此备份目录"
                    } else {
                        "单击选择，双击切换到最新备份"
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if this.busy || this.prompt_open {
                            return;
                        }
                        if this.multi_select {
                            this.toggle_checked(&key, cx);
                            return;
                        }
                        this.selected = Some(key.clone());
                        window.focus(&this.list_focus, cx);
                        cx.notify();
                        if event.click_count() == 2 {
                            this.switch_selected(window, cx);
                        }
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("folder:{folder_key}")))
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .accessibility_label(format!("打开 {} 的备份目录", profile.name))
                    .tooltip(format!("打开 {} 的备份目录", profile.name))
                    .disabled(self.operation_pending())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run(Operation::Open(folder_key.clone()), window, cx)
                    })),
            )
            .context_menu(move |menu, _, cx| {
                let Some(owner) = view.upgrade() else {
                    return menu;
                };
                let unavailable = owner.read(cx).operation_pending();
                let backup_disabled = !owner.read(cx).can_backup();
                let switch_disabled = !owner.read(cx).can_switch_profile(&context_key);
                let backup_owner = view.clone();
                let switch_owner = view.clone();
                let history_owner = view.clone();
                let folder_owner = view.clone();
                let delete_owner = view.clone();
                let backup = context_key.clone();
                let switch = context_key.clone();
                let history = context_key.clone();
                let folder = context_key.clone();
                let delete = context_key.clone();
                menu.item(
                    PopupMenuItem::new(" 保存备份")
                        .icon(IconName::Save)
                        .disabled(backup_disabled)
                        .on_click(move |_, window, cx| {
                            let _ = backup_owner.update(cx, |this, cx| {
                                this.request_backup(backup.clone(), window, cx)
                            });
                        }),
                )
                .item(
                    PopupMenuItem::new(" 切换配置")
                        .icon(IconName::ArrowLeftRight)
                        .disabled(switch_disabled)
                        .on_click(move |_, window, cx| {
                            let _ = switch_owner
                                .update(cx, |this, cx| this.switch_to(&switch, window, cx));
                        }),
                )
                .item(
                    PopupMenuItem::new(" 历史版本")
                        .icon(IconName::FolderClock)
                        .disabled(unavailable)
                        .on_click(move |_, _, cx| {
                            let _ = history_owner.update(cx, |this, cx| {
                                if !this.operation_pending() {
                                    this.show_auxiliary_window(
                                        WindowKind::Versions(history.clone()),
                                        None,
                                        cx,
                                    );
                                }
                            });
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new(" 打开目录")
                        .icon(IconName::FolderOpen)
                        .disabled(unavailable)
                        .on_click(move |_, window, cx| {
                            let _ = folder_owner.update(cx, |this, cx| {
                                this.run(Operation::Open(folder.clone()), window, cx)
                            });
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new(" 删除备份")
                        .icon(IconName::Trash)
                        .disabled(unavailable)
                        .on_click(move |_, window, cx| {
                            let _ = delete_owner.update(cx, |this, cx| {
                                this.confirm_delete_profiles(vec![delete.clone()], window, cx)
                            });
                        }),
                )
            })
    }
}

impl Render for Switcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let selected = if self.multi_select {
            None
        } else {
            self.selected_profile()
        };
        let switch_disabled = self.operation_pending()
            || !self.loaded
            || selected.is_none_or(|p| p.problem.is_some() || (p.is_current && !p.is_dirty));
        let backup_disabled = !self.can_backup() || selected.is_none();
        let target = if self.multi_select {
            format!("已选择 {} 个配置目录", self.checked_profiles.len())
        } else {
            selected
                .map(|p| format!("保存到：{}", p.name))
                .unwrap_or_else(|| "先选择目录，或新建备份".into())
        };
        let problem = selected.and_then(|p| p.problem.clone());
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
                if !this.prompt_open {
                    this.run(Operation::Refresh, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SaveBackup, window, cx| this.backup(window, cx)))
            .on_action(cx.listener(|this, _: &NewBackup, _, cx| this.show_new_window(cx)))
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .flex_1()
                                    .gap_0p5()
                                    .child(div().font_semibold().truncate().child(current))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .truncate()
                                            .child(current_detail),
                                    ),
                            )
                            .child(
                                Button::new("edit-current-config")
                                    .outline()
                                    .icon(IconName::FilePen)
                                    .tooltip("编辑当前 config.toml")
                                    .flex_shrink_0()
                                    .disabled(
                                        self.operation_pending()
                                            || !self.loaded
                                            || !self.snapshot.has_config,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if !this.prompt_open {
                                            this.open_config_window(window, cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("open-current-config-directory")
                                    .outline()
                                    .icon(IconName::FolderOpen)
                                    .flex_shrink_0()
                                    .accessibility_label("打开 config.toml 所在目录")
                                    .tooltip("打开 config.toml 所在目录")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let path = this.store.root().join("config.toml");
                                        cx.reveal_path(&path)
                                    })),
                            )
                            .child(self.render_theme_controls(cx)),
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
                    .child(
                        Button::new("multi-select")
                            .outline()
                            .small()
                            .label(if self.multi_select {
                                "完成"
                            } else {
                                "多选"
                            })
                            .selected(self.multi_select)
                            .disabled(
                                self.operation_pending() || (!self.multi_select && count == 0),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.multi_select = !this.multi_select;
                                this.checked_profiles.clear();
                                cx.notify();
                            })),
                    )
                    .children(self.multi_select.then(|| {
                        Button::new("select-all-profiles")
                            .ghost()
                            .small()
                            .label("全选")
                            .disabled(
                                self.operation_pending()
                                    || count == 0
                                    || self.checked_profiles.len() == count,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.checked_profiles = this
                                    .snapshot
                                    .profiles
                                    .iter()
                                    .map(|p| p.key.clone())
                                    .collect();
                                cx.notify();
                            }))
                    }))
                    .children(self.multi_select.then(|| {
                        Button::new("deselect-all-profiles")
                            .ghost()
                            .small()
                            .label("取消全选")
                            .disabled(self.operation_pending() || self.checked_profiles.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.checked_profiles.clear();
                                cx.notify();
                            }))
                    }))
                    .child(self.render_sort_menu(cx))
                    .child(
                        Button::new("new-backup")
                            .outline()
                            .small()
                            .tooltip("新建备份…")
                            .icon(IconName::Plus)
                            .disabled(!self.can_backup())
                            .on_click(cx.listener(|this, _, _, cx| this.show_new_window(cx))),
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
                            .children(problem.map(|problem| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .child(problem)
                            })),
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
                                    .icon(IconName::Save)
                                    .label("备份")
                                    .tooltip("保存当前备份到选中的配置目录")
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
                                    .icon(IconName::ArrowLeftRight)
                                    .label("切换")
                                    .tooltip("切换到最新版本，并自动备份当前文件")
                                    .disabled(switch_disabled)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.switch_selected(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("versions")
                                    .outline()
                                    .icon(IconName::FolderClock)
                                    .label("历史")
                                    .tooltip("打开历史版本列表")
                                    .disabled(
                                        self.operation_pending()
                                            || !self.loaded
                                            || selected.is_none(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(name) = this.selected.clone() {
                                            this.show_auxiliary_window(
                                                WindowKind::Versions(name),
                                                None,
                                                cx,
                                            );
                                        }
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("delete-profile")
                                    .danger()
                                    .small()
                                    .icon(IconName::Trash)
                                    .label(if self.multi_select {
                                        format!("删除 ({})", self.checked_profiles.len())
                                    } else {
                                        "删除".into()
                                    })
                                    .tooltip("删除选中的配置目录及其所有版本")
                                    .disabled(
                                        self.operation_pending()
                                            || !self.loaded
                                            || self.deletion_targets().is_empty(),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_delete_profiles(
                                            this.deletion_targets(),
                                            window,
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            )
            .children((!self.message.is_empty()).then(|| {
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
                    .child(self.message.clone())
            }))
    }
}
