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
            .map(|profile| crate::tr!("dialogs.the_current_configuration_belongs_to_which_is_not_the"; arg0 = profile.name))
            .unwrap_or_else(|| crate::tr!("dialogs.the_current_configuration_is_not_assigned_to_a_profile").into());
        self.confirm(
            crate::tr!("dialogs.back_up_the_current_configuration_to"; name = name),
            crate::tr!("dialogs.nthis_will_save_the_current_config_toml_and_auth"; current = current, name = name),
            crate::tr!("dialogs.save_backup"),
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
            let target = crate::tr!("dialogs.the_version_of"; name = name, arg0 = version.time);
            (Operation::SwitchVersion(name.clone(), id), target)
        } else {
            if !self.can_switch_profile(&name) {
                return;
            }
            let target =
                crate::tr!("dialogs.the_latest_backup_of"; name = name, arg0 = profile.time);
            (Operation::Switch(name.clone()), target)
        };
        self.confirm_with_origin(
            crate::tr!("dialogs.switch_to_profile"; name = name),
            crate::tr!("dialogs.replace_the_current_config_toml_and_auth_json_with"; target = target),
            crate::tr!("shell.switch_profile"),
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
            crate::tr!("dialogs.this_will_force_stop_chatgpt_and_its_child_processes")
        } else {
            crate::tr!("dialogs.this_will_quit_all_chatgpt_windows_save_any_unsaved")
        };
        let detail = if matches!(action, AppAction::Restart) {
            crate::tr!("dialogs.the_app_will_restart_afterward_to_load_the_saved"; detail = detail)
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
            crate::tr!("dialogs.delete_backup"; arg0 = names[0])
        } else {
            crate::tr!("dialogs.delete_the_selected_backup_directories"; arg0 = names.len())
        };
        let preview = names.iter().take(8).cloned().collect::<Vec<_>>().join("、");
        let detail = crate::tr!("dialogs.nthese_directories_and_their_versions_will_be_permanently_deleted"; arg0 = preview, arg1 = if names.len() > 8 { crate::tr!("dialogs.and_others") } else { "" }, count = count);
        self.confirm(
            title,
            detail,
            crate::tr!("dialogs.delete_backups"),
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
            crate::tr!("dialogs.delete_the_version_of"; name = name, time = time),
            crate::tr!("dialogs.this_cannot_be_undone_the_current_configuration_files_will").into(),
            crate::tr!("dialogs.delete_version"),
            ConfirmAction::Operation(Operation::DeleteVersion(name, id)),
            window,
            cx,
        );
    }

    pub(crate) fn request_exit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.prompt_open {
            self.message =
                crate::tr!("dialogs.an_operation_or_confirmation_is_pending_finish_it_before")
                    .into();
            cx.notify();
            return;
        }
        if self.exit_needs_confirmation(cx) {
            self.confirm(
                crate::tr!("dialogs.quit_codex_switch").into(),
                crate::tr!("dialogs.the_config_toml_editor_has_unsaved_changes_quitting_will")
                    .into(),
                crate::tr!("dialogs.discard_and_quit"),
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
        self.message = crate::tr!("dialogs.stopping_background_tasks_and_quitting").into();
        cx.notify();
        let service = self.theme_service.clone();
        let job = cx.background_spawn(async move { service.shutdown() });
        cx.spawn(async move |this, cx| match job.await {
            Ok(()) => cx.update(|cx| cx.quit()),
            Err(error) => {
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    this.refresh_themes(cx);
                    this.show_theme_error(crate::tr!("dialogs.could_not_stop_the_theme_worker"; error = format!("{:#}", error)), cx);
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
                            owner.message = crate::tr!("dialogs.the_operation_was_not_performed"; error = format!("{:#}", error));
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
