use super::*;
use gpui_kit::prelude::FluentBuilder;

impl Switcher {
    pub(super) fn refresh_apps(&mut self, cx: &mut Context<Self>) {
        if !self.loaded || self.busy || self.checking_apps {
            return;
        }
        self.checking_apps = true;
        let revision = self.app_revision;
        let paths = self.snapshot.settings.app_paths();
        let job = cx.background_spawn(async move { desktop::inspect(&paths) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                this.checking_apps = false;
                if revision != this.app_revision || this.busy {
                    this.refresh_apps(cx);
                    return;
                }
                match result {
                    Ok(statuses) => {
                        this.app_statuses = statuses;
                        this.app_error.clear();
                    }
                    Err(error) => {
                        this.app_statuses.clear();
                        this.app_error = format!("{error:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .max_w_64()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .truncate()
                                    .child(crate::app_title()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(self.store.root().display().to_string()),
                            ),
                    )
                    .child(self.render_app_controls(DesktopApp::ChatGpt, cx))
                    .child(div().flex_1())
                    .child(
                        Button::new("open-root")
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .accessibility_label("打开 Codex 配置目录")
                            .tooltip("打开 Codex 配置目录")
                            .on_click(
                                cx.listener(|this, _, _, cx| {
                                    cx.open_with_system(this.store.root())
                                }),
                            ),
                    )
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .accessibility_label("刷新配置列表")
                            .tooltip(if cfg!(target_os = "macos") {
                                "刷新配置列表（⌘R / F5）"
                            } else {
                                "刷新配置列表（Ctrl+R / F5）"
                            })
                            .disabled(self.busy || self.prompt_open)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if !this.prompt_open {
                                    this.run(Operation::Refresh, window, cx);
                                }
                            })),
                    )
                    .child(self.render_app_menu(cx)),
            )
            .children((!self.app_error.is_empty()).then(|| {
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(self.app_error.clone())
            }))
    }

    fn render_app_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        Button::new("app-menu")
            .ghost()
            .icon(IconName::Menu)
            .accessibility_label("应用菜单：设置和关于")
            .dropdown_menu(move |menu, _, cx| {
                let Some(owner) = view.upgrade() else {
                    return menu;
                };
                let owner = owner.read(cx);
                let settings_disabled = owner.busy || !owner.loaded || owner.prompt_open;
                let settings = view.clone();
                let about = view.clone();
                menu.item(
                    PopupMenuItem::new("设置…")
                        .disabled(settings_disabled)
                        .on_click(move |_, _, cx| {
                            let _ = settings.update(cx, |this, cx| {
                                if !this.busy && this.loaded && !this.prompt_open {
                                    this.show_auxiliary_window(WindowKind::Settings, None, cx);
                                }
                            });
                        }),
                )
                .separator()
                .item(PopupMenuItem::new("关于…").on_click(move |_, _, cx| {
                    let _ = about.update(cx, |this, cx| {
                        this.show_auxiliary_window(WindowKind::About, None, cx);
                    });
                }))
            })
    }

    fn render_app_controls(&self, app: DesktopApp, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.app_statuses.iter().find(|status| status.app() == app);
        let running = status.is_some_and(AppStatus::is_running);
        let problem = status.and_then(AppStatus::problem);
        let unavailable =
            self.operation_pending() || !self.loaded || status.is_none() || problem.is_some();
        let label = if problem.is_some() {
            "路径无效"
        } else if running {
            "已打开"
        } else if status.is_some_and(AppStatus::is_installed) {
            "未打开"
        } else if status.is_some() {
            "未找到"
        } else if self.checking_apps || !self.loaded {
            "检查中…"
        } else {
            "状态未知"
        };
        let actions: &[AppAction] = if running {
            &[AppAction::Close, AppAction::Restart]
        } else {
            &[AppAction::Open]
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .flex_shrink_0()
            .child(
                div()
                    .text_xs()
                    .text_color(if problem.is_some() {
                        cx.theme().warning
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(if running && problem.is_none() {
                        format!(
                            "{} · {label} · 调试入口{}",
                            app.name(),
                            if status.is_some_and(AppStatus::has_debug_endpoint) {
                                "已开启"
                            } else {
                                "未开启"
                            }
                        )
                    } else {
                        format!("{} · {label}", app.name())
                    }),
            )
            .children(actions.iter().copied().map(|action| {
                let disabled = unavailable
                    || (!matches!(action, AppAction::Close)
                        && !status.is_some_and(AppStatus::is_installed));
                Button::new(SharedString::from(format!(
                    "desktop:{}:{}",
                    app.name(),
                    action.label()
                )))
                .ghost()
                .outline()
                .when(matches!(action, AppAction::Open), |button| {
                    button.label(action.label())
                })
                .when(matches!(action, AppAction::Close), |button| {
                    button.icon(IconName::Power)
                })
                .when(matches!(action, AppAction::Restart), |button| {
                    button.icon(IconName::RotateCw)
                })
                .disabled(disabled)
                .accessibility_label(format!("{} {}", action.label(), app.name()))
                .tooltip(problem.map(str::to_owned).unwrap_or_else(|| {
                    if !status.is_some_and(AppStatus::is_installed) {
                        "未找到应用，可在设置中指定路径".into()
                    } else {
                        match action {
                            AppAction::Open => format!("打开 {} 桌面应用", app.name()),
                            AppAction::Close => {
                                if cfg!(target_os = "windows") {
                                    format!("关闭 {}（强制结束应用及其子进程）", app.name())
                                } else {
                                    format!("关闭 {}（退出桌面应用的所有窗口）", app.name())
                                }
                            }
                            AppAction::Restart => {
                                if cfg!(target_os = "windows") {
                                    format!(
                                        "重新打开 {}（强制结束应用及其子进程，等待退出后启动）",
                                        app.name()
                                    )
                                } else {
                                    format!("重新打开 {}（退出后重新启动桌面应用）", app.name())
                                }
                            }
                        }
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !this.prompt_open {
                        this.request_desktop_action(app, action, window, cx);
                    }
                }))
            }))
            .child(
                Button::new("open-chatgpt-installation")
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .accessibility_label("在安装目录中选中 ChatGPT 应用")
                    .tooltip(if status.is_some_and(AppStatus::has_installation_path) {
                        if cfg!(target_os = "macos") {
                            "在 Finder 中选中 ChatGPT.app"
                        } else {
                            "在资源管理器中选中 ChatGPT.exe"
                        }
                    } else {
                        "未找到安装路径，可在设置中指定 ChatGPT 应用路径"
                    })
                    .disabled(unavailable || !status.is_some_and(AppStatus::has_installation_path))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if !this.prompt_open {
                            this.run(Operation::OpenInstallation(app), window, cx);
                        }
                    })),
            )
    }
}
