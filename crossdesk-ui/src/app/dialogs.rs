//! The modal dialogs, and the transient state they own.

use std::time::{Duration, Instant};

use async_channel::Receiver;
use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, Frame, Id, Layout, RichText, Sense, Stroke,
    Vec2,
};
use lan_mouse_ipc::{ClientHandle, FrontendRequest, Position};

use crate::app::CrossDeskApp;
use crate::app::geometry::position_occupied;
use crate::app::widgets::icon;
use crate::model::{DeviceDraft, position_label};
use crate::scan::{FoundDevice, ScanEvent};
use crate::theme;

pub(crate) struct Editor {
    pub(crate) handle: Option<ClientHandle>,
    pub(crate) draft: DeviceDraft,
    pub(crate) error: Option<String>,
}

/// The one-click LAN scan behind the "添加设备" button.
///
/// Owns the receiving half of the scan channel; the worker thread lives in
/// [`crate::scan`] and drops out of existence shortly after the dialog (and
/// with it the channel) is gone.
pub(crate) struct ScanDialog {
    rx: Receiver<ScanEvent>,
    pub(crate) devices: Vec<FoundDevice>,
    pub(crate) scanning: bool,
    pub(crate) error: Option<String>,
    #[cfg(test)]
    test_tx: Option<async_channel::Sender<ScanEvent>>,
}

impl ScanDialog {
    pub(crate) fn start(ctx: &egui::Context) -> Self {
        #[cfg(not(test))]
        {
            Self::from_receiver(crate::scan::start_scan(ctx.clone()))
        }
        #[cfg(test)]
        {
            // Tests drive the dialog through `inject` instead of real sockets
            // (`scan::scan_target` is exercised in its own tests). Referencing
            // the real entry point keeps it from going dead in test builds.
            let _ = crate::scan::start_scan;
            let _ = ctx;
            let (tx, rx) = async_channel::bounded(64);
            let mut dialog = Self::from_receiver(rx);
            dialog.test_tx = Some(tx);
            dialog
        }
    }

    fn from_receiver(rx: Receiver<ScanEvent>) -> Self {
        Self {
            rx,
            devices: Vec::new(),
            scanning: true,
            error: None,
            #[cfg(test)]
            test_tx: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn inject(&self, event: ScanEvent) {
        self.test_tx
            .as_ref()
            .expect("test scan sender")
            .try_send(event)
            .expect("test scan channel has capacity");
    }
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

    pub(crate) fn open_scanner(&mut self, ctx: &egui::Context) {
        self.editor = None;
        self.scan = Some(ScanDialog::start(ctx));
    }

    pub(crate) fn drain_scan(&mut self) {
        let Some(scan) = self.scan.as_mut() else {
            return;
        };
        while let Ok(event) = scan.rx.try_recv() {
            match event {
                ScanEvent::Found(device) => {
                    let known = scan
                        .devices
                        .iter()
                        .any(|existing| existing.addr == device.addr);
                    if !known {
                        scan.devices.push(device);
                    }
                }
                ScanEvent::Failed(error) => {
                    scan.error = Some(error);
                    scan.scanning = false;
                }
                ScanEvent::Finished => scan.scanning = false,
            }
        }
    }

    fn device_known(&self, device: &FoundDevice) -> bool {
        self.state.clients.values().any(|client| {
            client.config.fix_ips.contains(&device.addr)
                || client.state.ips.contains(&device.addr)
                || client.state.dns_ips.contains(&device.addr)
                || client
                    .state
                    .active_addr
                    .is_some_and(|addr| addr.ip() == device.addr)
                || (!device.hostname.is_empty()
                    && client.config.hostname.as_deref() == Some(device.hostname.as_str()))
        })
    }

    /// The scanner sees this machine too; adding it would share input with
    /// itself. The heuristic (announced hostname + port match ours) can
    /// misfire on a hostname collision - then the user simply cannot add
    /// that namesake from the scan list and can still use 手动添加.
    fn is_local_device(&self, device: &FoundDevice) -> bool {
        !device.hostname.is_empty()
            && device.hostname == self.local_hostname
            && device.port.to_string() == self.local_port
    }

    pub(crate) fn scan_dialog(&mut self, ctx: &egui::Context) {
        let open = self.scan.is_some();
        if open && !self.scan_dialog_open {
            ctx.animate_value_with_time(Id::new("dialog-scan-fade"), 0.0, 0.0);
        }
        self.scan_dialog_open = open;
        if !open {
            return;
        }
        let fade = ctx.animate_value_with_time(Id::new("dialog-scan-fade"), 1.0, 0.18);

        enum Action {
            Cancel,
            Rescan,
            Manual,
            Pick(usize),
        }

        let Some(scan) = self.scan.as_ref() else {
            return;
        };
        let scanning = scan.scanning;
        let error = scan.error.clone();
        let devices = scan.devices.clone();
        let mut action = None;
        egui::Area::new(Id::new("device-scan"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.multiply_opacity(fade);
                dialog_frame(ui).show(ui, |ui| {
                    let palette = theme::palette(ui);
                    ui.set_min_width(420.0);
                    ui.label(theme::heading_text("添加设备"));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("自动扫描局域网中运行 CrossDesk 的设备")
                            .size(theme::BODY_SIZE)
                            .color(palette.text_secondary),
                    );
                    ui.add_space(8.0);

                    // Fixed-height results region: the dialog must not jump
                    // around while the spinner and arriving results change
                    // the content size.
                    let width = ui.available_width().max(1.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(width, SCAN_RESULTS_HEIGHT),
                        Layout::top_down(Align::LEFT),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("scan-results")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if devices.is_empty() {
                                        scan_status(ui, scanning, error.as_deref());
                                    } else {
                                        if scanning {
                                            ui.horizontal(|ui| {
                                                ui.spinner();
                                                ui.label("正在扫描局域网设备…");
                                            });
                                            ui.add_space(6.0);
                                        }
                                        for (index, device) in devices.iter().enumerate() {
                                            if self.scan_device_row(ui, device) {
                                                action = Some(Action::Pick(index));
                                            }
                                            ui.add_space(6.0);
                                        }
                                    }
                                });
                        },
                    );

                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            action = Some(Action::Cancel);
                        }
                        if ui.button("手动添加").clicked() {
                            action = Some(Action::Manual);
                        }
                        if ui.add_enabled(!scanning, Button::new("重新扫描")).clicked() {
                            action = Some(Action::Rescan);
                        }
                    });
                });
            });

        match action {
            Some(Action::Cancel) => self.scan = None,
            Some(Action::Rescan) => self.scan = Some(ScanDialog::start(ctx)),
            Some(Action::Manual) => {
                self.scan = None;
                self.editor = Some(Editor {
                    handle: None,
                    draft: DeviceDraft::default(),
                    error: None,
                });
            }
            Some(Action::Pick(index)) => {
                if let Some(device) = devices.get(index) {
                    self.scan = None;
                    self.editor = Some(Editor {
                        handle: None,
                        draft: DeviceDraft {
                            hostname: device.hostname.clone(),
                            ips: device.addr.to_string(),
                            port: device.port.to_string(),
                            ..Default::default()
                        },
                        error: None,
                    });
                }
            }
            None => {}
        }
    }

    /// One row in the scan results: device info on the left, an add action
    /// (or a reason there is none) on the right. Returns true when the user
    /// picked this device.
    fn scan_device_row(&self, ui: &mut egui::Ui, device: &FoundDevice) -> bool {
        let palette = theme::palette(ui);
        let mut picked = false;
        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon("monitor", 18.0, palette.accent));
                ui.vertical(|ui| {
                    ui.label(RichText::new(device.display_name()).strong());
                    ui.label(
                        RichText::new(format!("{}:{}", device.addr, device.port))
                            .size(theme::CAPTION_SIZE)
                            .color(palette.text_secondary),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.is_local_device(device) {
                        ui.label(RichText::new("本机").color(palette.text_muted));
                    } else if self.device_known(device) {
                        ui.label(RichText::new("已添加").color(palette.text_muted));
                    } else if ui.button("添加").clicked() {
                        picked = true;
                    }
                });
            });
        });
        picked
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

/// Height of the fixed results region in the scan dialog.
const SCAN_RESULTS_HEIGHT: f32 = 220.0;

/// The centered placeholder of the scan dialog while no device was found
/// (yet): progress, failure, or the empty result.
fn scan_status(ui: &mut egui::Ui, scanning: bool, error: Option<&str>) {
    let palette = theme::palette(ui);
    ui.add_space(SCAN_RESULTS_HEIGHT / 2.0 - 30.0);
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        if scanning {
            ui.spinner();
            ui.add_space(8.0);
            ui.label("正在扫描局域网设备…");
        } else if let Some(error) = error {
            ui.colored_label(palette.danger, error);
            ui.add_space(4.0);
            ui.label(
                RichText::new("可尝试重新扫描或手动添加")
                    .size(theme::BODY_SIZE)
                    .color(palette.text_secondary),
            );
        } else {
            ui.label(RichText::new("未发现设备").strong());
            ui.add_space(4.0);
            ui.label(
                RichText::new("确认对方设备已启动 CrossDesk，且与本机在同一局域网")
                    .size(theme::BODY_SIZE)
                    .color(palette.text_secondary),
            );
        }
    });
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
