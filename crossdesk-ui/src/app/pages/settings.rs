//! Service status, clipboard synchronization and platform permissions.

use eframe::egui::{self, RichText};
use lan_mouse_ipc::{ControlMode, FrontendRequest, Status};

use crate::app::CrossDeskApp;
use crate::app::widgets::{option_card, secondary_button, toggle_row};
use crate::theme;

#[cfg(target_os = "macos")]
use crate::macos_privacy;

impl CrossDeskApp {
    pub(crate) fn settings_page(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);

        theme::section_label(ui, "本机");
        theme::full_card(ui, theme::group_frame(ui), |ui| {
            theme::group_row(ui, "本机名称", None, |ui| {
                ui.label(
                    RichText::new(&self.local_hostname)
                        .size(theme::BODY_SIZE)
                        .color(palette.text_secondary),
                );
            });
            theme::group_divider(ui);
            let mut port_label = None;
            let port_title = theme::group_row(
                ui,
                "监听端口",
                Some("其他设备连接本机时使用的 UDP/TCP 端口"),
                |ui| {
                    if secondary_button(ui, "应用").clicked() {
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
                    let port = ui.add(
                        egui::TextEdit::singleline(&mut self.local_port)
                            .font(egui::FontId::monospace(theme::BODY_SIZE))
                            .margin(egui::Margin::symmetric(8, 6))
                            .desired_width(80.0),
                    );
                    port_label = Some(port);
                },
            );
            if let Some(port) = port_label {
                port.labelled_by(port_title.id);
            }
        });

        theme::section_label(ui, "输入服务");
        theme::full_card(ui, theme::group_frame(ui), |ui| {
            self.status_row(
                ui,
                "输入捕获",
                "把本机键鼠转发给其他设备",
                self.state.capture_status,
                FrontendRequest::EnableCapture,
            );
            theme::group_divider(ui);
            self.status_row(
                ui,
                "输入注入",
                "在本机重放其他设备发来的键鼠",
                self.state.emulation_status,
                FrontendRequest::EnableEmulation,
            );
        });

        theme::section_label(ui, "剪贴板");
        theme::full_card(ui, theme::group_frame(ui), |ui| {
            let mut enabled = self.state.clipboard_enabled;
            let description = if self.state.clipboard_available {
                "在设备之间自动同步复制的文本"
            } else {
                "本机剪贴板当前不可用"
            };
            if toggle_row(ui, &mut enabled, "同步文本剪贴板", description).changed()
                && self.send(FrontendRequest::SetClipboardSync(enabled))
            {
                self.state.clipboard_enabled = enabled;
            }
        });

        self.control_mode_section(ui);

        #[cfg(target_os = "macos")]
        self.macos_permissions_section(ui);

        theme::section_label(ui, "运行状态");
        theme::full_card(ui, theme::group_frame(ui), |ui| {
            theme::group_row(ui, "后台服务", Some(&self.connection_detail), |ui| {
                theme::status_badge(
                    ui,
                    if self.connected {
                        palette.success
                    } else {
                        palette.warning
                    },
                    if self.connected {
                        "已连接"
                    } else {
                        "正在重连"
                    },
                );
            });
        });
        ui.add_space(8.0);
    }

    fn control_mode_section(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        theme::section_label(ui, "控制方式");
        let mut selected_mode = self.state.control_mode;
        ui.spacing_mut().item_spacing.y = 6.0;
        for (mode, icon_name, title, description) in [
            (
                ControlMode::SendOnly,
                "mouse-pointer-2",
                "仅控制其他设备",
                "使用本机键鼠控制其他设备，不接受远端控制",
            ),
            (
                ControlMode::ReceiveOnly,
                "inbox",
                "仅允许被控制",
                "只允许已授权设备控制本机，不捕获本机屏幕边缘",
            ),
            (
                ControlMode::Bidirectional,
                "arrow-left-right",
                "双向自动",
                "两端均可发起控制；同一时刻只会有一个控制角色",
            ),
        ] {
            let advanced = mode == ControlMode::Bidirectional;
            if option_card(
                ui,
                selected_mode == mode,
                icon_name,
                title,
                description,
                |ui| {
                    if advanced {
                        theme::status_badge(ui, palette.warning, "高级");
                    }
                },
            )
            .clicked()
            {
                selected_mode = mode;
            }
        }
        ui.spacing_mut().item_spacing.y = 8.0;
        if selected_mode != self.state.control_mode
            && self.send(FrontendRequest::SetControlMode(selected_mode))
        {
            self.state.control_mode = selected_mode;
        }
    }

    fn status_row(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        description: &str,
        status: Status,
        request: FrontendRequest,
    ) {
        let palette = theme::palette(ui);
        let enabled = status == Status::Enabled;
        theme::group_row(ui, label, Some(description), |ui| {
            if !enabled && secondary_button(ui, "重新启用").clicked() {
                self.send(request);
            }
            theme::status_badge(
                ui,
                if enabled {
                    palette.success
                } else {
                    palette.warning
                },
                if enabled { "已启用" } else { "未启用" },
            );
        });
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn macos_permissions_section(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        theme::section_label(ui, "macOS 权限");

        let accessibility = self.macos_permissions.accessibility;
        let input_monitoring = self.macos_permissions.input_monitoring;
        theme::full_card(ui, theme::group_frame(ui), |ui| {
            for (title, description, granted, open) in [
                (
                    "辅助功能",
                    "控制本机光标与键盘所需",
                    accessibility,
                    macos_privacy::open_accessibility_settings as fn(),
                ),
                (
                    "输入监控",
                    "读取本机键鼠以转发给其他设备所需",
                    input_monitoring,
                    macos_privacy::open_input_monitoring_settings as fn(),
                ),
            ] {
                if title == "输入监控" {
                    theme::group_divider(ui);
                }
                theme::group_row(ui, title, Some(description), |ui| {
                    if !granted && secondary_button(ui, "打开系统设置").clicked() {
                        open();
                    }
                    theme::status_badge(
                        ui,
                        if granted {
                            palette.success
                        } else {
                            palette.danger
                        },
                        if granted { "已授权" } else { "未授权" },
                    );
                });
            }
        });

        if self.macos_restart_required {
            ui.add_space(8.0);
            theme::full_card(
                ui,
                theme::card_frame(ui)
                    .fill(palette.warning.gamma_multiply(0.12))
                    .stroke(egui::Stroke::new(1.0, palette.warning.gamma_multiply(0.5)))
                    .inner_margin(egui::Margin::symmetric(16, 4)),
                |ui| {
                    theme::group_row(
                        ui,
                        "权限已更新，需要重启 CrossDesk 后生效",
                        None,
                        |ui| {
                            if crate::app::widgets::primary_button(
                                ui,
                                Some("rotate-cw"),
                                "立即重启",
                            )
                            .clicked()
                            {
                                macos_privacy::relaunch_bundle();
                                self.request_quit(ui.ctx());
                            }
                        },
                    );
                },
            );
        }
    }
}
