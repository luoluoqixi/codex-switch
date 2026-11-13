use super::auxiliary_window::{AuxiliaryWindow, WindowContent};
use super::*;

pub(super) struct BackupForm {
    pub(super) name: Entity<InputState>,
}

impl BackupForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<AuxiliaryWindow>) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("例如：官方、工作、备用")),
        }
    }
}

impl AuxiliaryWindow {
    pub(super) fn create_backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.owner.read(cx).can_backup() {
            return;
        }
        let WindowContent::NewBackup(form) = &self.content else {
            return;
        };
        let name = form.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.error = "请输入配置名称。".into();
            cx.notify();
            return;
        }
        self.run(Operation::Create(name), window, cx);
    }

    pub(super) fn render_new_backup(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let WindowContent::NewBackup(form) = &self.content else {
            unreachable!()
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p_4()
            .gap_2()
            .child(div().text_sm().child("配置名称"))
            .child(
                Input::new(&form.name)
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
    }
}
