use super::*;
use gpui_kit::component::{
    WindowExt,
    dialog::DialogButtonProps,
    select::{Select, SelectItem},
    slider::{Slider, SliderState},
};

#[derive(Clone)]
pub(super) struct ThemeChoice {
    id: String,
    name: SharedString,
}

impl ThemeChoice {
    pub(super) fn off() -> Self {
        Self {
            id: String::new(),
            name: crate::tr!("shell.theme_off").into(),
        }
    }
}

impl SelectItem for ThemeChoice {
    type Value = String;

    fn title(&self) -> SharedString {
        self.name.clone()
    }
    fn value(&self) -> &Self::Value {
        &self.id
    }
}

struct TransparencyControl {
    slider: Entity<SliderState>,
    _subscription: Subscription,
}

impl Render for TransparencyControl {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .py_2()
            .child(crate::tr!("theme_controls.background_transparency"; arg0 = self.slider.read(cx).value().start().round() as u8))
            .child(Slider::new(&self.slider).accessibility_label(crate::tr!("theme_controls.background_transparency_0_to_100")))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::tr!("theme_controls.0_opaque"))
                    .child(crate::tr!("theme_controls.100_transparent")),
            )
    }
}

impl Switcher {
    pub(crate) fn theme_snapshot(&self) -> &ThemeSnapshot {
        &self.themes
    }

    pub(crate) fn select_theme_from_tray(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.loaded && !self.operation_pending() {
            self.run_with_origin(Operation::SelectTheme(id), None, cx);
        }
    }

    pub(super) fn refresh_themes(&mut self, cx: &mut Context<Self>) {
        if !self.loaded || self.operation_pending() || self.checking_themes {
            return;
        }
        self.checking_themes = true;
        let directory = self.snapshot.settings.theme_directory.clone();
        let revision = self.app_revision;
        let service = self.theme_service.clone();
        let job = cx.background_spawn(async move { service.snapshot(&directory) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                this.checking_themes = false;
                if this.operation_pending() {
                    return;
                }
                if revision != this.app_revision {
                    this.refresh_themes(cx);
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        let unchanged = this.themes.selected == snapshot.selected
                            && this.themes.status == snapshot.status
                            && this.themes.transparency == snapshot.transparency
                            && this.themes.themes.len() == snapshot.themes.len()
                            && this
                                .themes
                                .themes
                                .iter()
                                .zip(&snapshot.themes)
                                .all(|(old, new)| old.id == new.id && old.name == new.name);
                        if unchanged && this.theme_select_synced {
                            return;
                        }
                        if this.themes.status != snapshot.status && snapshot.selected.is_some() {
                            this.message = snapshot.status.message();
                            this.is_error =
                                matches!(snapshot.status, crate::themes::ThemeStatus::Failed(_));
                        }
                        let mut choices = vec![ThemeChoice::off()];
                        choices.extend(snapshot.themes.iter().map(|theme| ThemeChoice {
                            id: theme.id.clone(),
                            name: theme.name.clone().into(),
                        }));
                        let selected = snapshot.selected.clone().unwrap_or_default();
                        this.themes = snapshot;
                        this.theme_select_synced = false;
                        let select = this.theme_select.clone();
                        let owner = cx.entity().downgrade();
                        // Updating the select needs its native window, and must happen
                        // after releasing the Switcher entity lease.
                        cx.defer(move |cx| {
                            let Some(window) = crate::shell::main_window_handle(cx) else {
                                return;
                            };
                            if cx
                                .update_window(window, |_, window, cx| {
                                    select.update(cx, |select, cx| {
                                        select.set_items(choices, window, cx);
                                        select.set_selected_value(&selected, window, cx);
                                    });
                                })
                                .is_ok()
                            {
                                let _ = owner.update(cx, |this, _| {
                                    if revision == this.app_revision {
                                        this.theme_select_synced = true;
                                    }
                                });
                            }
                        });
                        cx.notify();
                    }
                    Err(error) => this.show_theme_error(crate::tr!("theme_controls.could_not_read_the_theme_directory"; error = format!("{:#}", error)), cx),
                }
            });
        })
        .detach();
    }

    pub(super) fn show_theme_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.message = message.clone();
        self.is_error = true;
        cx.notify();
        if self.last_theme_error.as_ref() == Some(&message) {
            return;
        }
        if self.prompt_open {
            self.pending_theme_error = Some(message);
            return;
        }
        let Some(origin) = crate::shell::main_window_handle(cx) else {
            self.pending_theme_error = Some(message);
            return;
        };
        self.prompt_open = true;
        self.last_theme_error = Some(message.clone());
        let owner = cx.entity().downgrade();
        cx.defer(move |cx| {
            let failed_owner = owner.clone();
            let failed_message = message.clone();
            // Tray commands may run after the main native window was closed.
            let origin = crate::shell::show_main(cx).unwrap_or(origin);
            if cx
                .update_window(origin, |_, window, cx| {
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let owner = owner.clone();
                        alert
                            .title(crate::tr!(
                                "theme_controls.could_not_complete_the_theme_operation"
                            ))
                            .description(message.clone())
                            .button_props(
                                DialogButtonProps::default()
                                    .ok_text(crate::tr!("theme_controls.ok")),
                            )
                            .on_close(move |_, _, cx| {
                                let _ = owner.update(cx, |this, cx| {
                                    this.prompt_open = false;
                                    cx.notify();
                                });
                            })
                    });
                })
                .is_err()
            {
                let _ = failed_owner.update(cx, |this, cx| {
                    this.prompt_open = false;
                    this.last_theme_error = None;
                    this.pending_theme_error = Some(failed_message);
                    cx.notify();
                });
            }
        });
    }

    fn import_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation_pending() || !self.loaded {
            return;
        }
        self.prompt_open = true;
        let owner = cx.entity().downgrade();
        let origin = window.window_handle();
        cx.notify();
        cx.defer(move |cx| {
            let receiver = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some(crate::tr!("theme_controls.choose_zips_or_images").into()),
            });
            cx.spawn(async move |cx| {
                let result = receiver.await;
                cx.update(|cx| {
                    let _ =
                        owner.update(cx, |this, cx| {
                            this.prompt_open = false;
                            match result {
                                Ok(Ok(Some(paths))) => {
                                    if !paths.is_empty() {
                                        this.run_with_origin(
                                            Operation::ImportThemes(paths),
                                            Some(origin),
                                            cx,
                                        );
                                    }
                                }
                                Ok(Ok(None)) => {}
                                Ok(Err(error)) => this
                                    .show_theme_error(crate::tr!("theme_controls.could_not_select_theme_files"; error = format!("{:#}", error)), cx),
                                Err(_) => this
                                    .show_theme_error(crate::tr!("theme_controls.the_file_picker_returned_no_result_try_again").into(), cx),
                            }
                            cx.notify();
                        });
                });
            })
            .detach();
        });
    }

    fn show_theme_effects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation_pending() || self.themes.selected.is_none() {
            return;
        }
        self.prompt_open = true;
        let initial = self.themes.transparency;
        let owner = cx.entity().downgrade();
        let origin = window.window_handle();
        cx.notify();
        cx.defer(move |cx| {
            let failed_owner = owner.clone();
            if cx
                .update_window(origin, |_, window, cx| {
                    let slider = cx.new(|_| {
                        SliderState::new()
                            .min(0.)
                            .max(100.)
                            .step(1.)
                            .default_value(initial as f32)
                    });
                    let control = cx.new(|cx| {
                        let subscription = cx.observe(&slider, |_, _, cx| cx.notify());
                        TransparencyControl {
                            slider: slider.clone(),
                            _subscription: subscription,
                        }
                    });
                    window.open_alert_dialog(cx, move |alert, _, cx| {
                        let commit_owner = owner.clone();
                        let close_owner = owner.clone();
                        let slider_value = slider.clone();
                        let busy = owner.upgrade().is_none_or(|owner| owner.read(cx).busy);
                        alert
                            .title(crate::tr!("theme_controls.theme_effects"))
                            .description(crate::tr!("theme_controls.adjust_the_interface_background_transparency_higher_values_make_the"))
                            .confirm()
                            .button_props(
                                DialogButtonProps::default()
                                    .ok_text(if busy { crate::tr!("theme_controls.applying") } else { crate::tr!("theme_controls.apply") })
                                    .cancel_text(crate::tr!("desktop.close")),
                            )
                            .child(control.clone())
                            .on_ok(move |_, window, cx| {
                                let value = slider_value.read(cx).value().start().round() as u8;
                                let _ = commit_owner.update(cx, |this, cx| {
                                    if this.themes.selected.is_some() {
                                        this.run(Operation::ThemeTransparency(value), window, cx);
                                    }
                                });
                                false
                            })
                            .on_close(move |_, _, cx| {
                                let _ = close_owner.update(cx, |this, cx| {
                                    this.prompt_open = false;
                                    cx.notify();
                                });
                            })
                    });
                })
                .is_err()
            {
                let _ = failed_owner.update(cx, |this, cx| {
                    this.prompt_open = false;
                    cx.notify();
                });
            }
        });
    }

    pub(super) fn render_theme_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.operation_pending() || !self.loaded;
        div()
            .flex()
            .items_center()
            .gap_2()
            .ml_10()
            .min_w_0()
            .max_w_96()
            .child(
                Select::new(&self.theme_select)
                    .id("theme-select")
                    .w_40()
                    .min_w_24()
                    .flex_shrink(1.)
                    .small()
                    .accessibility_label(crate::tr!("theme_controls.current_theme"))
                    .disabled(disabled),
            )
            .child(
                Button::new("import-theme")
                    .outline()
                    .small()
                    .label(crate::tr!("theme_controls.import"))
                    .accessibility_label(crate::tr!("theme_controls.import_theme_zips_or_background_images_multiple_selection_supported"))
                    .tooltip(crate::tr!("theme_controls.import_theme_zips_or_background_images_multiple_files"))
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, window, cx| this.import_theme(window, cx))),
            )
            .child(
                Button::new("theme-effects")
                    .outline()
                    .small()
                    .label(crate::tr!("theme_controls.effects"))
                    .accessibility_label(crate::tr!("theme_controls.theme_effects"))
                    .disabled(disabled || self.themes.selected.is_none())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.show_theme_effects(window, cx)),
                    ),
            )
            .child(
                Button::new("open-theme-gallery")
                    .outline()
                    .small()
                    .icon(IconName::ExternalLink)
                    .accessibility_label(crate::tr!("theme_controls.open_the_dreamskin_theme_gallery"))
                    .tooltip(crate::tr!("theme_controls.browse_the_dreamskin_theme_gallery"))
                    .disabled(disabled)
                    .on_click(|_, _, cx| cx.open_url("https://dreamskin.cc/gallery")),
            )
            .child(
                Button::new("open-themes-directory")
                    .outline()
                    .small()
                    .icon(IconName::FolderOpen)
                    .accessibility_label(crate::tr!("theme_controls.open_theme_folder"))
                    .tooltip(crate::tr!("theme_controls.open_theme_folder"))
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.run(Operation::OpenThemes, window, cx)
                    })),
            )
    }
}
