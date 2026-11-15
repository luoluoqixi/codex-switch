use super::*;
use crate::update::UpdateInfo;
use gpui_kit::base::Link;
use gpui_kit::component::{
    WindowExt, dialog::DialogButtonProps, scroll::ScrollableElement, text::TextView,
};

pub(super) struct UpdateNotice {
    origin: AnyWindowHandle,
    result: Result<Option<UpdateInfo>, String>,
}

impl Switcher {
    pub(super) fn check_updates_on_startup(&mut self, cx: &mut Context<Self>) {
        if self.loaded
            && self
                .updates
                .begin_startup(self.snapshot.settings.auto_check_updates)
        {
            self.fetch_update(None, cx);
        }
    }

    pub(super) fn check_updates(&mut self, origin: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.updates.begin() {
            self.fetch_update(Some(origin), cx);
        }
    }

    fn fetch_update(&mut self, origin: Option<AnyWindowHandle>, cx: &mut Context<Self>) {
        let job = cx.background_spawn(async { crate::update::check_latest() });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                this.updates.finish(&result);
                if let Some(origin) = origin {
                    this.pending_update_notice = Some(UpdateNotice {
                        origin,
                        result: result.map_err(|error| format!("{error:#}")),
                    });
                    this.show_pending_update_notice(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn show_pending_update_notice(&mut self, cx: &mut Context<Self>) {
        if self.operation_pending() {
            return;
        }
        let Some(notice) = self.pending_update_notice.take() else {
            return;
        };
        self.prompt_open = true;
        let owner = cx.entity().downgrade();
        cx.defer(move |cx| {
            let close_owner = owner.clone();
            if cx.update_window(notice.origin, |_, window, cx| {
                window.open_alert_dialog(cx, move |alert, _, cx| {
                    let close_owner = close_owner.clone();
                    let alert = match &notice.result {
                        Ok(Some(info)) => {
                            let notes = if info.notes().is_empty() {
                                crate::tr!("update.no_release_notes").to_string()
                            } else { info.notes().to_string() };
                            alert
                                .title(crate::tr!("update.new_version_available"; version = info.version()))
                                .description(crate::tr!("update.version_comparison"; current = crate::APP_VERSION, latest = info.version()))
                                .child(div().flex().flex_col().min_w_0().gap_3()
                                    .child(div().font_semibold().child(info.title().to_string()))
                                    .child(div().max_h_48().min_h_0().overflow_y_scrollbar()
                                        .child(div().pr_3().child(TextView::markdown("update-release-notes", notes).selectable(true))))
                                    .child(Link::new("update-download")
                                        .href(info.url().to_string())
                                        .open_with(|url, _, _, cx| cx.open_url(url))
                                        .accessibility_label(crate::tr!("update.open_github_download"))
                                        .text_color(cx.theme().link)
                                        .text_decoration_1()
                                        .cursor_pointer()
                                        .rounded_sm().px_1().py_1()
                                        .hover(|style| style.bg(cx.theme().muted))
                                        .focus_visible(|style| style.bg(cx.theme().muted))
                                        .child(crate::tr!("update.open_github_download"))))
                        }
                        Ok(None) => alert
                            .title(crate::tr!("update.already_up_to_date"))
                            .description(crate::tr!("update.current_version_is_latest"; version = crate::APP_VERSION)),
                        Err(message) => alert
                            .title(crate::tr!("update.could_not_check_for_updates"))
                            .description(crate::tr!("update.check_failed_help"; error = message)),
                    };
                    alert
                        .button_props(DialogButtonProps::default().ok_text(crate::tr!("update.close")))
                        .on_close(move |_, _, cx| {
                            let _ = close_owner.update(cx, |this, cx| {
                                this.prompt_open = false;
                                cx.notify();
                            });
                        })
                });
            }).is_err() {
                let _ = owner.update(cx, |this, cx| {
                    this.prompt_open = false;
                    cx.notify();
                });
            }
        });
        cx.notify();
    }

    pub(super) fn render_update_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("update-available")
            .outline()
            .small()
            .icon(IconName::ArrowUp)
            .label(crate::tr!("update.update_available"))
            .accessibility_label(crate::tr!("update.open_update_details"))
            .tooltip(
                self.updates
                    .available()
                    .map(
                        |info| crate::tr!("update.new_version_available"; version = info.version()),
                    )
                    .unwrap_or_default(),
            )
            .disabled(self.operation_pending())
            .on_click(cx.listener(|this, _, window, cx| {
                if let Some(info) = this.updates.available().cloned() {
                    this.pending_update_notice = Some(UpdateNotice {
                        origin: window.window_handle(),
                        result: Ok(Some(info)),
                    });
                    this.show_pending_update_notice(cx);
                }
            }))
    }
}
