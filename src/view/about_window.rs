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
        div().flex_1().min_h_0().overflow_y_scrollbar().child(
            div()
                .flex()
                .flex_col()
                .p_4()
                .gap_3()
                .child(div().text_lg().font_semibold().child(crate::APP_NAME))
                .child(div().child(format!("版本 {}", crate::APP_VERSION)))
                .child(div().child("本地 Codex 配置切换与备份工具"))
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
                        .accessibility_label("在浏览器中打开 GitHub 项目 luoluoqixi/codex-switch")
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
