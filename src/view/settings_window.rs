use super::auxiliary_window::{AuxiliaryWindow, WindowContent};
use super::*;
use gpui_kit::component::{scroll::ScrollableElement, switch::Switch};

pub(super) struct SettingsForm {
    pub(super) version_limit: Entity<InputState>,
    pub(super) app_path: Entity<InputState>,
    pub(super) theme_directory: Entity<InputState>,
    launch_at_login: bool,
    exit_on_close: bool,
}

impl SettingsForm {
    pub(super) fn new(
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<AuxiliaryWindow>,
    ) -> Self {
        Self {
            launch_at_login: settings.launch_at_login,
            exit_on_close: settings.exit_on_close,
            version_limit: cx.new(|cx| {
                InputState::new(window, cx).default_value(settings.max_versions.to_string())
            }),
            app_path: cx
                .new(|cx| InputState::new(window, cx).default_value(settings.chatgpt_app_path)),
            theme_directory: cx
                .new(|cx| InputState::new(window, cx).default_value(settings.theme_directory)),
        }
    }
}

impl AuxiliaryWindow {
    pub(super) fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.owner.read(cx).operation_pending() {
            return;
        }
        let WindowContent::Settings(form) = &self.content else {
            return;
        };
        let count = form.version_limit.read(cx).value().trim().parse::<usize>();
        match count {
            Ok(max_versions) if (1..=1000).contains(&max_versions) => {
                let chatgpt_app_path = form
                    .app_path
                    .read(cx)
                    .value()
                    .trim()
                    .trim_matches('"')
                    .to_string();
                self.run(
                    Operation::Settings(Settings {
                        max_versions,
                        chatgpt_app_path,
                        theme_directory: form
                            .theme_directory
                            .read(cx)
                            .value()
                            .trim()
                            .trim_matches('"')
                            .to_string(),
                        launch_at_login: form.launch_at_login,
                        exit_on_close: form.exit_on_close,
                    }),
                    window,
                    cx,
                );
            }
            _ => {
                self.error = "请输入 1–1000 之间的整数。".into();
                cx.notify();
            }
        }
    }

    fn choose_settings_path(
        &mut self,
        theme_directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.owner.read(cx).operation_pending() {
            return;
        }
        self.owner.update(cx, |owner, cx| {
            owner.prompt_open = true;
            cx.notify();
        });
        let owner = self.owner.clone();
        let child = cx.entity().downgrade();
        let origin = window.window_handle();
        cx.defer(move |cx| {
            // .app bundles are selectable as files in NSOpenPanel; selecting a
            // parent folder is deliberately not accepted by path validation.
            let receiver = cx.prompt_for_paths(PathPromptOptions {
                files: !theme_directory,
                directories: theme_directory,
                multiple: false,
                prompt: Some(
                    if theme_directory {
                        "选择主题目录"
                    } else if cfg!(target_os = "macos") {
                        "选择 ChatGPT.app"
                    } else {
                        "选择 ChatGPT.exe"
                    }
                    .into(),
                ),
            });
            cx.spawn(async move |cx| {
                let result = receiver.await;
                cx.update(|cx| {
                    owner.update(cx, |owner, cx| {
                        owner.prompt_open = false;
                        cx.notify();
                    });
                    let _ = cx.update_window(origin, |_, window, cx| {
                        let _ = child.update(cx, |child, cx| {
                            match result {
                                Ok(Ok(Some(paths))) => {
                                    if let Some(path) = paths.first() {
                                        let text = path.to_string_lossy().to_string();
                                        match if theme_directory {
                                            Ok(())
                                        } else {
                                            desktop::validate_paths(std::slice::from_ref(&text))
                                        } {
                                            Ok(()) => {
                                                if let WindowContent::Settings(form) =
                                                    &child.content
                                                {
                                                    let input = if theme_directory {
                                                        &form.theme_directory
                                                    } else {
                                                        &form.app_path
                                                    };
                                                    input.update(cx, |input, cx| {
                                                        input.set_value(text, window, cx)
                                                    });
                                                }
                                                child.error.clear();
                                            }
                                            Err(error) => child.error = format!("{error:#}"),
                                        }
                                    }
                                }
                                Ok(Ok(None)) => {}
                                Ok(Err(error)) => child.error = format!("无法选择路径：{error:#}"),
                                Err(_) => child.error = "系统文件选择器未返回结果，请重试。".into(),
                            }
                            cx.notify();
                        });
                    });
                });
            })
            .detach();
        });
    }

    pub(super) fn render_settings(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let WindowContent::Settings(form) = &self.content else {
            unreachable!()
        };
        div().flex_1().min_h_0().overflow_y_scrollbar().child(
            div().flex().flex_col().gap_2().p_4()
                .child(div().text_lg().font_semibold().child("全局设置"))
                                .child(div().font_semibold().mt_2().child("启动与关闭"))
                .child(Switch::new("launch-at-login").label("开机启动 Codex Switch")
                    .checked(form.launch_at_login).disabled(busy)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let WindowContent::Settings(form) = &mut this.content { form.launch_at_login = *checked; }
                        this.error.clear(); cx.notify();
                    })))
                .child(div().text_xs().text_color(cx.theme().muted_foreground))
                .child(Switch::new("exit-on-close").label("关闭主窗口时退出程序")
                    .checked(form.exit_on_close).disabled(busy)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let WindowContent::Settings(form) = &mut this.content { form.exit_on_close = *checked; }
                        this.error.clear(); cx.notify();
                    })))
                .child(div().text_xs().text_color(cx.theme().muted_foreground))
                .child(div().font_semibold().mt_2().child("备份"))
                .child(div().text_sm().child("每个配置保留的版本数"))
                .child(Input::new(&form.version_limit).id("max-versions")
                    .aria_label("每个配置保留的版本数，1 到 1000").disabled(busy))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child("默认 10，范围 1–1000。适用于所有配置及自动备份。保存设置后立即清理超出的旧版本，以后每次备份也会自动清理。"))
                .child(div().text_xs().text_color(cx.theme().warning)
                    .child("清理的旧版本无法恢复，当前配置文件会保留。"))
                .child(div().font_semibold().mt_2().child("桌面应用"))
                .child(div().text_sm().child("ChatGPT 应用路径（可选）"))
                .child(div().flex().items_center().gap_2()
                    .child(Input::new(&form.app_path).id("chatgpt-app-path")
                        .flex_1().min_w_0().aria_label("ChatGPT 应用路径，留空自动查找").disabled(busy))
                    .child(Button::new("choose-chatgpt-app").outline().icon(IconName::FolderOpen)
                        .label("选择…").disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.choose_settings_path(false, window, cx)))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(if cfg!(target_os = "macos") { "留空自动查找；或选择 ChatGPT.app。" } else { "留空自动查找；或选择 ChatGPT.exe。" }))
                .child(div().font_semibold().mt_2().child("主题"))
                .child(div().text_sm().child("主题目录（可选）"))
                .child(div().flex().items_center().gap_2()
                    .child(Input::new(&form.theme_directory).id("theme-directory").flex_1().min_w_0()
                        .aria_label("主题目录，留空使用程序旁的 Themes 文件夹").disabled(busy))
                    .child(Button::new("choose-theme-directory").outline().icon(IconName::FolderOpen)
                        .label("选择…").disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.choose_settings_path(true, window, cx)))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child("留空使用程序所在文件夹中的 Themes 目录。导入 ZIP 仅解压；手动解压到此目录的主题也会自动出现在列表中。"))
        )
    }
}
