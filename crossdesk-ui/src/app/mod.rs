//! The CrossDesk desktop window.
//!
//! [`CrossDeskApp`] holds the whole UI state and the bridge to the service;
//! the submodules render it. Pages live in [`pages`], reusable drawing
//! helpers in [`widgets`], the modal dialogs in [`dialogs`], and the pure
//! geometry behind the screen-layout editor in [`geometry`] where it can be
//! tested on its own.

mod dialogs;
mod fonts;
mod geometry;
mod pages;
mod widgets;

#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, Frame, Id, Layout, RichText, Sense, Stroke,
    UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use lan_mouse_ipc::{ClientHandle, FrontendEvent, FrontendRequest};

#[cfg(target_os = "macos")]
use crate::macos_privacy::{self, PermissionState};
use crate::{
    bridge::{Bridge, BridgeEvent},
    model::{DeviceDraft, UiState, position_label},
    theme,
    tray::{TrayAction, TrayController},
};

const SIDEBAR_WIDTH: f32 = 200.0;
const THEME_STORAGE_KEY: &str = "crossdesk_theme";

use dialogs::{Editor, Notice, PendingPosition};
use fonts::install_fonts;
use widgets::{icon, icon_button, nav_item};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Page {
    Devices,
    Authorization,
    Settings,
}

pub struct CrossDeskApp {
    bridge: Bridge,
    state: UiState,
    page: Page,
    last_page: Page,
    selected: Option<ClientHandle>,
    editor: Option<Editor>,
    editor_dialog_open: bool,
    delete_confirmation: Option<ClientHandle>,
    delete_dialog_open: bool,
    pending_positions: HashMap<ClientHandle, PendingPosition>,
    connected: bool,
    connection_detail: String,
    notice: Option<Notice>,
    local_hostname: String,
    local_port: String,
    auth_description: String,
    auth_fingerprint: String,
    local_commit: [u8; 8],
    tray: Option<TrayController>,
    owns_service: bool,
    quit_requested: bool,
    selected_theme: egui::Theme,
    #[cfg(target_os = "macos")]
    macos_permissions: PermissionState,
    #[cfg(target_os = "macos")]
    initial_macos_permissions: PermissionState,
    #[cfg(target_os = "macos")]
    last_macos_permission_poll: Instant,
    #[cfg(target_os = "macos")]
    macos_restart_required: bool,
}

impl CrossDeskApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        local_commit: [u8; 8],
        owns_service: bool,
    ) -> Self {
        let tray = match TrayController::new() {
            Ok(tray) => Some(tray),
            Err(error) => {
                log::warn!("failed to create tray icon: {error}");
                None
            }
        };

        let app = Self::from_parts(
            cc,
            local_commit,
            owns_service,
            Bridge::start(cc.egui_ctx.clone()),
            tray,
        );
        #[cfg(target_os = "macos")]
        macos_privacy::fire_initial_prompts();
        app
    }

    fn from_parts(
        cc: &eframe::CreationContext<'_>,
        local_commit: [u8; 8],
        owns_service: bool,
        bridge: Bridge,
        tray: Option<TrayController>,
    ) -> Self {
        install_fonts(&cc.egui_ctx);
        theme::configure_style(&cc.egui_ctx);
        let selected_theme = cc
            .storage
            .and_then(|storage| storage.get_string(THEME_STORAGE_KEY))
            .as_deref()
            .and_then(parse_theme)
            .unwrap_or_else(|| cc.egui_ctx.theme());
        cc.egui_ctx.set_theme(selected_theme);

        #[cfg(target_os = "macos")]
        let macos_permissions = macos_privacy::permission_state();

        Self {
            bridge,
            state: UiState::new(),
            page: Page::Devices,
            last_page: Page::Devices,
            selected: None,
            editor: None,
            editor_dialog_open: false,
            delete_confirmation: None,
            delete_dialog_open: false,
            pending_positions: HashMap::new(),
            connected: false,
            connection_detail: "正在连接后台服务".into(),
            notice: None,
            local_hostname: hostname::get()
                .ok()
                .and_then(|name| name.into_string().ok())
                .unwrap_or_else(|| "本机".into()),
            local_port: lan_mouse_ipc::DEFAULT_PORT.to_string(),
            auth_description: String::new(),
            auth_fingerprint: String::new(),
            local_commit,
            tray,
            owns_service,
            quit_requested: false,
            selected_theme,
            #[cfg(target_os = "macos")]
            macos_permissions,
            #[cfg(target_os = "macos")]
            initial_macos_permissions: macos_permissions,
            #[cfg(target_os = "macos")]
            last_macos_permission_poll: Instant::now(),
            #[cfg(target_os = "macos")]
            macos_restart_required: false,
        }
    }

    #[cfg(test)]
    fn for_test(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::from_parts(cc, *b"test0000", false, Bridge::for_test(), None);
        app.connected = true;
        app.connection_detail = "后台服务已连接".into();
        app
    }

    fn drain_bridge(&mut self) {
        while let Some(event) = self.bridge.try_event() {
            match event {
                BridgeEvent::Connected => {
                    self.connected = true;
                    self.connection_detail = "后台服务已连接".into();
                }
                BridgeEvent::Disconnected(detail) => {
                    self.connected = false;
                    self.connection_detail = detail;
                    if !self.pending_positions.is_empty() {
                        self.pending_positions.clear();
                        self.show_notice("服务连接中断，未确认的方向更改已恢复", true);
                    }
                }
                BridgeEvent::Frontend(event) => self.apply_frontend_event(event),
            }
        }

        if self.bridge.resync_if_overflowed() {
            self.show_notice("界面状态已重新同步", false);
        }

        let expired = self
            .pending_positions
            .iter()
            .filter_map(|(handle, pending)| pending.expired().then_some(*handle))
            .collect::<Vec<_>>();
        if !expired.is_empty() {
            for handle in expired {
                self.pending_positions.remove(&handle);
            }
            let _ = self.bridge.request(FrontendRequest::Sync);
            self.show_notice("方向更新超时，已恢复服务端状态", true);
        }
    }

    fn apply_frontend_event(&mut self, event: FrontendEvent) {
        match &event {
            FrontendEvent::State(handle, config, _) => {
                if self
                    .pending_positions
                    .get(handle)
                    .is_some_and(|pending| pending.target == config.pos)
                {
                    self.pending_positions.remove(handle);
                    self.show_notice("屏幕方向已保存", false);
                }
            }
            FrontendEvent::Deleted(handle) | FrontendEvent::NoSuchClient(handle) => {
                if self.selected == Some(*handle) {
                    self.selected = None;
                }
                self.pending_positions.remove(handle);
            }
            FrontendEvent::PortChanged(port, message) => {
                self.local_port = port.to_string();
                if let Some(message) = message {
                    self.show_notice(message.clone(), true);
                }
            }
            FrontendEvent::Error(message) => self.show_notice(message.clone(), true),
            FrontendEvent::ConnectionAttempt { fingerprint } => {
                self.page = Page::Authorization;
                self.auth_fingerprint = fingerprint.clone();
                self.show_notice("收到新的设备授权请求", false);
            }
            FrontendEvent::DeviceConnected { addr, .. } => {
                self.show_notice(format!("设备已连接：{addr}"), false)
            }
            FrontendEvent::DeviceEntered { addr, pos, .. } => {
                self.show_notice(format!("设备从{}进入：{addr}", position_label(*pos)), false)
            }
            FrontendEvent::IncomingDisconnected(addr) => {
                self.show_notice(format!("设备已断开：{addr}"), true)
            }
            _ => {}
        }

        self.state.apply(event);
        if self.selected.is_none() {
            self.selected = self.state.clients.keys().next().copied();
        }
    }

    fn show_notice(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(Notice {
            text: text.into(),
            error,
            created: Instant::now(),
        });
    }

    fn send(&mut self, request: FrontendRequest) -> bool {
        if !self.connected {
            self.show_notice("后台服务尚未连接，请稍后重试", true);
            return false;
        }

        match self.bridge.request(request) {
            Ok(()) => true,
            Err(error) => {
                self.show_notice(error, true);
                false
            }
        }
    }

    fn side_nav(&mut self, ui: &mut egui::Ui, height: f32) {
        let palette = theme::palette(ui);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(SIDEBAR_WIDTH, height), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, palette.surface);
        ui.painter().vline(
            rect.right() - 0.5,
            rect.y_range(),
            Stroke::new(1.0, palette.border),
        );

        let inner = rect.shrink2(Vec2::new(14.0, 18.0));
        let mut nav = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::top_down(Align::LEFT)),
        );
        nav.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);

        nav.horizontal(|ui| {
            ui.label(icon("monitor", 20.0, palette.accent));
            ui.label(RichText::new("CrossDesk").size(17.0).strong());
        });
        nav.add_space(22.0);

        for (page, icon_name, label) in [
            (Page::Devices, "layout-grid", "设备"),
            (Page::Authorization, "shield-check", "授权"),
            (Page::Settings, "settings", "设置"),
        ] {
            if nav_item(&mut nav, self.page, page, icon_name, label) {
                self.page = page;
            }
        }

        let remaining = nav.available_height() - 20.0;
        if remaining > 0.0 {
            nav.add_space(remaining);
        }
        let (color, text) = if self.connected {
            (palette.success, "服务已连接")
        } else {
            (palette.warning, "正在重连")
        };
        nav.horizontal(|ui| {
            theme::status_dot(ui, color);
            ui.label(
                RichText::new(text)
                    .size(theme::CAPTION_SIZE)
                    .color(palette.text_secondary),
            );
        })
        .response
        .on_hover_text(&self.connection_detail);
    }

    fn page_header(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        let (title, subtitle) = match self.page {
            Page::Devices => ("屏幕布局", "拖动屏幕到四个方向，调整设备相对位置"),
            Page::Authorization => ("设备授权", "管理证书指纹，审批待授权设备"),
            Page::Settings => ("本机设置", "主题、端口、剪贴板与运行状态"),
        };
        ui.horizontal(|ui| {
            ui.label(theme::title_text(title));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (next_theme, icon_name, tooltip) = match self.selected_theme {
                    egui::Theme::Dark => (egui::Theme::Light, "sun", "切换到亮色主题"),
                    egui::Theme::Light => (egui::Theme::Dark, "moon", "切换到暗色主题"),
                };
                let theme_button = icon_button(ui, icon_name, tooltip, palette.text_secondary);
                theme_button.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, tooltip));
                if theme_button.clicked() {
                    self.selected_theme = next_theme;
                    ui.ctx().set_theme(next_theme);
                }
                if self.page == Page::Devices
                    && ui
                        .add(
                            Button::new(RichText::new("添加设备").color(Color32::WHITE))
                                .fill(palette.accent),
                        )
                        .clicked()
                {
                    self.editor = Some(Editor {
                        handle: None,
                        draft: DeviceDraft::default(),
                        error: None,
                    });
                }
            });
        });
        ui.label(
            RichText::new(subtitle)
                .size(theme::BODY_SIZE)
                .color(palette.text_secondary),
        );
    }

    fn handle_window_lifecycle(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        self.poll_macos_permissions(ctx);

        if let Some(action) = self.tray.as_ref().and_then(TrayController::poll) {
            match action {
                TrayAction::Open => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::Quit => self.request_quit(ctx),
            }
        }

        if ctx.input(|input| input.viewport().close_requested()) && !self.quit_requested {
            if self.tray.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.request_quit(ctx);
            }
        }
    }

    fn request_quit(&mut self, ctx: &egui::Context) {
        if self.quit_requested {
            return;
        }
        self.quit_requested = true;
        if self.owns_service {
            let _ = self.bridge.request(FrontendRequest::ShutdownService);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    #[cfg(target_os = "macos")]
    fn poll_macos_permissions(&mut self, ctx: &egui::Context) {
        if self.last_macos_permission_poll.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_macos_permission_poll = Instant::now();

        let current = macos_privacy::permission_state();
        if self.macos_permissions.any_granted() && current.any_revoked_from(self.macos_permissions)
        {
            log::warn!("macOS input permission was revoked; exiting for input safety");
            self.request_quit(ctx);
            return;
        }

        if current.any_granted_from(self.initial_macos_permissions) {
            self.macos_restart_required = true;
            self.show_notice("权限已授予，请重启 CrossDesk 使输入服务生效", false);
        }
        self.macos_permissions = current;
    }
}

impl eframe::App for CrossDeskApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(
            THEME_STORAGE_KEY,
            match self.selected_theme {
                egui::Theme::Dark => "dark",
                egui::Theme::Light => "light",
            }
            .to_owned(),
        );
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_window_lifecycle(ctx);
        self.drain_bridge();
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let palette = theme::palette(ui);
        // Since egui 0.35 a Frame hugs its content instead of filling the
        // available space, so the window background must be painted explicitly.
        ui.painter().rect_filled(
            ui.available_rect_before_wrap(),
            CornerRadius::ZERO,
            palette.bg,
        );
        Frame::central_panel(ui.style()).show(ui, |ui| {
            let height = ui.available_height();
            ui.horizontal(|ui| {
                self.side_nav(ui, height);

                let content_rect = ui
                    .available_rect_before_wrap()
                    .shrink2(Vec2::new(5.0, 18.0));
                let mut content = ui.new_child(
                    UiBuilder::new()
                        .max_rect(content_rect)
                        .layout(Layout::top_down(Align::LEFT)),
                );
                self.page_header(&mut content);
                content.add_space(10.0);

                // Fade + slide the page in whenever it changes.
                let ctx = content.ctx().clone();
                if self.page != self.last_page {
                    self.last_page = self.page;
                    ctx.animate_value_with_time(Id::new("page-fade"), 0.0, 0.0);
                }
                let fade = ctx.animate_value_with_time(Id::new("page-fade"), 1.0, 0.2);

                let page_height = content.available_height().max(0.0);
                egui::ScrollArea::vertical()
                    .id_salt("page-scroll")
                    .auto_shrink([false, false])
                    .max_height(page_height)
                    .show(&mut content, |ui| {
                        ui.multiply_opacity(fade);
                        ui.add_space((1.0 - fade) * 8.0);
                        match self.page {
                            Page::Devices => self.devices_page(ui),
                            Page::Authorization => self.authorization_page(ui),
                            Page::Settings => self.settings_page(ui),
                        }
                    });
            });
        });

        let ctx = ui.ctx().clone();
        // egui's animate_* calls request repaints while they run; the breathing
        // pending dot and the notice lifecycle need explicit repaints instead.
        if !self.pending_positions.is_empty() || self.notice.is_some() {
            ctx.request_repaint();
        }

        self.editor_dialog(&ctx);
        self.delete_dialog(&ctx);
        self.notice(&ctx);
    }
}

fn parse_theme(value: &str) -> Option<egui::Theme> {
    match value {
        "dark" => Some(egui::Theme::Dark),
        "light" => Some(egui::Theme::Light),
        _ => None,
    }
}
