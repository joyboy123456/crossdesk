//! The modal dialogs, and the transient state they own.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, Frame, Id, Layout, RichText, Sense, Stroke,
    Vec2,
};
use lan_mouse_ipc::{ClientHandle, FrontendRequest, Position};

use crate::app::CrossDeskApp;
use crate::app::geometry::position_occupied;
use crate::model::{DeviceDraft, position_label};
use crate::theme;

pub(crate) struct Editor {
    pub(crate) handle: Option<ClientHandle>,
    pub(crate) draft: DeviceDraft,
    pub(crate) error: Option<String>,
}

pub(crate) struct PendingPosition {
    pub(crate) target: Position,
    pub(crate) started: Instant,
}

impl PendingPosition {
    pub(crate) fn expired(&self) -> bool {
        self.started.elapsed() > Duration::from_secs(3)
    }
}

pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) error: bool,
    pub(crate) created: Instant,
}

impl CrossDeskApp {
    pub(crate) fn editor_dialog(&mut self, ctx: &egui::Context) {
        let open = self.editor.is_some();
        if open && !self.editor_dialog_open {
            // Restart the fade-in from fully transparent.
            ctx.animate_value_with_time(Id::new("dialog-editor-fade"), 0.0, 0.0);
        }
        self.editor_dialog_open = open;
        if !open {
            return;
        }
        let fade = ctx.animate_value_with_time(Id::new("dialog-editor-fade"), 1.0, 0.18);

        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let title = if editor.handle.is_some() {
            "编辑设备"
        } else {
            "添加设备"
        };
        let mut action = None;
        egui::Area::new(Id::new("device-editor"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.multiply_opacity(fade);
                dialog_frame(ui).show(ui, |ui| {
                    ui.set_min_width(420.0);
                    ui.label(theme::heading_text(title));
                    ui.add_space(8.0);
                    let hostname_label = ui.label("主机名");
                    ui.text_edit_singleline(&mut editor.draft.hostname)
                        .labelled_by(hostname_label.id);
                    let ips_label = ui.label("IP 地址");
                    ui.text_edit_singleline(&mut editor.draft.ips)
                        .labelled_by(ips_label.id);
                    let port_label = ui.label("端口");
                    ui.add(egui::TextEdit::singleline(&mut editor.draft.port).desired_width(120.0))
                        .labelled_by(port_label.id);
                    ui.add_space(6.0);
                    ui.label("屏幕方向");
                    ui.horizontal(|ui| {
                        for pos in [
                            Position::Left,
                            Position::Right,
                            Position::Top,
                            Position::Bottom,
                        ] {
                            let occupied = position_occupied(
                                &self.state,
                                &self.pending_positions,
                                pos,
                                editor.handle,
                            );
                            let selected = editor.draft.pos == pos;
                            if ui
                                .add_enabled(
                                    !occupied || selected,
                                    Button::new(position_label(pos)).selected(selected),
                                )
                                .clicked()
                            {
                                editor.draft.pos = pos;
                            }
                        }
                    });
                    ui.checkbox(&mut editor.draft.active, "立即启用");
                    if let Some(error) = &editor.error {
                        ui.colored_label(theme::palette(ui).danger, error);
                    }
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("保存").clicked() {
                            action = Some(true);
                        }
                        if ui.button("取消").clicked() {
                            action = Some(false);
                        }
                    });
                });
            });

        match action {
            Some(false) => self.editor = None,
            Some(true) => self.save_editor(),
            None => {}
        }
    }

    fn save_editor(&mut self) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let handle = editor.handle;
        let draft = editor.draft.clone();
        let config = match draft.validate() {
            Ok(config) => config,
            Err(error) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.error = Some(error);
                }
                return;
            }
        };
        if draft.active
            && position_occupied(&self.state, &self.pending_positions, config.pos, handle)
        {
            if let Some(editor) = self.editor.as_mut() {
                editor.error = Some(format!("{}已有启用设备", position_label(config.pos)));
            }
            return;
        }

        let success = if let Some(handle) = handle {
            let Some(original) = self.state.clients.get(&handle).cloned() else {
                self.show_notice("设备已不存在", true);
                self.editor = None;
                return;
            };
            self.send(FrontendRequest::UpdateHostname(
                handle,
                config.hostname.clone(),
            )) && self.send(FrontendRequest::UpdateFixIps(
                handle,
                config.fix_ips.clone(),
            )) && self.send(FrontendRequest::UpdatePort(handle, config.port))
                && self.send(FrontendRequest::UpdatePosition(handle, config.pos))
                && (original.state.active == draft.active
                    || self.send(FrontendRequest::Activate(handle, draft.active)))
        } else {
            self.send(FrontendRequest::CreateConfigured {
                config,
                active: draft.active,
            })
        };

        if success {
            self.editor = None;
        }
    }

    pub(crate) fn delete_dialog(&mut self, ctx: &egui::Context) {
        let open = self.delete_confirmation.is_some();
        if open && !self.delete_dialog_open {
            ctx.animate_value_with_time(Id::new("dialog-delete-fade"), 0.0, 0.0);
        }
        self.delete_dialog_open = open;
        if !open {
            return;
        }
        let fade = ctx.animate_value_with_time(Id::new("dialog-delete-fade"), 1.0, 0.18);

        let Some(handle) = self.delete_confirmation else {
            return;
        };
        let mut action = None;
        egui::Area::new(Id::new("delete-device"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.multiply_opacity(fade);
                dialog_frame(ui).show(ui, |ui| {
                    ui.label(theme::heading_text("删除设备"));
                    ui.add_space(8.0);
                    ui.label("删除后需要重新添加并配置该设备。");
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(Button::new("删除").fill(theme::palette(ui).danger))
                            .clicked()
                        {
                            action = Some(true);
                        }
                        if ui.button("取消").clicked() {
                            action = Some(false);
                        }
                    });
                });
            });
        match action {
            Some(true) => {
                if self.send(FrontendRequest::Delete(handle)) {
                    self.delete_confirmation = None;
                }
            }
            Some(false) => self.delete_confirmation = None,
            None => {}
        }
    }

    pub(crate) fn notice(&mut self, ctx: &egui::Context) {
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.created.elapsed() > Duration::from_secs(4))
        {
            self.notice = None;
        }
        let Some(notice) = &self.notice else { return };

        const LIFETIME: f32 = 4.0;
        const FADE: f32 = 0.25;
        let age = notice.created.elapsed().as_secs_f32();
        let fade_in = (age / FADE).min(1.0);
        let fade_out = ((LIFETIME - age) / FADE).clamp(0.0, 1.0);
        let alpha = (fade_in * fade_out).max(0.01);
        let offset_y = 56.0 - (1.0 - fade_in) * 14.0;

        egui::Area::new(Id::new("notice"))
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, offset_y))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.multiply_opacity(alpha);
                let palette = theme::palette(ui);
                let bar_color = if notice.error {
                    palette.danger
                } else {
                    palette.accent
                };
                Frame::new()
                    .fill(palette.surface_raised)
                    .stroke(Stroke::new(1.0, palette.border))
                    .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(50),
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::new(3.0, 16.0), Sense::hover());
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(2), bar_color);
                            ui.label(
                                RichText::new(&notice.text)
                                    .size(theme::BODY_SIZE)
                                    .color(palette.text),
                            );
                        });
                    });
            });
    }
}

pub(crate) fn dialog_frame(ui: &egui::Ui) -> Frame {
    let palette = theme::palette(ui);
    Frame::new()
        .fill(palette.surface_raised)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(20)
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(60),
        })
}
