//! Service status, clipboard synchronization and platform permissions.

use eframe::egui::{self, Align, Layout, RichText};
use lan_mouse_ipc::{FrontendRequest, Status};

use crate::app::CrossDeskApp;
use crate::app::widgets::settings_row;
use crate::theme;

#[cfg(target_os = "macos")]
use crate::macos_privacy;

impl CrossDeskApp {
    pub(crate) fn settings_page(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        settings_row(ui, "本机名称", &self.local_hostname, None);
        ui.add_space(6.0);
        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                let port_label = ui.label(RichText::new("监听端口").strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("应用").clicked() {
                        match self
                            .local_port
                            .trim()
                            .parse::<u16>()
                            .ok()
                            .filter(|port| *port != 0)
                        {
                            Some(port) => {
                                self.send(FrontendRequest::ChangePort(port));
                            }
                            None => self.show_notice("端口必须是 1 到 65535 之间的数字", true),
                        }
                    }
                    let port = ui
                        .add(egui::TextEdit::singleline(&mut self.local_port).desired_width(90.0));
                    port.labelled_by(port_label.id);
                });
            });
        });

        ui.add_space(18.0);
        theme::section_label(ui, "输入服务");
        ui.add_space(6.0);
        self.status_row(
            ui,
            "输入捕获",
            self.state.capture_status,
            FrontendRequest::EnableCapture,
        );
        ui.add_space(6.0);
        self.status_row(
            ui,
            "输入注入",
            self.state.emulation_status,
            FrontendRequest::EnableEmulation,
        );

        ui.add_space(18.0);
        theme::section_label(ui, "剪贴板");
        ui.add_space(6.0);
        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut enabled = self.state.clipboard_enabled;
                if ui.checkbox(&mut enabled, "同步文本剪贴板").changed()
                    && self.send(FrontendRequest::SetClipboardSync(enabled))
                {
                    self.state.clipboard_enabled = enabled;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    theme::status_badge(
                        ui,
                        if self.state.clipboard_available {
                            palette.success
                        } else {
                            palette.warning
                        },
                        if self.state.clipboard_available {
                            "可用"
                        } else {
                            "不可用"
                        },
                    );
                });
            });
        });

        #[cfg(target_os = "macos")]
        self.macos_permissions_section(ui);

        ui.add_space(18.0);
        theme::section_label(ui, "运行状态");
        ui.add_space(6.0);
        settings_row(
            ui,
            "后台服务",
            if self.connected {
                "已连接"
            } else {
                "正在重连"
            },
            Some(if self.connected {
                palette.success
            } else {
                palette.warning
            }),
        );
    }

    fn status_row(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        status: Status,
        request: FrontendRequest,
    ) {
        let palette = theme::palette(ui);
        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).strong());
                let enabled = status == Status::Enabled;
                theme::status_badge(
                    ui,
                    if enabled {
                        palette.success
                    } else {
                        palette.warning
                    },
                    if enabled { "已启用" } else { "未启用" },
                );
                if !enabled {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("重新启用").clicked() {
                            self.send(request);
                        }
                    });
                }
            });
        });
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn macos_permissions_section(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        ui.add_space(18.0);
        theme::section_label(ui, "macOS 权限");
        ui.add_space(6.0);

        let accessibility = self.macos_permissions.accessibility;
        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("辅助功能").strong());
                theme::status_badge(
                    ui,
                    if accessibility {
                        palette.success
                    } else {
                        palette.danger
                    },
                    if accessibility {
                        "已授权"
                    } else {
                        "未授权"
                    },
                );
                if !accessibility {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("打开系统设置").clicked() {
                            macos_privacy::open_accessibility_settings();
                        }
                    });
                }
            });
        });

        ui.add_space(6.0);
        let input_monitoring = self.macos_permissions.input_monitoring;
        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("输入监控").strong());
                theme::status_badge(
                    ui,
                    if input_monitoring {
                        palette.success
                    } else {
                        palette.danger
                    },
                    if input_monitoring {
                        "已授权"
                    } else {
                        "未授权"
                    },
                );
                if !input_monitoring {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("打开系统设置").clicked() {
                            macos_privacy::open_input_monitoring_settings();
                        }
                    });
                }
            });
        });

        if self.macos_restart_required {
            ui.add_space(8.0);
            Frame::new()
                .fill(Color32::from_rgb(120, 78, 8))
                .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(Color32::WHITE, "权限已更新，需要重启 CrossDesk 后生效");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("立即重启").clicked() {
                                macos_privacy::relaunch_bundle();
                                self.request_quit(ui.ctx());
                            }
                        });
                    });
                });
        }
    }
}
