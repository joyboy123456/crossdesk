//! Pending and approved peer fingerprints.

use eframe::egui::{self, Align, Align2, CornerRadius, FontId, Layout, RichText, Sense, Vec2};
use lan_mouse_ipc::FrontendRequest;

use crate::app::CrossDeskApp;
use crate::app::widgets::{
    icon_button, icon_tile, primary_button, secondary_button, short_fingerprint,
};
use crate::theme;

impl CrossDeskApp {
    pub(crate) fn authorization_page(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);

        theme::full_card(ui, theme::card_frame(ui), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                icon_tile(ui, "fingerprint-pattern", 40.0, palette.accent, true);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new("本机证书指纹")
                            .size(14.0)
                            .strong()
                            .color(palette.text),
                    );
                    ui.label(
                        RichText::new("对方授权本机时，请让对方核对这串指纹")
                            .size(theme::CAPTION_SIZE + 1.0)
                            .color(palette.text_muted),
                    );
                });
            });
            ui.add_space(10.0);
            theme::full_card(
                ui,
                egui::Frame::new()
                    .fill(palette.bg_canvas)
                    .stroke(egui::Stroke::new(1.0, palette.border))
                    .corner_radius(CornerRadius::same(theme::WIDGET_RADIUS))
                    .inner_margin(egui::Margin::symmetric(12, 6)),
                |ui| {
                    ui.horizontal(|ui| {
                        let empty = self.state.fingerprint.is_empty();
                        ui.add(
                            egui::Label::new(
                                theme::mono_text(
                                    if empty {
                                        "尚未从服务读取"
                                    } else {
                                        &self.state.fingerprint
                                    },
                                    theme::BODY_SIZE - 0.5,
                                )
                                .color(if empty {
                                    palette.text_muted
                                } else {
                                    palette.text_secondary
                                }),
                            )
                            .wrap()
                            .selectable(true),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, "copy", "复制指纹", palette.text_muted).clicked()
                                && !empty
                            {
                                ui.ctx().copy_text(self.state.fingerprint.clone());
                                self.show_notice("指纹已复制", false);
                            }
                        });
                    });
                },
            );
        });

        if !self.state.pending_authorizations.is_empty() {
            theme::section_label(ui, "待授权");
        }
        for fingerprint in self.state.pending_authorizations.clone() {
            let mut cancel = false;
            theme::full_card(
                ui,
                theme::card_frame(ui)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .fill(theme::lerp_color(palette.surface, palette.warning, 0.06))
                    .stroke(egui::Stroke::new(1.0, palette.warning.gamma_multiply(0.45))),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        icon_tile(ui, "shield-alert", 34.0, palette.warning, false);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                RichText::new("新设备请求连接")
                                    .size(theme::BODY_SIZE)
                                    .strong()
                                    .color(palette.text),
                            );
                            ui.label(
                                theme::mono_text(&fingerprint, theme::CAPTION_SIZE + 0.5)
                                    .color(palette.text_muted),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if secondary_button(ui, "取消").clicked() {
                                cancel = true;
                            }
                            if primary_button(ui, Some("check"), "批准").clicked() {
                                self.auth_fingerprint = fingerprint.clone();
                            }
                        });
                    });
                },
            );
            ui.add_space(4.0);
            if cancel {
                self.state
                    .pending_authorizations
                    .retain(|item| item != &fingerprint);
            }
        }

        theme::section_label(ui, "手动授权");
        theme::full_card(ui, theme::card_frame(ui), |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let description_label = ui.label(
                RichText::new("设备名称")
                    .size(theme::CAPTION_SIZE + 1.0)
                    .color(palette.text_secondary),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.auth_description)
                    .hint_text("例如：客厅 MacBook")
                    .margin(egui::Margin::symmetric(10, 7))
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(description_label.id);
            ui.add_space(4.0);
            let fingerprint_label = ui.label(
                RichText::new("证书指纹")
                    .size(theme::CAPTION_SIZE + 1.0)
                    .color(palette.text_secondary),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.auth_fingerprint)
                    .font(FontId::monospace(theme::BODY_SIZE - 0.5))
                    .hint_text("在对方设备的「授权」页复制")
                    .margin(egui::Margin::symmetric(10, 7))
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(fingerprint_label.id);
            ui.add_space(8.0);
            if primary_button(ui, Some("shield-check"), "授权设备").clicked() {
                let description = self.auth_description.trim().to_owned();
                let fingerprint = self.auth_fingerprint.trim().to_owned();
                if description.is_empty() || fingerprint.is_empty() {
                    self.show_notice("请填写设备名称和证书指纹", true);
                } else if self.send(FrontendRequest::AuthorizeKey(
                    description,
                    fingerprint.clone(),
                )) {
                    self.state
                        .pending_authorizations
                        .retain(|item| item != &fingerprint);
                    self.auth_description.clear();
                    self.auth_fingerprint.clear();
                }
            }
        });

        let mut authorized = self
            .state
            .authorized
            .iter()
            .map(|(fingerprint, description)| (fingerprint.clone(), description.clone()))
            .collect::<Vec<_>>();
        authorized.sort_by(|a, b| a.1.cmp(&b.1));
        ui.horizontal(|ui| {
            theme::section_label(ui, "已授权设备");
            if !authorized.is_empty() {
                ui.label(
                    RichText::new(authorized.len().to_string())
                        .size(theme::CAPTION_SIZE)
                        .color(palette.text_muted),
                );
            }
        });
        theme::full_card(ui, theme::group_frame(ui), |ui| {
            if authorized.is_empty() {
                theme::group_row(ui, "暂无已授权设备", None, |_| {});
            }
            for (index, (fingerprint, description)) in authorized.iter().enumerate() {
                if index > 0 {
                    theme::group_divider(ui);
                }
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), theme::ROW_HEIGHT + 8.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_height(theme::ROW_HEIGHT + 8.0);
                        ui.spacing_mut().item_spacing.x = 10.0;
                        avatar(ui, description);
                        ui.label(
                            RichText::new(description)
                                .size(theme::BODY_SIZE)
                                .strong()
                                .color(palette.text),
                        );
                        ui.label(
                            theme::mono_text(
                                short_fingerprint(fingerprint),
                                theme::CAPTION_SIZE + 0.5,
                            )
                            .color(palette.text_muted),
                        )
                        .on_hover_text(fingerprint);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, "trash-2", "移除授权", palette.danger).clicked()
                            {
                                self.send(FrontendRequest::RemoveAuthorizedKey(
                                    fingerprint.clone(),
                                ));
                            }
                        });
                    },
                );
            }
        });
        ui.add_space(8.0);
    }
}

/// A round monogram for a peer name.
fn avatar(ui: &mut egui::Ui, name: &str) {
    let palette = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
    // stable per-name hue between the two gradient ends
    let t = name.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    }) % 100;
    let color = theme::accent_gradient_stop(palette, t as f32 / 100.0);
    ui.painter()
        .circle_filled(rect.center(), 14.0, color.gamma_multiply(0.16));
    ui.painter().circle_stroke(
        rect.center(),
        14.0,
        egui::Stroke::new(1.0, color.gamma_multiply(0.4)),
    );
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(12.5),
        color,
    );
}
