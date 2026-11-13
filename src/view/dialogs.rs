use super::*;
use crate::store::BackupVersion;
use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::prelude::FluentBuilder;

impl Switcher {
    pub(super) fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !matches!(self.modal, Some(Modal::Settings)) {
            return;
        }
        let count = self.version_limit.read(cx).value().trim().parse::<usize>();
        match count {
            Ok(max_versions) if (1..=1000).contains(&max_versions) => {
                let paths = self
                    .app_paths
                    .each_ref()
                    .map(|input| input.read(cx).value().trim().trim_matches('"').to_string());
                self.run(
                    Operation::Settings(Settings {
                        max_versions,
                        chatgpt_app_path: paths[0].clone(),
                    }),
                    window,
                    cx,
                );
            }
            _ => {
                self.dialog_error = "请输入 1–1000 之间的整数。".into();
                cx.notify();
                window.refresh();
            }
        }
    }

    fn close_management_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.modal = None;
        self.pending_delete = None;
        self.dialog_error.clear();
        window.close_dialog(cx);
        cx.notify();
    }

    pub(super) fn show_management_dialog(
        &mut self,
        modal: Modal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || !self.loaded || self.modal.is_some() {
            return;
        }
        self.dialog_error.clear();
        self.pending_delete = None;
        self.message.clear();
        self.is_error = false;
        self.modal = Some(modal.clone());
        self.modal_revision += 1;
        let revision = self.modal_revision;
        let view = cx.entity().downgrade();
        if let Modal::Delete(name) = modal {
            window.open_alert_dialog(cx, move |dialog, _, cx| {
                let Some(entity) = view.upgrade() else { return dialog; };
                let this = entity.read(cx);
                let busy = this.busy;
                let count = this.snapshot.profiles.iter().find(|p| p.key == name)
                    .map_or(0, |p| p.version_count);
                let cancel = view.clone();
                let delete = view.clone();
                let close = view.clone();
                let name = name.clone();
                dialog.title(format!("删除备份“{name}”？"))
                    .description(format!("将永久删除该目录及其中 {count} 个版本，无法恢复。当前使用的 config.toml 和 auth.json 会保留。"))
                    .centered().keyboard(!busy).close_button(!busy)
                    .on_cancel(move |_, _, _| !busy)
                    .on_ok(|_, _, _| false)
                    .children((!this.dialog_error.is_empty()).then(|| {
                        div().text_sm().text_color(cx.theme().danger).child(this.dialog_error.clone())
                    }))
                    .footer(DialogFooter::new()
                        .child(Button::new("cancel-delete-profile").label("取消").disabled(busy)
                            .on_click(move |_, window, cx| {
                                let _ = cancel.update(cx, |this, cx| this.close_management_dialog(window, cx));
                            }))
                        .child(Button::new("confirm-delete-profile").danger().label("删除备份")
                            .disabled(busy).loading(busy)
                            .on_click(move |_, window, cx| {
                                let _ = delete.update(cx, |this, cx| {
                                    this.run(Operation::DeleteProfile(name.clone()), window, cx);
                                });
                            })))
                    .on_close(move |_, _, cx| {
                        let view = close.clone();
                        cx.defer(move |cx| { let _ = view.update(cx, |this, cx| {
                            if this.modal_revision == revision {
                                this.modal = None;
                                cx.notify();
                            }
                        }); });
                    })
            });
        } else {
            if matches!(modal, Modal::Settings) {
                let count = self.snapshot.settings.max_versions.to_string();
                self.version_limit
                    .update(cx, |input, cx| input.set_value(count, window, cx));
                for (input, value) in self
                    .app_paths
                    .iter()
                    .zip(self.snapshot.settings.app_paths())
                {
                    input.update(cx, |input, cx| input.set_value(value, window, cx));
                }
            } else {
                self.version_scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            let initial_focus = Cell::new(true);
            window.open_dialog(cx, move |dialog, window, cx| {
                let Some(entity) = view.upgrade() else { return dialog; };
                if initial_focus.replace(false) && matches!(modal, Modal::Settings) {
                    let input = entity.read(cx).version_limit.clone();
                    input.focus_handle(cx).focus(window, cx);
                }
                let this = entity.read(cx);
                let busy = this.busy;
                let cancel = view.clone();
                let close = view.clone();
                let confirm = view.clone();
                let settings_dialog = matches!(modal, Modal::Settings);
                let mut footer = DialogFooter::new()
                    .child(Button::new("close-management").label(if matches!(modal, Modal::Settings) { "取消" } else { "关闭" })
                        .disabled(busy).on_click(move |_, window, cx| {
                            let _ = cancel.update(cx, |this, cx| this.close_management_dialog(window, cx));
                        }));
                let (title, body) = match &modal {
                    Modal::Settings => {
                        let save = view.clone();
                        footer = footer.child(Button::new("save-settings").primary().label("保存设置")
                            .disabled(busy).loading(busy).on_click(move |_, window, cx| {
                                let _ = save.update(cx, |this, cx| this.save_settings(window, cx));
                            }));
                        ("全局设置".to_string(), div().flex().flex_col().gap_2()
                            .child(div().font_semibold().child("备份"))
                            .child(div().text_sm().child("每个配置保留的版本数"))
                            .child(Input::new(&this.version_limit).id("max-versions")
                                .aria_label("每个配置保留的版本数，1 到 1000").disabled(busy))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground)
                                .child("默认 10，范围 1–1000。适用于所有配置及自动备份。保存设置后立即清理超出的旧版本，以后每次备份也会自动清理。"))
                            .child(div().text_xs().text_color(cx.theme().warning)
                                .child("清理的旧版本无法恢复，当前配置文件会保留。"))
                            .child(div().font_semibold().mt_2().child("桌面应用"))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground)
                                .child(if cfg!(target_os = "macos") { "留空自动查找已安装的应用，也可指定 .app 的绝对路径，例如 /Applications/ChatGPT.app。" } else { "留空自动查找开始菜单中的应用，也可指定 ChatGPT.exe 的完整路径。" }))
                            .child(div().text_sm().child("ChatGPT 应用路径（可选）"))
                            .child(Input::new(&this.app_paths[0]).id("chatgpt-app-path").aria_label("ChatGPT 应用路径，留空自动查找").disabled(busy))
                            .into_any_element())
                    }
                    Modal::Versions(name) => {
                        let count = this.snapshot.profiles.iter().find(|p| p.key == *name)
                            .map_or(0, |p| p.versions.len());
                        let title = format!("历史版本 · {name} · {count} 个");
                        (title, div().into_any_element())
                    }
                    _ => (String::new(), div().into_any_element()),
                };
                dialog.title(title).centered()
                    // Dialog width accepts resolved pixels; derive it from the window rem.
                    .width(window.rem_size() * 38.)
                    .overlay_closable(!busy).keyboard(!busy).close_button(!busy)
                    .on_cancel(move |_, _, _| !busy)
                    .on_ok(move |_, window, cx| {
                        if settings_dialog {
                            let _ = confirm.update(cx, |this, cx| this.save_settings(window, cx));
                        }
                        false
                    })
                    .map(|dialog| {
                        if let Modal::Versions(name) = &modal {
                            let name = name.clone();
                            let view = view.clone();
                            // A virtual list owns its scrolling. Avoid Dialog's automatic
                            // scroll wrapper around children, which creates a second viewport.
                            dialog.content(move |content, window, cx| {
                                let Some(entity) = view.upgrade() else { return content; };
                                content.min_h_0().overflow_hidden().child(
                                    entity.read(cx).render_version_body(&name, view.clone(), window, cx)
                                )
                            })
                        } else {
                            dialog.child(body)
                                .children((!this.dialog_error.is_empty()).then(|| {
                                    div().text_sm().text_color(cx.theme().danger).child(this.dialog_error.clone())
                                }))
                                .children((this.dialog_error.is_empty() && !this.message.is_empty()).then(|| {
                                    div().text_xs().text_color(cx.theme().muted_foreground).child(this.message.clone())
                                }))
                        }
                    })
                    .footer(footer)
                    .on_close(move |_, _, cx| {
                        let view = close.clone();
                        cx.defer(move |cx| { let _ = view.update(cx, |this, cx| {
                            if this.modal_revision == revision {
                                this.modal = None;
                                this.pending_delete = None;
                                cx.notify();
                            }
                        }); });
                    })
            });
        }
        cx.notify();
    }

    fn render_version_body(
        &self,
        name: &str,
        view: WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> impl IntoElement {
        let count = self
            .snapshot
            .profiles
            .iter()
            .find(|p| p.key == name)
            .map_or(0, |p| p.versions.len());
        let name = name.to_string();
        // Leave room for the title, help, feedback and footer on smaller windows.
        let height = (window.viewport_size().height - window.rem_size() * 14.)
            .clamp(window.rem_size() * 4., window.rem_size() * 12.);
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "按时间从新到旧排列。切换前自动备份当前文件；删除版本不会改动当前配置。",
                    ),
            )
            .child(if count == 0 {
                div()
                    .p_4()
                    .text_sm()
                    .child("此配置暂无备份版本，可关闭后保存备份。")
                    .into_any_element()
            } else {
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .w_full()
                    .h(height)
                    .flex_shrink_0()
                    .child(
                        uniform_list("backup-versions", count, move |range, _, cx| {
                            let Some(entity) = view.upgrade() else {
                                return Vec::new();
                            };
                            entity.update(cx, |this, cx| {
                                let Some(profile) =
                                    this.snapshot.profiles.iter().find(|p| p.key == name)
                                else {
                                    return Vec::new();
                                };
                                range
                                    .filter_map(|ix| profile.versions.get(ix))
                                    .map(|version| {
                                        this.render_version(&name, version, cx).into_any_element()
                                    })
                                    .collect()
                            })
                        })
                        .size_full()
                        .min_h_0()
                        .pr_4()
                        .track_scroll(&self.version_scroll),
                    )
                    // Pin the overlay to the viewport, not the next normal-flow position.
                    // Use the list handle's bounds so painting and dragging share its geometry.
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::vertical(&self.version_scroll)
                                .id("backup-versions-scrollbar")
                                .mode(ScrollbarMode::Always),
                        ),
                    )
                    .into_any_element()
            })
            .children((!self.dialog_error.is_empty()).then(|| {
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(self.dialog_error.clone())
            }))
            .children(
                (self.dialog_error.is_empty() && !self.message.is_empty()).then(|| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(self.message.clone())
                }),
            )
    }

    fn render_version(
        &self,
        name: &str,
        version: &BackupVersion,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pending = self.pending_delete.as_ref() == Some(&version.id);
        let unavailable = self.busy || !self.loaded;
        let switch_name = name.to_string();
        let switch_id = version.id.clone();
        let delete_name = name.to_string();
        let delete_id = version.id.clone();
        let cancel_id = version.id.clone();
        let folder_name = name.to_string();
        let folder_id = version.id.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .min_w_0()
            .h_16()
            .px_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .truncate()
                            .child(version.time.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(format!(
                                "{} · {}",
                                version.summary.provider, version.summary.model
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if version.problem.is_some() {
                                cx.theme().warning
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(if pending {
                                "删除此版本后无法恢复".into()
                            } else if version.is_current {
                                "使用中".into()
                            } else {
                                version.problem.clone().unwrap_or_default()
                            }),
                    ),
            )
            .child(if pending {
                Button::new(SharedString::from(format!("cancel-version:{}", version.id)))
                    .small()
                    .label("取消")
                    .disabled(unavailable)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.pending_delete.as_ref() == Some(&cancel_id) {
                            this.pending_delete = None;
                            cx.notify();
                            window.refresh();
                        }
                    }))
            } else {
                Button::new(SharedString::from(format!("switch-version:{}", version.id)))
                    .small()
                    .outline()
                    .label("切换到此版本")
                    .disabled(unavailable || version.problem.is_some() || version.is_current)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run(
                            Operation::SwitchVersion(switch_name.clone(), switch_id.clone()),
                            window,
                            cx,
                        );
                    }))
            })
            .child(
                Button::new(SharedString::from(format!("delete-version:{}", version.id)))
                    .small()
                    .when(pending, |button| button.danger())
                    .label(if pending { "删除版本" } else { "删除…" })
                    .accessibility_label(format!("删除 {} 的 {} 版本", name, version.time))
                    .disabled(unavailable)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.pending_delete.as_ref() == Some(&delete_id) {
                            this.run(
                                Operation::DeleteVersion(delete_name.clone(), delete_id.clone()),
                                window,
                                cx,
                            );
                        } else {
                            this.pending_delete = Some(delete_id.clone());
                            cx.notify();
                            window.refresh();
                        }
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("folder-version:{}", version.id)))
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .accessibility_label(format!("打开 {} 的 {} 版本目录", name, version.time))
                    .tooltip("打开此版本的备份目录")
                    .disabled(unavailable)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run(
                            Operation::OpenVersion(folder_name.clone(), folder_id.clone()),
                            window,
                            cx,
                        );
                    })),
            )
    }
}
