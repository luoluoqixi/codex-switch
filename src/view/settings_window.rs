use super::auxiliary_window::{AuxiliaryWindow, WindowContent};
use super::*;
use crate::i18n::Language;
use gpui_kit::component::{
    radio::{Radio, RadioGroup},
    scroll::ScrollableElement,
    switch::Switch,
};

pub(super) struct SettingsForm {
    pub(super) version_limit: Entity<InputState>,
    pub(super) app_path: Entity<InputState>,
    pub(super) theme_directory: Entity<InputState>,
    launch_at_login: bool,
    exit_on_close: bool,
    language: Language,
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
            language: settings.language,
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
                        language: form.language,
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
                self.error =
                    crate::tr!("settings_window.enter_an_integer_between_1_and_1000").into();
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
                        crate::tr!("settings_window.choose_theme_directory")
                    } else if cfg!(target_os = "macos") {
                        crate::tr!("settings_window.choose_chatgpt_app")
                    } else {
                        crate::tr!("settings_window.choose_chatgpt_exe")
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
                                Ok(Err(error)) => child.error = crate::tr!("settings_window.could_not_choose_the_path"; error = format!("{:#}", error)),
                                Err(_) => child.error = crate::tr!("settings_window.the_system_file_picker_returned_no_result_try_again").into(),
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
                .child(div().text_lg().font_semibold().child(crate::tr!("auxiliary_window.settings")))
                .child(div().font_semibold().mt_2().child(crate::tr!("settings_window.language")))
                .child(RadioGroup::new("language").layout(Axis::Horizontal)
                    .selected_index(Some(if form.language == Language::English { 0 } else { 1 }))
                    .children([
                        Radio::new("language-en").label("English"),
                        Radio::new("language-zh-cn").label("简体中文"),
                    ])
                    .disabled(busy)
                    .on_change(cx.listener(|this, index, _, cx| {
                        if let WindowContent::Settings(form) = &mut this.content {
                            form.language = if *index == 0 { Language::English } else { Language::SimplifiedChinese };
                        }
                        this.error.clear();
                        cx.notify();
                    })))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(crate::tr!("settings_window.language_help")))
                .child(div().font_semibold().mt_2().child(crate::tr!("settings_window.startup_and_closing")))
                .child(Switch::new("launch-at-login").label(crate::tr!("settings_window.launch_codex_switch_at_login"))
                    .checked(form.launch_at_login).disabled(busy)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let WindowContent::Settings(form) = &mut this.content { form.launch_at_login = *checked; }
                        this.error.clear(); cx.notify();
                    })))
                .child(div().text_xs().text_color(cx.theme().muted_foreground))
                .child(Switch::new("exit-on-close").label(crate::tr!("settings_window.quit_when_the_main_window_closes"))
                    .checked(form.exit_on_close).disabled(busy)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        if let WindowContent::Settings(form) = &mut this.content { form.exit_on_close = *checked; }
                        this.error.clear(); cx.notify();
                    })))
                .child(div().text_xs().text_color(cx.theme().muted_foreground))
                .child(div().font_semibold().mt_2().child(crate::tr!("settings_window.back_up")))
                .child(div().text_sm().child(crate::tr!("settings_window.versions_to_keep_per_profile")))
                .child(Input::new(&form.version_limit).id("max-versions")
                    .aria_label(crate::tr!("settings_window.versions_to_keep_per_profile_1_to_1000")).disabled(busy))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(crate::tr!("settings_window.default_10_range_1_1000_applies_to_all_profiles")))
                .child(div().text_xs().text_color(cx.theme().warning)
                    .child(crate::tr!("settings_window.removed_versions_cannot_be_recovered_the_current_configuration_files")))
                .child(div().font_semibold().mt_2().child(crate::tr!("settings_window.desktop_app")))
                .child(div().text_sm().child(crate::tr!("settings_window.chatgpt_app_path_optional")))
                .child(div().flex().items_center().gap_2()
                    .child(Input::new(&form.app_path).id("chatgpt-app-path")
                        .flex_1().min_w_0().aria_label(crate::tr!("settings_window.chatgpt_app_path_leave_blank_to_detect_automatically")).disabled(busy))
                    .child(Button::new("choose-chatgpt-app").outline().icon(IconName::FolderOpen)
                        .label(crate::tr!("settings_window.choose")).disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.choose_settings_path(false, window, cx)))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(if cfg!(target_os = "macos") { crate::tr!("settings_window.leave_blank_to_detect_automatically_or_select_chatgpt_app") } else { crate::tr!("settings_window.leave_blank_to_detect_automatically_or_select_chatgpt_exe") }))
                .child(div().font_semibold().mt_2().child(crate::tr!("settings_window.theme")))
                .child(div().text_sm().child(crate::tr!("settings_window.theme_directory_optional")))
                .child(div().flex().items_center().gap_2()
                    .child(Input::new(&form.theme_directory).id("theme-directory").flex_1().min_w_0()
                        .aria_label(crate::tr!("settings_window.theme_directory_leave_blank_to_use_the_themes_folder")).disabled(busy))
                    .child(Button::new("choose-theme-directory").outline().icon(IconName::FolderOpen)
                        .label(crate::tr!("settings_window.choose")).disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.choose_settings_path(true, window, cx)))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(crate::tr!("settings_window.leave_blank_to_use_the_themes_directory_beside_the")))
        )
    }
}
