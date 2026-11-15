use super::auxiliary_window::AuxiliaryWindow;
use super::*;
use gpui_kit::base::Link;
use gpui_kit::component::scroll::ScrollableElement;

impl AuxiliaryWindow {
    pub(super) fn render_about(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let platform = if cfg!(target_os = "windows") {
            "Windows"
        } else if cfg!(target_os = "macos") {
            "macOS"
        } else {
            std::env::consts::OS
        };
        let checking = self.owner.read(cx).updates.is_checking();
        div().flex_1().min_h_0().overflow_y_scrollbar().child(
            div()
                .flex()
                .flex_col()
                .p_4()
                .gap_3()
                .child(div().text_lg().font_semibold().child(crate::APP_NAME))
                .child(div().child(crate::tr!("about_window.version"; arg0 = crate::APP_VERSION)))
                .child(
                    Button::new("check-for-updates")
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(if checking {
                            crate::tr!("update.checking")
                        } else {
                            crate::tr!("update.check_for_updates")
                        })
                        .loading(checking)
                        .disabled(checking || self.owner.read(cx).operation_pending())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.owner.update(cx, |owner, cx| {
                                owner.check_updates(window.window_handle(), cx)
                            });
                        })),
                )
                .child(div().child(crate::tr!(
                    "about_window.a_local_codex_profile_switcher_and_backup_tool"
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{platform} · {} · GPUI Kit",
                            std::env::consts::ARCH
                        )),
                )
                .child(
                    Link::new("about-github")
                        .href(crate::APP_REPOSITORY)
                        .open_with(|url, _, _, cx| cx.open_url(url))
                        .accessibility_label(crate::tr!(
                            "about_window.open_luoluoqixi_codex_switch_on_github_in_your_browser"
                        ))
                        .text_color(cx.theme().link)
                        .text_decoration_1()
                        .text_decoration_color(cx.theme().link)
                        .cursor_pointer()
                        .rounded_sm()
                        .px_1()
                        .py_1()
                        .hover(|style| style.bg(cx.theme().muted))
                        .focus_visible(|style| style.bg(cx.theme().muted))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(IconName::Github)
                                .child("GitHub · luoluoqixi/codex-switch"),
                        ),
                ),
        )
    }
}
