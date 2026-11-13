use super::*;

enum ConfirmAction {
    Operation(Operation),
    Exit,
}

impl Switcher {
    pub(super) fn request_backup(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_backup() {
            return;
        }
        let Some(target) = self
            .snapshot
            .profiles
            .iter()
            .find(|profile| profile.key == name)
        else {
            return;
        };
        if target.is_current {
            self.run(Operation::Backup(name), window, cx);
            return;
        }
        let current = self
            .snapshot
            .profiles
            .iter()
            .find(|profile| profile.is_current)
            .map(|profile| format!("当前配置属于“{}”，与目标备份目录不同。", profile.name))
            .unwrap_or_else(|| "当前配置尚未归入任何配置目录。".into());
        self.confirm(
            format!("将当前配置备份到“{name}”？"),
            format!("{current}\n继续将把当前 config.toml 和 auth.json 保存为“{name}”的新版本，并将当前配置关联到该目录。超出保留数量的旧版本会自动清理。"),
            "保存备份",
            ConfirmAction::Operation(Operation::Backup(name)),
            window,
            cx,
        );
    }

    pub(super) fn confirm_switch(
        &mut self,
        name: String,
        version: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_switch_at(name, version, Some(window.window_handle()), cx);
    }

    pub(crate) fn confirm_switch_at(
        &mut self,
        name: String,
        version: Option<String>,
        origin: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if !self.loaded || self.operation_pending() {
            return;
        }
        let Some(profile) = self
            .snapshot
            .profiles
            .iter()
            .find(|profile| profile.key == name)
        else {
            return;
        };
        let (operation, target) = if let Some(id) = version {
            let Some(version) = profile.versions.iter().find(|version| version.id == id) else {
                return;
            };
            if version.problem.is_some() || version.is_current {
                return;
            }
            let target = format!("“{name}”的 {} 版本", version.time);
            (Operation::SwitchVersion(name.clone(), id), target)
        } else {
            if !self.can_switch_profile(&name) {
                return;
            }
            let target = format!("“{name}”的最新备份（{}）", profile.time);
            (Operation::Switch(name.clone()), target)
        };
        self.confirm_with_origin(
            format!("切换到配置“{name}”？"),
            format!("将使用{target}替换当前 config.toml 和 auth.json。切换前会自动备份当前文件，不会自动重启 ChatGPT。"),
            "切换配置",
            ConfirmAction::Operation(operation),
            origin,
            cx,
        );
    }

    pub(crate) fn request_desktop_action(
        &mut self,
        app: DesktopApp,
        action: AppAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_desktop_action_at(app, action, Some(window.window_handle()), cx);
    }

    pub(crate) fn request_desktop_action_at(
        &mut self,
        app: DesktopApp,
        action: AppAction,
        origin: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if matches!(action, AppAction::Open) {
            self.run_with_origin(Operation::Desktop(app, action), origin, cx);
            return;
        }
        let title = format!("{} {}？", action.label(), app.name());
        let detail = if cfg!(target_os = "windows") {
            "将强制结束 ChatGPT 及其子进程，应用内尚未保存的内容可能丢失。"
        } else {
            "将退出 ChatGPT 的所有窗口。请先保存应用内尚未保存的内容。"
        };
        let detail = if matches!(action, AppAction::Restart) {
            format!("{detail} 退出完成后将重新启动应用，加载已保存的配置。")
        } else {
            detail.to_string()
        };
        self.confirm_with_origin(
            title,
            detail,
            action.label(),
            ConfirmAction::Operation(Operation::Desktop(app, action)),
            origin,
            cx,
        );
    }

    pub(super) fn deletion_targets(&self) -> Vec<String> {
        if self.multi_select {
            self.checked_profiles.iter().cloned().collect()
        } else {
            self.selected.iter().cloned().collect()
        }
    }

    pub(super) fn confirm_delete_profiles(
        &mut self,
        names: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if names.is_empty() || !self.loaded {
            return;
        }
        let count: usize = self
            .snapshot
            .profiles
            .iter()
            .filter(|p| names.contains(&p.key))
            .map(|p| p.version_count)
            .sum();
        let title = if names.len() == 1 {
            format!("删除备份“{}”？", names[0])
        } else {
            format!("删除选中的 {} 个备份目录？", names.len())
        };
        let preview = names.iter().take(8).cloned().collect::<Vec<_>>().join("、");
        let detail = format!(
            "{}{}\n将永久删除这些目录及其中 {count} 个版本，无法恢复。当前使用的 config.toml 和 auth.json 会保留。",
            preview,
            if names.len() > 8 { "等" } else { "" }
        );
        self.confirm(
            title,
            detail,
            "删除备份",
            ConfirmAction::Operation(Operation::DeleteProfiles(names)),
            window,
            cx,
        );
    }

    pub(super) fn confirm_delete_version(
        &mut self,
        name: String,
        id: String,
        time: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm(
            format!("删除“{name}”的 {time} 版本？"),
            "删除后无法恢复，当前使用的配置文件会保留。".into(),
            "删除版本",
            ConfirmAction::Operation(Operation::DeleteVersion(name, id)),
            window,
            cx,
        );
    }

    pub(crate) fn request_exit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.prompt_open {
            self.message = "正在处理操作或等待确认，请完成后再退出。".into();
            cx.notify();
            return;
        }
        if self.exit_needs_confirmation(cx) {
            self.confirm(
                "退出 Codex Switch？".into(),
                "config.toml 编辑器有未保存的修改，退出将丢弃这些修改。".into(),
                "丢弃修改并退出",
                ConfirmAction::Exit,
                window,
                cx,
            );
        } else {
            self.shutdown_and_quit(cx);
        }
    }

    pub(crate) fn shutdown_and_quit(&mut self, cx: &mut Context<Self>) {
        if self.operation_pending() {
            return;
        }
        self.busy = true;
        self.app_revision += 1;
        self.message = "正在停止后台任务并退出…".into();
        cx.notify();
        let service = self.theme_service.clone();
        let job = cx.background_spawn(async move { service.shutdown() });
        cx.spawn(async move |this, cx| match job.await {
            Ok(()) => cx.update(|cx| cx.quit()),
            Err(error) => {
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    this.refresh_themes(cx);
                    this.show_theme_error(format!("无法停止主题后台任务：{error:#}"), cx);
                });
            }
        })
        .detach();
    }

    pub(crate) fn exit_needs_confirmation(&self, cx: &App) -> bool {
        self.editor_window
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|editor| editor.read(cx).has_unsaved_config(cx))
    }

    fn confirm(
        &mut self,
        title: String,
        detail: String,
        label: &str,
        action: ConfirmAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_with_origin(
            title,
            detail,
            label,
            action,
            Some(window.window_handle()),
            cx,
        );
    }

    fn confirm_with_origin(
        &mut self,
        title: String,
        detail: String,
        label: &str,
        action: ConfirmAction,
        origin: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.prompt_open {
            return;
        }
        self.prompt_open = true;
        let owner = cx.entity();
        let label = label.to_string();
        cx.notify();
        // Native prompts can change focus synchronously. Release the owner lease
        // before calling the platform API, just as for independent windows.
        cx.defer(move |cx| {
            let prompt = crate::native_prompt::confirm(title, detail, label, origin, cx);
            cx.spawn(async move |cx| {
                let answer = prompt.await;
                let confirmed = matches!(answer, Ok(true));
                cx.update(|cx| {
                    owner.update(cx, |owner, cx| {
                        owner.prompt_open = false;
                        if let Err(error) = &answer {
                            owner.message = format!("操作未执行：{error:#}");
                            owner.is_error = true;
                        }
                        cx.notify();
                    });
                    if !confirmed {
                        return;
                    }
                    match action {
                        ConfirmAction::Exit => {
                            owner.update(cx, |owner, cx| owner.shutdown_and_quit(cx))
                        }
                        ConfirmAction::Operation(operation) => {
                            owner.update(cx, |owner, cx| {
                                owner.run_with_origin(operation, origin, cx);
                            });
                        }
                    }
                });
            })
            .detach();
        });
    }
}
