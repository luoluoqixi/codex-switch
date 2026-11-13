use super::config_editor::ConfigEditor;
use super::history_window::VersionHistory;
use super::new_backup_window::BackupForm;
use super::settings_window::SettingsForm;
use super::*;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum WindowKind {
    Editor,
    Settings,
    Versions(String),
    NewBackup,
    About,
}

pub(super) enum WindowContent {
    Editor(ConfigEditor),
    Settings(SettingsForm),
    Versions(VersionHistory),
    NewBackup(BackupForm),
    About,
}

pub(super) struct AuxiliaryWindow {
    pub(super) owner: Entity<Switcher>,
    pub(super) content: WindowContent,
    pub(super) error: String,
    pub(super) message: String,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

// Opening or activating a native window can synchronously render its content.
// Call this from an App callback, outside any update of the shared owner.
fn activate_auxiliary(owner: &Entity<Switcher>, kind: &WindowKind, cx: &mut App) -> bool {
    let handle = owner
        .read(cx)
        .auxiliary_windows
        .iter()
        .find(|(key, _)| key == kind)
        .map(|(_, handle)| *handle);
    let Some(handle) = handle else {
        return false;
    };
    if cx
        .update_window(handle, |_, window, _| window.activate_window())
        .is_ok()
    {
        return true;
    }
    owner.update(cx, |owner, _| {
        owner
            .auxiliary_windows
            .retain(|(_, existing)| *existing != handle);
    });
    false
}

impl Switcher {
    pub(super) fn open_config_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.entity();
        let origin = window.window_handle();
        cx.defer(move |cx| {
            if !activate_auxiliary(&owner, &WindowKind::Editor, cx) {
                let _ = cx.update_window(origin, |_, window, cx| {
                    owner.update(cx, |owner, cx| owner.run(Operation::ReadConfig, window, cx));
                });
            }
        });
    }

    pub(super) fn show_auxiliary_window(
        &mut self,
        kind: WindowKind,
        config: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity();
        // App::defer releases the Switcher lease before open_window draws the
        // child. defer_in would lease Switcher again and retain the panic.
        cx.defer(move |cx| open_auxiliary_window(owner, kind, config, cx));
    }
}

fn open_auxiliary_window(
    owner: Entity<Switcher>,
    kind: WindowKind,
    config: Option<String>,
    cx: &mut App,
) {
    if activate_auxiliary(&owner, &kind, cx) {
        return;
    }
    let (title, default_size, minimum_size) = match &kind {
        WindowKind::About => (
            format!("关于 {}", crate::APP_NAME),
            size(px(440.), px(260.)),
            size(px(360.), px(240.)),
        ),
        WindowKind::NewBackup => (
            "新建备份".to_string(),
            size(px(480.), px(240.)),
            size(px(380.), px(240.)),
        ),
        WindowKind::Editor => (
            "编辑 config.toml".to_string(),
            size(px(880.), px(640.)),
            size(px(560.), px(380.)),
        ),
        WindowKind::Settings => (
            "全局设置".to_string(),
            size(px(600.), px(520.)),
            size(px(460.), px(380.)),
        ),
        WindowKind::Versions(name) => (
            format!("历史版本 · {name}"),
            size(px(720.), px(520.)),
            size(px(600.), px(360.)),
        ),
    };
    let (bounds, display_id) = crate::window_placement::centered_on_main(default_size, cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id,
        window_min_size: Some(minimum_size),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let settings = owner.read(cx).snapshot.settings.clone();
    let child_owner = owner.clone();
    let window_kind = kind.clone();
    match open_window(options, cx, move |window, cx| {
        cx.new(|cx| AuxiliaryWindow::new(child_owner, window_kind, config, settings, window, cx))
    }) {
        Ok((handle, child)) => owner.update(cx, |owner, _| {
            if kind == WindowKind::Editor {
                owner.editor_window = Some(child.downgrade());
            }
            owner.auxiliary_windows.push((kind, handle));
        }),
        Err(error) => {
            owner.update(cx, |owner, cx| {
                owner.message = format!("无法打开窗口：{error:#}");
                owner.is_error = true;
                cx.notify();
            });
        }
    }
}

impl AuxiliaryWindow {
    pub(super) fn has_unsaved_config(&self, cx: &App) -> bool {
        matches!(&self.content, WindowContent::Editor(editor) if editor.is_dirty(cx))
    }

    fn new(
        owner: Entity<Switcher>,
        kind: WindowKind,
        config: Option<String>,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = match kind {
            WindowKind::About => WindowContent::About,
            WindowKind::NewBackup => WindowContent::NewBackup(BackupForm::new(window, cx)),
            WindowKind::Editor => {
                WindowContent::Editor(ConfigEditor::new(config.unwrap_or_default(), window, cx))
            }
            WindowKind::Settings => {
                WindowContent::Settings(SettingsForm::new(settings, window, cx))
            }
            WindowKind::Versions(name) => WindowContent::Versions(VersionHistory::new(name)),
        };
        let focus = cx.focus_handle();
        let initial_focus = match &content {
            WindowContent::Editor(editor) => editor.state.focus_handle(cx),
            WindowContent::Settings(form) => form.version_limit.focus_handle(cx),
            WindowContent::Versions(_) | WindowContent::About => focus.clone(),
            WindowContent::NewBackup(form) => form.name.focus_handle(cx),
        };
        initial_focus.focus(window, cx);
        let mut subscriptions = vec![cx.observe(&owner, |_, _, cx| cx.notify())];
        subscriptions.push(cx.subscribe_in(
            &owner,
            window,
            |this, _, event: &OperationFinished, window, cx| {
                if event.origin != Some(window.window_handle()) {
                    return;
                }
                if event.success {
                    this.error.clear();
                    this.message = event.message.clone();
                    if event.close_window {
                        window.remove_window();
                    }
                } else {
                    this.error = event.message.clone();
                }
                cx.notify();
            },
        ));
        match &content {
            WindowContent::NewBackup(form) => subscriptions.push(cx.subscribe_in(
                &form.name,
                window,
                |this, _, event, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.error.clear();
                        cx.notify();
                    }
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.create_backup(window, cx);
                    }
                },
            )),
            WindowContent::Editor(editor) => subscriptions.push(cx.subscribe_in(
                &editor.state,
                window,
                |this, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.error.clear();
                        if let WindowContent::Editor(editor) = &mut this.content {
                            editor.discard_pending = false;
                        }
                        cx.notify();
                    }
                },
            )),
            WindowContent::Settings(form) => {
                for input in [&form.version_limit, &form.app_path] {
                    subscriptions.push(cx.subscribe_in(
                        input,
                        window,
                        |this, _, event, window, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.error.clear();
                                cx.notify();
                            }
                            if matches!(event, InputEvent::PressEnter { .. }) {
                                this.save_settings(window, cx);
                            }
                        },
                    ));
                }
            }
            _ => {}
        }
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            view.update(cx, |this, cx| this.allow_close(false, cx))
                .unwrap_or(true)
        });
        Self {
            owner,
            content,
            error: String::new(),
            message: String::new(),
            focus,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn run(
        &mut self,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.owner.read(cx).operation_pending() {
            return;
        }
        self.error.clear();
        self.message.clear();
        self.owner
            .update(cx, |owner, cx| owner.run(operation, window, cx));
        self.message = self.owner.read(cx).message.clone();
        cx.notify();
    }

    fn allow_close(&mut self, discard: bool, cx: &mut Context<Self>) -> bool {
        if self.owner.read(cx).operation_pending() {
            self.error = "正在处理操作，请等待完成后再关闭窗口。".into();
            cx.notify();
            return false;
        }
        if let WindowContent::Editor(editor) = &mut self.content
            && editor.is_dirty(cx)
            && !discard
        {
            editor.discard_pending = true;
            cx.notify();
            return false;
        }
        true
    }

    pub(super) fn request_close(
        &mut self,
        discard: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.allow_close(discard, cx) {
            window.remove_window();
        }
    }

    pub(super) fn render_footer(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (dirty, pending) = match &self.content {
            WindowContent::Editor(editor) => (editor.is_dirty(cx), editor.discard_pending),
            _ => (false, false),
        };
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .p_3()
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .children(pending.then(|| {
                Button::new("continue-editing")
                    .label("继续编辑")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let WindowContent::Editor(editor) = &mut this.content {
                            editor.discard_pending = false;
                            editor.state.focus_handle(cx).focus(window, cx);
                        }
                        cx.notify();
                    }))
            }))
            .children(pending.then(|| {
                Button::new("discard-changes")
                    .label("放弃修改")
                    .danger()
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.request_close(true, window, cx)),
                    )
            }))
            .children(match &self.content {
                WindowContent::NewBackup(form) => Some(
                    Button::new("create-backup")
                        .primary()
                        .icon(IconName::Save)
                        .label("创建并保存")
                        .disabled(
                            !self.owner.read(cx).can_backup()
                                || form.name.read(cx).value().trim().is_empty(),
                        )
                        .loading(busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.create_backup(window, cx)),
                        ),
                ),
                WindowContent::Editor(_) => Some(
                    Button::new("save-config")
                        .primary()
                        .icon(IconName::Save)
                        .label("保存")
                        .disabled(busy || !dirty)
                        .on_click(cx.listener(|this, _, window, cx| this.save_editor(window, cx))),
                ),
                WindowContent::Settings(_) => Some(
                    Button::new("save-settings")
                        .primary()
                        .icon(IconName::Save)
                        .label("保存设置")
                        .disabled(busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.save_settings(window, cx)),
                        ),
                ),
                WindowContent::Versions(_) | WindowContent::About => None,
            })
    }
}

impl Render for AuxiliaryWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.owner.read(cx).operation_pending();
        let body = match &self.content {
            WindowContent::Editor(_) => self.render_editor(busy, cx).into_any_element(),
            WindowContent::Settings(_) => self.render_settings(busy, cx).into_any_element(),
            WindowContent::Versions(_) => self.render_history(busy, cx).into_any_element(),
            WindowContent::NewBackup(_) => self.render_new_backup(busy, cx).into_any_element(),
            WindowContent::About => self.render_about(cx).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .text_sm()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .track_focus(&self.focus)
            .key_context("AuxiliaryWindow")
            .on_action(cx.listener(|this, _: &CloseAuxiliary, window, cx| {
                this.request_close(false, window, cx)
            }))
            .child(body)
            .children((!self.error.is_empty()).then(|| {
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .flex_shrink_0()
                    .text_color(cx.theme().danger)
                    .child(self.error.clone())
            }))
            .children(
                (self.error.is_empty() && !self.message.is_empty()).then(|| {
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(self.message.clone())
                }),
            )
            .children(
                (!matches!(
                    self.content,
                    WindowContent::Versions(_) | WindowContent::About
                ))
                .then(|| self.render_footer(busy, cx)),
            )
    }
}
