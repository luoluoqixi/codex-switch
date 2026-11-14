use super::auxiliary_window::{AuxiliaryWindow, WindowContent};
use super::*;
use gpui_kit::component::input::{Editor, EditorState};

pub(super) struct ConfigEditor {
    pub(super) state: Entity<EditorState>,
    original: String,
    pub(super) discard_pending: bool,
}

impl ConfigEditor {
    pub(super) fn new(
        original: String,
        window: &mut Window,
        cx: &mut Context<AuxiliaryWindow>,
    ) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("toml")
                .default_value(original.clone())
        });
        Self {
            state,
            original,
            discard_pending: false,
        }
    }

    pub(super) fn is_dirty(&self, cx: &App) -> bool {
        self.state.read(cx).value().as_ref() != self.original
    }
}

impl AuxiliaryWindow {
    pub(super) fn save_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let WindowContent::Editor(editor) = &self.content else {
            return;
        };
        if !editor.is_dirty(cx) {
            return;
        }
        self.run(
            Operation::SaveConfig(
                editor.original.clone(),
                editor.state.read(cx).value().to_string(),
            ),
            window,
            cx,
        );
    }

    pub(super) fn render_editor(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let WindowContent::Editor(editor) = &self.content else {
            unreachable!()
        };
        let root = self.owner.read(cx).store.root().join("config.toml");
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p_3()
            .gap_2()
            .key_context("ConfigEditor")
            .on_action(
                cx.listener(|this, _: &SaveConfigFile, window, cx| this.save_editor(window, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_shrink_0()
                    .child(div().flex_1().min_w_0().font_semibold().truncate().child(
                        if editor.is_dirty(cx) {
                            crate::tr!("config_editor.config_toml_unsaved")
                        } else {
                            "config.toml"
                        },
                    ))
                    .child(
                        Button::new("editor-open-folder")
                            .outline()
                            .icon(IconName::FolderOpen)
                            .flex_shrink_0()
                            .tooltip(crate::tr!(
                                "config_editor.open_the_folder_containing_config_toml"
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let path = this.owner.read(cx).store.root().to_path_buf();
                                cx.defer(move |cx| cx.open_with_system(&path));
                            })),
                    )
                    .child(
                        Button::new("editor-open-externally")
                            .outline()
                            .icon(IconName::ExternalLink)
                            .flex_shrink_0()
                            .disabled(busy)
                            .tooltip(crate::tr!(
                                "config_editor.open_config_toml_from_disk_with_the_default_app"
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let path = this.owner.read(cx).store.root().join("config.toml");
                                cx.defer(move |cx| cx.open_with_system(&path));
                            })),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(root.display().to_string()),
            )
            .child(
                Editor::new(&editor.state)
                    .h(relative(1.))
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .readonly(busy)
                    .aria_label(crate::tr!("config_editor.current_config_toml_editor")),
            )
            .children(editor.discard_pending.then(|| {
                div()
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .child(crate::tr!(
                        "config_editor.you_have_unsaved_changes_save_before_closing_or_choose"
                    ))
            }))
    }
}
