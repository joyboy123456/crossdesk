//! Pending and approved peer fingerprints.

use eframe::egui::{self, Align, Layout, RichText};
use lan_mouse_ipc::FrontendRequest;

use crate::app::CrossDeskApp;
use crate::app::widgets::{icon_button, short_fingerprint};
use crate::theme;

impl CrossDeskApp {
    pub(crate) fn authorization_page(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        theme::card_frame(ui).show(ui, |ui| {
            ui.label(RichText::new("本机证书指纹").strong());
            ui.horizontal(|ui| {
                ui.monospace(if self.state.fingerprint.is_empty() {
                    "尚未从服务读取"
                } else {
                    &self.state.fingerprint
                });
                if icon_button(ui, "copy", "复制指纹", palette.text_muted).clicked()
                    && !self.state.fingerprint.is_empty()
                {
                    ui.ctx().copy_text(self.state.fingerprint.clone());
                    self.show_notice("指纹已复制", false);
                }
            });
        });

        ui.add_space(14.0);
        theme::section_label(ui, "待授权");
        ui.add_space(4.0);
        for fingerprint in self.state.pending_authorizations.clone() {
            let mut cancel = false;
            theme::row_frame(ui).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.monospace(&fingerprint);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                        if ui.button("批准").clicked() {
                            self.auth_fingerprint = fingerprint.clone();
                        }
                    });
                });
            });
            ui.add_space(6.0);
            if cancel {
                self.state
                    .pending_authorizations
                    .retain(|item| item != &fingerprint);
            }
        }

        ui.add_space(12.0);
        theme::card_frame(ui).show(ui, |ui| {
            let description_label = ui.label("设备名称");
            ui.text_edit_singleline(&mut self.auth_description)
                .labelled_by(description_label.id);
            let fingerprint_label = ui.label("证书指纹");
            ui.text_edit_singleline(&mut self.auth_fingerprint)
                .labelled_by(fingerprint_label.id);
            ui.add_space(6.0);
            if ui.button("授权设备").clicked() {
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

        ui.add_space(14.0);
        theme::section_label(ui, "已授权设备");
        ui.add_space(4.0);
        let mut authorized = self
            .state
            .authorized
            .iter()
            .map(|(fingerprint, description)| (fingerprint.clone(), description.clone()))
            .collect::<Vec<_>>();
        authorized.sort_by(|a, b| a.1.cmp(&b.1));
        for (fingerprint, description) in authorized {
            theme::row_frame(ui).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&description).strong());
                    ui.monospace(short_fingerprint(&fingerprint));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, "trash-2", "移除授权", palette.danger).clicked() {
                            self.send(FrontendRequest::RemoveAuthorizedKey(fingerprint.clone()));
                        }
                    });
                });
            });
            ui.add_space(6.0);
        }
    }
}
