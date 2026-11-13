use super::auxiliary_window::{AuxiliaryWindow, WindowContent};
use super::*;
use crate::store::BackupVersion;
use gpui_kit::component::scroll::ScrollbarMode;

pub(super) struct VersionHistory {
    name: String,
    scroll: UniformListScrollHandle,
}

impl VersionHistory {
    pub(super) fn new(name: String) -> Self {
        Self {
            name,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

impl AuxiliaryWindow {
    pub(super) fn render_history(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let WindowContent::Versions(history) = &self.content else {
            unreachable!()
        };
        let owner = self.owner.read(cx);
        let profile = owner
            .snapshot
            .profiles
            .iter()
            .find(|p| p.key == history.name);
        let count = profile.map_or(0, |p| p.versions.len());
        let exists = profile.is_some();
        let view = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p_3()
            .gap_2()
            .child(
                div()
                    .font_semibold()
                    .flex_shrink_0()
                    .truncate()
                    .child(format!("历史版本 · {} · {count} 个", history.name)),
            )
            .child(
                div()
                    .text_xs()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "按时间从新到旧排列。切换前自动备份当前文件；删除版本不会改动当前配置。",
                    ),
            )
            .child(if count == 0 {
                div()
                    .flex_1()
                    .p_4()
                    .child(if exists {
                        "此配置暂无备份版本，可保存新的备份。"
                    } else {
                        "此配置目录已被删除。"
                    })
                    .into_any_element()
            } else {
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("backup-versions", count, move |range, _, cx| {
                            let Some(view) = view.upgrade() else {
                                return Vec::new();
                            };
                            view.update(cx, |this, cx| {
                                let WindowContent::Versions(history) = &this.content else {
                                    return Vec::new();
                                };
                                let owner = this.owner.read(cx);
                                let Some(profile) = owner
                                    .snapshot
                                    .profiles
                                    .iter()
                                    .find(|p| p.key == history.name)
                                else {
                                    return Vec::new();
                                };
                                let versions: Vec<_> = range
                                    .filter_map(|index| profile.versions.get(index).cloned())
                                    .collect();
                                versions
                                    .iter()
                                    .map(|version| {
                                        this.render_version(version, cx).into_any_element()
                                    })
                                    .collect()
                            })
                        })
                        .size_full()
                        .min_h_0()
                        .pr_4()
                        .track_scroll(&history.scroll),
                    )
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::vertical(&history.scroll)
                                .id("backup-versions-scrollbar")
                                .mode(ScrollbarMode::Always),
                        ),
                    )
                    .into_any_element()
            })
            .child(
                div().flex_shrink_0().child(
                    Button::new("refresh-history")
                        .ghost()
                        .icon(IconName::RefreshCw)
                        .label("刷新")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.run(Operation::Refresh, window, cx)
                        })),
                ),
            )
    }

    fn render_version(&self, version: &BackupVersion, cx: &mut Context<Self>) -> impl IntoElement {
        let WindowContent::Versions(history) = &self.content else {
            unreachable!()
        };
        let owner = self.owner.read(cx);
        let unavailable = owner.operation_pending() || !owner.loaded;
        let name = history.name.clone();
        let switch_name = name.clone();
        let delete_name = name.clone();
        let folder_name = name.clone();
        let switch_id = version.id.clone();
        let delete_id = version.id.clone();
        let delete_time = version.time.clone();
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
                            .child(if version.is_current {
                                "使用中".into()
                            } else {
                                version.problem.clone().unwrap_or_default()
                            }),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!("switch-version:{}", version.id)))
                    .small()
                    .outline()
                    .icon(IconName::Undo2)
                    .label("还原到此版本")
                    .disabled(unavailable || version.problem.is_some() || version.is_current)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.owner.update(cx, |owner, cx| {
                            owner.confirm_switch(
                                switch_name.clone(),
                                Some(switch_id.clone()),
                                window,
                                cx,
                            );
                        });
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("delete-version:{}", version.id)))
                    .small()
                    .danger()
                    .icon(IconName::Trash)
                    .label("删除…")
                    .accessibility_label(format!("删除 {name} 的 {} 版本", version.time))
                    .disabled(unavailable)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.owner.update(cx, |owner, cx| {
                            owner.confirm_delete_version(
                                delete_name.clone(),
                                delete_id.clone(),
                                delete_time.clone(),
                                window,
                                cx,
                            )
                        });
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("folder-version:{}", version.id)))
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .accessibility_label(format!("打开 {name} 的 {} 版本目录", version.time))
                    .tooltip("打开此版本的备份目录")
                    .disabled(unavailable)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run(
                            Operation::OpenVersion(folder_name.clone(), folder_id.clone()),
                            window,
                            cx,
                        )
                    })),
            )
    }
}
