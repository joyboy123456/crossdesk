use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, CursorIcon, FontData, FontDefinitions,
    FontFamily, FontId, Frame, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use iconflow::{Pack, Size, Style, fonts, try_icon};
use lan_mouse_ipc::{ClientHandle, FrontendEvent, FrontendRequest, Position, Status};

#[cfg(target_os = "macos")]
use crate::macos_privacy::{self, PermissionState};
use crate::{
    bridge::{Bridge, BridgeEvent},
    model::{DeviceDraft, UiClient, UiState, displayed_position, position_label},
    theme,
    tray::{TrayAction, TrayController},
};

const SIDEBAR_WIDTH: f32 = 200.0;
const THEME_STORAGE_KEY: &str = "crossdesk_theme";
const SCREEN_LAYOUT_DESIGN_WIDTH: f32 = 574.0;
const SLOT_DROP_TOLERANCE: f32 = 12.0;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Page {
    Devices,
    Authorization,
    Settings,
}

struct Editor {
    handle: Option<ClientHandle>,
    draft: DeviceDraft,
    error: Option<String>,
}

struct PendingPosition {
    target: Position,
    started: Instant,
}

impl PendingPosition {
    fn expired(&self) -> bool {
        self.started.elapsed() > Duration::from_secs(3)
    }
}

struct Notice {
    text: String,
    error: bool,
    created: Instant,
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

    fn devices_page(&mut self, ui: &mut egui::Ui) {
        self.screen_layout(ui);
        ui.add_space(16.0);

        theme::section_label(ui, "设备");
        ui.add_space(4.0);
        if self.state.clients.is_empty() {
            theme::card_frame(ui).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(icon("monitor", 20.0, theme::palette(ui).text_muted));
                    ui.label("还没有已配置的设备");
                });
            });
        } else {
            let clients = self.state.clients.values().cloned().collect::<Vec<_>>();
            for client in clients {
                self.device_row(ui, &client);
                ui.add_space(6.0);
            }
        }
    }

    fn screen_layout(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        let width = ui.available_width().max(1.0);
        let (canvas, _) = ui.allocate_exact_size(Vec2::new(width, 300.0), Sense::hover());
        let painter = ui.painter_at(canvas);
        painter.rect_filled(
            canvas,
            CornerRadius::same(theme::CANVAS_RADIUS),
            palette.bg_canvas,
        );
        theme::radial_glow(&painter, canvas.center(), palette.glow);
        painter.rect_stroke(
            canvas,
            CornerRadius::same(theme::CANVAS_RADIUS),
            Stroke::new(1.0, palette.border),
            StrokeKind::Inside,
        );

        let (center, slots) = screen_layout_geometry(canvas);

        for (pos, rect) in slots {
            if !position_occupied(&self.state, &self.pending_positions, pos, None) {
                painter.rect_filled(
                    rect,
                    CornerRadius::same(theme::CARD_RADIUS),
                    palette.surface.gamma_multiply(0.35),
                );
                theme::dashed_rect_stroke(&painter, rect, Stroke::new(1.0, palette.border_strong));
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    position_label(pos),
                    FontId::proportional(theme::CAPTION_SIZE),
                    palette.text_muted,
                );
            }
        }

        // Interactions first, so drag targeting can drive slot highlights below.
        let mut interactions = Vec::new();
        let clients = self
            .state
            .clients
            .values()
            .filter(|client| client.state.active)
            .cloned()
            .collect::<Vec<_>>();
        for client in &clients {
            let pending = self.pending_positions.get(&client.handle);
            let drag_enabled = self.connected && pending.is_none();
            let display_position =
                displayed_position(client.config.pos, pending.map(|pending| pending.target));
            let base_rect = slots
                .iter()
                .find_map(|(pos, rect)| (*pos == display_position).then_some(*rect))
                .unwrap_or(center);
            let mut response = ui.interact(
                base_rect,
                Id::new(("screen", client.handle)),
                if drag_enabled {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                },
            );
            let tooltip = if pending.is_some() {
                "屏幕方向正在保存"
            } else if !self.connected {
                "后台服务未连接"
            } else {
                "拖动调整屏幕方向"
            };
            response = response
                .on_hover_cursor(if drag_enabled {
                    CursorIcon::Grab
                } else {
                    CursorIcon::PointingHand
                })
                .on_hover_text(tooltip);
            if response.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
            let device_name = client.config.hostname.as_deref().unwrap_or("未命名设备");
            let accessibility_label = format!("拖动 {device_name} 调整屏幕方向");
            response.widget_info(|| {
                WidgetInfo::labeled(WidgetType::Button, true, accessibility_label.clone())
            });
            interactions.push((client, pending.is_some(), drag_enabled, base_rect, response));
        }

        // Highlight the slot under the dragged card (red when occupied).
        let drag_target = interactions
            .iter()
            .find_map(|(client, _, _, base_rect, response)| {
                response
                    .dragged()
                    .then(|| base_rect.center() + response.total_drag_delta().unwrap_or_default())
                    .and_then(|center| screen_slot_at(&slots, center))
                    .map(|target| (client.handle, target))
            });
        for (index, (pos, rect)) in slots.iter().enumerate() {
            let current_target = drag_target.filter(|(_, target)| target == pos);
            let targeted = current_target.is_some();
            let t = ui
                .ctx()
                .animate_bool_with_time(Id::new(("slot-hl", index)), targeted, 0.12);
            if let Some((handle, target)) = current_target.filter(|_| t > 0.01) {
                let occupied =
                    position_occupied(&self.state, &self.pending_positions, target, Some(handle));
                let color = if occupied {
                    palette.danger
                } else {
                    palette.accent
                };
                painter.rect_stroke(
                    rect.expand(2.0),
                    CornerRadius::same(theme::CARD_RADIUS),
                    Stroke::new(2.0, color.gamma_multiply(t)),
                    StrokeKind::Inside,
                );
            }
        }

        paint_screen(
            &painter,
            center,
            ScreenPresentation {
                number: "屏幕 1",
                name: &self.local_hostname,
                online: true,
                local: true,
                selected: false,
                pending: false,
                drag_enabled: false,
                drag_hovered: false,
                dragging: false,
            },
            ui,
        );

        for (client, is_pending, drag_enabled, base_rect, response) in interactions {
            let anim_x = Id::new(("screen-anim-x", client.handle));
            let anim_y = Id::new(("screen-anim-y", client.handle));
            let drag_center_id = Id::new(("screen-drag-center", client.handle));
            let visual_rect = if response.dragged() {
                let drag_center =
                    base_rect.center() + response.total_drag_delta().unwrap_or_default();
                ui.ctx()
                    .data_mut(|data| data.insert_temp(drag_center_id, drag_center));
                // Keep the animated value tracking the pointer so the
                // release animation starts from the drop point.
                ui.ctx()
                    .animate_value_with_time(anim_x, drag_center.x, 0.05);
                ui.ctx()
                    .animate_value_with_time(anim_y, drag_center.y, 0.05);
                base_rect.translate(response.total_drag_delta().unwrap_or_default())
            } else {
                let cx = ui
                    .ctx()
                    .animate_value_with_time(anim_x, base_rect.center().x, 0.3);
                let cy = ui
                    .ctx()
                    .animate_value_with_time(anim_y, base_rect.center().y, 0.3);
                Rect::from_center_size(Pos2::new(cx, cy), base_rect.size())
            };

            if response.clicked() {
                self.selected = Some(client.handle);
            }

            paint_screen(
                &painter,
                visual_rect,
                ScreenPresentation {
                    number: &format!("屏幕 {}", self.state.next_screen_number(client.handle)),
                    name: client.config.hostname.as_deref().unwrap_or("未命名设备"),
                    online: client.state.alive,
                    local: false,
                    selected: self.selected == Some(client.handle),
                    pending: is_pending,
                    drag_enabled,
                    drag_hovered: response.hovered(),
                    dragging: response.dragged(),
                },
                ui,
            );

            if response.drag_stopped() {
                let drop_center = ui
                    .ctx()
                    .data_mut(|data| data.remove_temp(drag_center_id))
                    .unwrap_or_else(|| base_rect.center());
                let target = screen_slot_at(&slots, drop_center);
                if let Some(target) = target.filter(|target| *target != client.config.pos) {
                    if position_occupied(
                        &self.state,
                        &self.pending_positions,
                        target,
                        Some(client.handle),
                    ) {
                        self.show_notice(format!("{}已有启用设备", position_label(target)), true);
                    } else if self.send(FrontendRequest::UpdatePosition(client.handle, target)) {
                        self.pending_positions.insert(
                            client.handle,
                            PendingPosition {
                                target,
                                started: Instant::now(),
                            },
                        );
                    }
                }
            }
        }
    }

    fn device_row(&mut self, ui: &mut egui::Ui, client: &UiClient) {
        let palette = theme::palette(ui);
        let selected = self.selected == Some(client.handle);
        let selected_t = ui.ctx().animate_bool_with_time(
            Id::new(("device-row-selected", client.handle)),
            selected,
            0.15,
        );
        let fill = theme::lerp_color(palette.surface, palette.accent_soft, selected_t);
        let stroke = theme::lerp_color(palette.border, palette.accent, selected_t);
        let frame = Frame::new()
            .fill(fill)
            .stroke(Stroke::new(1.0, stroke))
            .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
            .inner_margin(16)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(
                            selected,
                            RichText::new(
                                client.config.hostname.as_deref().unwrap_or("未命名设备"),
                            )
                            .strong(),
                        )
                        .clicked()
                    {
                        self.selected = Some(client.handle);
                    }
                    let status = if !client.state.active {
                        (palette.text_muted, "已停用")
                    } else if client.state.alive {
                        (palette.success, "在线")
                    } else {
                        (palette.warning, "等待连接")
                    };
                    theme::status_badge(ui, status.0, status.1);
                    ui.label(
                        RichText::new(position_label(client.config.pos)).color(palette.text_muted),
                    );

                    if client
                        .state
                        .peer_commit
                        .is_some_and(|commit| commit != self.local_commit)
                    {
                        theme::status_badge(ui, palette.warning, "版本不同")
                            .on_hover_text("两端构建版本不一致");
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, "trash-2", "删除设备", palette.danger).clicked() {
                            self.delete_confirmation = Some(client.handle);
                        }
                        if icon_button(ui, "pencil", "编辑设备", palette.text_muted).clicked() {
                            self.editor = Some(Editor {
                                handle: Some(client.handle),
                                draft: DeviceDraft::from_client(client),
                                error: None,
                            });
                        }
                        if icon_button(ui, "refresh-cw", "重新解析主机", palette.text_muted)
                            .clicked()
                        {
                            self.send(FrontendRequest::ResolveDns(client.handle));
                        }

                        let mut active = client.state.active;
                        if ui.checkbox(&mut active, "启用").changed() {
                            if active
                                && position_occupied(
                                    &self.state,
                                    &self.pending_positions,
                                    client.config.pos,
                                    Some(client.handle),
                                )
                            {
                                self.show_notice(
                                    format!(
                                        "{}已有启用设备，请先调整方向",
                                        position_label(client.config.pos)
                                    ),
                                    true,
                                );
                            } else {
                                self.send(FrontendRequest::Activate(client.handle, active));
                            }
                        }
                    });
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("端口 {}", client.config.port))
                            .color(palette.text_muted),
                    );
                    if !client.config.fix_ips.is_empty() {
                        ui.label(
                            RichText::new(
                                client
                                    .config
                                    .fix_ips
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            )
                            .color(palette.text_muted),
                        );
                    }
                    if client.state.resolving {
                        ui.spinner();
                    }
                });
            });

        if selected_t > 0.01 {
            let rect = frame.response.rect;
            ui.painter().rect_filled(
                Rect::from_min_size(rect.min + Vec2::new(5.0, 10.0), Vec2::new(3.0, 20.0)),
                CornerRadius::same(2),
                palette.accent.gamma_multiply(selected_t),
            );
        }
    }

    fn authorization_page(&mut self, ui: &mut egui::Ui) {
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

    fn settings_page(&mut self, ui: &mut egui::Ui) {
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
    fn macos_permissions_section(&mut self, ui: &mut egui::Ui) {
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

    fn editor_dialog(&mut self, ctx: &egui::Context) {
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

    fn delete_dialog(&mut self, ctx: &egui::Context) {
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

    fn notice(&mut self, ctx: &egui::Context) {
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

fn nav_item(ui: &mut egui::Ui, current: Page, page: Page, icon_name: &str, label: &str) -> bool {
    let palette = theme::palette(ui);
    let selected = current == page;
    let t = ui
        .ctx()
        .animate_bool_with_time(Id::new(("nav", label)), selected, 0.15);
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
    response = response.on_hover_cursor(CursorIcon::PointingHand);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));

    if response.hovered() && !selected {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(theme::WIDGET_RADIUS),
            palette.surface_raised,
        );
    }
    if t > 0.01 {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(theme::WIDGET_RADIUS),
            palette.accent_soft.gamma_multiply(t),
        );
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min + Vec2::new(4.0, 9.0), Vec2::new(3.0, 20.0)),
            CornerRadius::same(2),
            palette.accent.gamma_multiply(t),
        );
    }

    let emphasized = t > 0.5 || response.hovered();
    let (glyph, family) = icon_glyph(icon_name);
    ui.painter().text(
        Pos2::new(rect.left() + 21.0, rect.center().y),
        Align2::CENTER_CENTER,
        glyph.to_string(),
        theme::icon_font(&family, 16.0),
        theme::lerp_color(palette.text_secondary, palette.accent, t),
    );
    ui.painter().text(
        Pos2::new(rect.left() + 40.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(theme::BODY_SIZE),
        if emphasized {
            palette.text
        } else {
            palette.text_secondary
        },
    );

    response.clicked()
}

fn dialog_frame(ui: &egui::Ui) -> Frame {
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

struct ScreenPresentation<'a> {
    number: &'a str,
    name: &'a str,
    online: bool,
    local: bool,
    selected: bool,
    pending: bool,
    drag_enabled: bool,
    drag_hovered: bool,
    dragging: bool,
}

fn screen_layout_geometry(canvas: Rect) -> (Rect, [(Position, Rect); 4]) {
    let scale = (canvas.width() / SCREEN_LAYOUT_DESIGN_WIDTH).min(1.0);
    let center = Rect::from_center_size(canvas.center(), Vec2::new(190.0, 104.0) * scale);
    (center, screen_slots(center, scale))
}

fn screen_slots(center: Rect, scale: f32) -> [(Position, Rect); 4] {
    [
        (
            Position::Left,
            Rect::from_center_size(
                Pos2::new(center.left() - 108.0 * scale, center.center().y),
                Vec2::new(168.0, 92.0) * scale,
            ),
        ),
        (
            Position::Right,
            Rect::from_center_size(
                Pos2::new(center.right() + 108.0 * scale, center.center().y),
                Vec2::new(168.0, 92.0) * scale,
            ),
        ),
        (
            Position::Top,
            Rect::from_center_size(
                Pos2::new(center.center().x, center.top() - 52.0 * scale),
                Vec2::new(168.0, 82.0) * scale,
            ),
        ),
        (
            Position::Bottom,
            Rect::from_center_size(
                Pos2::new(center.center().x, center.bottom() + 52.0 * scale),
                Vec2::new(168.0, 82.0) * scale,
            ),
        ),
    ]
}

fn screen_slot_at(slots: &[(Position, Rect); 4], point: Pos2) -> Option<Position> {
    slots.iter().find_map(|(position, rect)| {
        rect.expand(SLOT_DROP_TOLERANCE)
            .contains(point)
            .then_some(*position)
    })
}

fn screen_handle_rect(rect: Rect) -> Rect {
    Rect::from_center_size(
        Pos2::new(rect.left() + 14.0, rect.center().y),
        Vec2::new(20.0, 32.0),
    )
}

fn screen_content_rect(rect: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(rect.left() + 30.0, rect.top() + 8.0),
        Pos2::new(rect.right() - 28.0, rect.bottom() - 8.0),
    )
}

fn screen_status_rect(rect: Rect) -> Rect {
    Rect::from_center_size(
        Pos2::new(rect.right() - 12.0, rect.top() + 12.0),
        Vec2::splat(13.0),
    )
}

fn position_occupied(
    state: &UiState,
    pending_positions: &HashMap<ClientHandle, PendingPosition>,
    pos: Position,
    except: Option<ClientHandle>,
) -> bool {
    state.occupied(pos, except)
        || pending_positions
            .iter()
            .any(|(handle, pending)| Some(*handle) != except && pending.target == pos)
}

fn paint_screen(
    painter: &egui::Painter,
    rect: Rect,
    screen: ScreenPresentation<'_>,
    ui: &egui::Ui,
) {
    let palette = theme::palette(ui);

    if screen.selected {
        theme::glow_stroke(painter, rect, theme::CARD_RADIUS, palette.glow);
    }

    painter.rect(
        rect,
        CornerRadius::same(theme::CARD_RADIUS),
        if screen.local {
            palette.accent
        } else {
            palette.surface_raised
        },
        Stroke::new(
            if screen.selected || screen.dragging {
                2.0
            } else {
                1.0
            },
            if screen.dragging || screen.selected {
                palette.accent
            } else if screen.pending {
                palette.warning
            } else {
                palette.border_strong
            },
        ),
        StrokeKind::Inside,
    );
    // Inner hairline for a double-border look.
    painter.rect_stroke(
        rect.shrink(3.0),
        CornerRadius::same(theme::CARD_RADIUS - 3),
        Stroke::new(
            1.0,
            if screen.local {
                Color32::WHITE.gamma_multiply(0.35)
            } else {
                palette.border.gamma_multiply(0.6)
            },
        ),
        StrokeKind::Inside,
    );

    if !screen.local {
        let handle_rect = screen_handle_rect(rect);
        if screen.drag_hovered && screen.drag_enabled {
            painter.rect_filled(
                handle_rect,
                CornerRadius::same(theme::WIDGET_RADIUS),
                palette.accent.gamma_multiply(0.12),
            );
        }
        let handle_color = if screen.dragging {
            palette.accent
        } else if screen.drag_enabled {
            palette.text_muted
        } else {
            palette.border_strong
        };
        let (glyph, family) = icon_glyph("grip-vertical");
        painter.text(
            handle_rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            theme::icon_font(&family, 15.0),
            handle_color,
        );
    }

    let content_rect = if screen.local {
        rect.shrink2(Vec2::new(16.0, 8.0))
    } else {
        screen_content_rect(rect)
    };

    painter.text(
        Pos2::new(content_rect.center().x, rect.center().y - 12.0),
        Align2::CENTER_CENTER,
        screen.number,
        FontId::proportional(18.0),
        if screen.local {
            Color32::WHITE
        } else {
            palette.text
        },
    );
    painter.text(
        Pos2::new(content_rect.center().x, rect.center().y + 14.0),
        Align2::CENTER_CENTER,
        screen.name,
        FontId::proportional(13.0),
        if screen.local {
            Color32::from_rgb(219, 234, 254)
        } else {
            palette.text_muted
        },
    );

    let dot_center = screen_status_rect(rect).center();
    if screen.pending {
        // Breathing pulse while the backend has not confirmed the new position.
        let time = ui.ctx().input(|input| input.time) as f32;
        let pulse = 0.5 + 0.5 * (time * 4.0).sin();
        painter.circle_filled(
            dot_center,
            6.5,
            palette.warning.gamma_multiply(0.2 + 0.3 * pulse),
        );
        painter.circle_filled(
            dot_center,
            3.5,
            palette.warning.gamma_multiply(0.55 + 0.45 * pulse),
        );
    } else {
        painter.circle_filled(
            dot_center,
            3.5,
            if screen.online {
                palette.success
            } else {
                palette.warning
            },
        );
    }
}

fn settings_row(ui: &mut egui::Ui, label: &str, value: &str, color: Option<Color32>) {
    theme::card_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(color) = color {
                    ui.horizontal(|ui| {
                        theme::status_dot(ui, color);
                        ui.colored_label(color, value);
                    });
                } else {
                    ui.label(value);
                }
            });
        });
    });
}

fn icon_button(ui: &mut egui::Ui, name: &str, tooltip: &str, color: Color32) -> egui::Response {
    ui.add(Button::new(icon(name, 16.0, color)).frame(false))
        .on_hover_text(tooltip)
}

fn icon_glyph(name: &str) -> (char, String) {
    let icon =
        try_icon(Pack::Lucide, name, Style::Regular, Size::Regular).expect("required Lucide icon");
    (
        char::from_u32(icon.codepoint).unwrap_or('?'),
        icon.family.to_owned(),
    )
}

fn icon(name: &str, size: f32, color: Color32) -> RichText {
    let (glyph, family) = icon_glyph(name);
    RichText::new(glyph.to_string())
        .font(theme::icon_font(&family, size))
        .color(color)
}

fn install_fonts(ctx: &egui::Context) {
    let mut definitions = FontDefinitions::default();
    let fallback_fonts = definitions.font_data.keys().cloned().collect::<Vec<_>>();
    for font in fonts() {
        definitions.font_data.insert(
            font.family.to_owned(),
            std::sync::Arc::new(FontData::from_static(font.bytes)),
        );
        let family = definitions
            .families
            .entry(FontFamily::Name(font.family.into()))
            .or_default();
        family.push(font.family.to_owned());
        family.extend(fallback_fonts.iter().cloned());
    }

    if let Some(bytes) = load_cjk_font() {
        definitions.font_data.insert(
            "crossdesk-cjk".into(),
            std::sync::Arc::new(FontData::from_owned(bytes)),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            definitions
                .families
                .entry(family)
                .or_default()
                .insert(0, "crossdesk-cjk".into());
        }
    } else {
        log::warn!("no supported CJK system font found");
    }
    ctx.set_fonts(definitions);
}

fn load_cjk_font() -> Option<Vec<u8>> {
    #[cfg(windows)]
    let candidates = {
        let windows_root = std::env::var("WINDIR")
            .or_else(|_| std::env::var("SystemRoot"))
            .unwrap_or_else(|_| r"C:\Windows".to_owned());
        [
            Some(format!("{windows_root}\\Fonts\\msyh.ttc")),
            Some(format!("{windows_root}\\Fonts\\msyhbd.ttc")),
        ]
    };
    #[cfg(target_os = "macos")]
    let candidates = [
        Some("/System/Library/Fonts/PingFang.ttc".to_owned()),
        Some("/System/Library/Fonts/STHeiti Medium.ttc".to_owned()),
    ];
    #[cfg(not(any(windows, target_os = "macos")))]
    let candidates = [
        Some("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".to_owned()),
        Some("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".to_owned()),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(|path| std::fs::read(path).ok())
}

fn short_fingerprint(fingerprint: &str) -> String {
    if fingerprint.chars().count() > 24 {
        format!("{}…", fingerprint.chars().take(24).collect::<String>())
    } else {
        fingerprint.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Theme;
    use egui::accesskit::Role;
    use egui_kittest::{Harness, kittest::Queryable as _};

    fn app_harness() -> Harness<'static, CrossDeskApp> {
        Harness::builder()
            .with_size(Vec2::new(960.0, 680.0))
            .build_eframe(|cc| CrossDeskApp::for_test(cc))
    }

    fn add_active_screen(
        app: &mut CrossDeskApp,
        handle: ClientHandle,
        position: Position,
        name: &str,
        alive: bool,
    ) {
        app.state.apply(FrontendEvent::Created(
            handle,
            lan_mouse_ipc::ClientConfig {
                hostname: Some(name.into()),
                pos: position,
                ..Default::default()
            },
            lan_mouse_ipc::ClientState {
                active: true,
                alive,
                ..Default::default()
            },
        ));
    }

    fn drag_screen(
        harness: &mut Harness<'static, CrossDeskApp>,
        accessibility_label: &str,
        delta: Vec2,
    ) {
        let start = harness
            .get_by_role_and_label(Role::Button, accessibility_label)
            .rect()
            .center();
        let destination = start + delta;
        harness.drag_at(start);
        harness.step();
        harness.hover_at(destination);
        // Keep the pointer down long enough for the target highlight to become
        // non-zero. This covers the release frame while that animation decays.
        harness.run_steps(3);
        harness.event(egui::Event::PointerButton {
            pos: destination,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
        harness.remove_cursor();
    }

    #[test]
    fn chinese_tabs_expose_accessible_labels_and_switch_pages() {
        let mut harness = app_harness();
        harness.get_by_role_and_label(Role::Button, "授权").click();
        harness.step();
        harness.get_by_label("设备授权");

        harness.get_by_role_and_label(Role::Button, "设置").click();
        harness.step();
        harness.get_by_label("本机设置");

        harness.get_by_role_and_label(Role::Button, "设备").click();
        harness.step();
        harness.get_by_label("屏幕布局");
    }

    #[test]
    fn theme_button_switches_between_dark_and_light() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(960.0, 680.0))
            .with_theme(Theme::Dark)
            .build_eframe(|cc| CrossDeskApp::for_test(cc));

        harness
            .get_by_role_and_label(Role::Button, "切换到亮色主题")
            .click();
        harness.run_steps(2);
        assert_eq!(harness.ctx.theme(), Theme::Light);
        assert_eq!(harness.state().selected_theme, Theme::Light);

        harness
            .get_by_role_and_label(Role::Button, "切换到暗色主题")
            .click();
        harness.run_steps(2);
        assert_eq!(harness.ctx.theme(), Theme::Dark);
        assert_eq!(harness.state().selected_theme, Theme::Dark);
    }

    #[test]
    fn clipboard_checkbox_sends_persistent_setting_request() {
        let mut harness = app_harness();
        harness.state_mut().page = Page::Settings;
        harness.step();

        harness.get_by_label("同步文本剪贴板").click();
        harness.step();

        assert_eq!(
            harness.state().bridge.try_test_request(),
            Some(FrontendRequest::SetClipboardSync(false))
        );
        assert!(!harness.state().state.clipboard_enabled);
    }

    #[test]
    fn device_editor_selects_direction_and_sends_complete_create() {
        let mut harness = app_harness();
        harness
            .get_by_role_and_label(Role::Button, "添加设备")
            .click();
        harness.step();

        harness
            .get_by_role_and_label(Role::TextInput, "主机名")
            .focus();
        harness.step();
        harness
            .get_by_role_and_label(Role::TextInput, "主机名")
            .type_text("mac-mini.local");
        harness.step();
        assert_eq!(
            harness
                .state()
                .editor
                .as_ref()
                .expect("editor remains open")
                .draft
                .hostname,
            "mac-mini.local"
        );
        harness.get_by_role_and_label(Role::Button, "左侧").click();
        harness.step();
        harness.get_by_role_and_label(Role::Button, "保存").click();
        harness.step();

        let request = harness
            .state()
            .bridge
            .try_test_request()
            .expect("device creation request");
        let FrontendRequest::CreateConfigured { config, active } = request else {
            panic!("expected CreateConfigured request");
        };
        assert_eq!(config.hostname.as_deref(), Some("mac-mini.local"));
        assert_eq!(config.port, lan_mouse_ipc::DEFAULT_PORT);
        assert_eq!(config.pos, Position::Left);
        assert!(active);
    }

    #[test]
    fn authorization_approval_populates_form_and_sends_request() {
        let mut harness = app_harness();
        harness.state_mut().page = Page::Authorization;
        harness
            .state_mut()
            .state
            .pending_authorizations
            .push("aa:bb:cc".into());
        harness.step();

        harness.get_by_role_and_label(Role::Button, "批准").click();
        harness.step();
        assert_eq!(harness.state().auth_fingerprint, "aa:bb:cc");

        harness
            .get_by_role_and_label(Role::TextInput, "设备名称")
            .focus();
        harness.step();
        harness
            .get_by_role_and_label(Role::TextInput, "设备名称")
            .type_text("MacBook");
        harness.step();
        assert_eq!(harness.state().auth_description, "MacBook");
        harness
            .get_by_role_and_label(Role::Button, "授权设备")
            .click();
        harness.step();

        assert_eq!(
            harness.state().bridge.try_test_request(),
            Some(FrontendRequest::AuthorizeKey(
                "MacBook".into(),
                "aa:bb:cc".into()
            ))
        );
    }

    #[test]
    fn primary_controls_fit_minimum_and_default_sizes_in_both_themes() {
        for theme in [Theme::Dark, Theme::Light] {
            for size in [Vec2::new(760.0, 560.0), Vec2::new(960.0, 680.0)] {
                let harness = Harness::builder()
                    .with_size(size)
                    .with_theme(theme)
                    .build_eframe(|cc| CrossDeskApp::for_test(cc));
                for label in ["设备", "授权", "设置", "添加设备"] {
                    let rect = harness.get_by_role_and_label(Role::Button, label).rect();
                    assert!(
                        rect.min.x >= 0.0 && rect.min.y >= 0.0,
                        "{label} exceeds top/left"
                    );
                    assert!(
                        rect.max.x <= size.x && rect.max.y <= size.y,
                        "{label} exceeds {size:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn long_pages_scroll_at_the_minimum_window_size() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(760.0, 560.0))
            .build_eframe(|cc| CrossDeskApp::for_test(cc));
        harness.state_mut().page = Page::Authorization;
        for index in 0..20 {
            harness.state_mut().state.authorized.insert(
                format!("fingerprint-{index:02}"),
                format!("设备 {index:02}"),
            );
        }
        harness.step();

        let last = harness.get_by_label("设备 19");
        assert!(last.rect().max.y > 560.0);
        for _ in 0..12 {
            last.scroll_down();
        }
        harness.run_steps(2);

        let rect = harness.get_by_label("设备 19").rect();
        assert!(
            rect.min.y >= 0.0 && rect.max.y <= 560.0,
            "scrolled node remains outside viewport: {rect:?}"
        );
    }

    #[test]
    fn screen_slots_stay_inside_responsive_layout_canvases() {
        for width in [500.0, 540.0, 760.0] {
            let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 300.0));
            let (_, slots) = screen_layout_geometry(canvas);

            for (_, slot) in slots {
                assert!(slot.min.x >= canvas.min.x - 0.01);
                assert!(slot.max.x <= canvas.max.x + 0.01);
                assert!(slot.min.y >= canvas.min.y && slot.max.y <= canvas.max.y);
            }
        }
    }

    #[test]
    fn screen_slot_hit_testing_covers_directions_tolerance_and_empty_space() {
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(760.0, 300.0));
        let (center, slots) = screen_layout_geometry(canvas);

        for (position, slot) in slots {
            assert_eq!(screen_slot_at(&slots, slot.center()), Some(position));
        }

        let left = slots
            .iter()
            .find_map(|(position, rect)| (*position == Position::Left).then_some(*rect))
            .expect("left slot");
        assert_eq!(
            screen_slot_at(
                &slots,
                Pos2::new(left.left() - SLOT_DROP_TOLERANCE + 1.0, left.center().y)
            ),
            Some(Position::Left)
        );
        assert_eq!(
            screen_slot_at(
                &slots,
                Pos2::new(left.left() - SLOT_DROP_TOLERANCE - 1.0, left.center().y)
            ),
            None
        );
        assert_eq!(screen_slot_at(&slots, center.center()), None);
    }

    #[test]
    fn draggable_screen_nodes_fit_both_themes_and_supported_window_sizes() {
        for theme in [Theme::Dark, Theme::Light] {
            for size in [Vec2::new(760.0, 560.0), Vec2::new(960.0, 680.0)] {
                let mut harness = Harness::builder()
                    .with_size(size)
                    .with_theme(theme)
                    .build_eframe(|cc| CrossDeskApp::for_test(cc));
                add_active_screen(harness.state_mut(), 3, Position::Right, "Mac-mini-M4", true);
                harness.step();

                let rect = harness
                    .get_by_role_and_label(Role::Button, "拖动 Mac-mini-M4 调整屏幕方向")
                    .rect();
                assert!(rect.min.x >= 0.0 && rect.min.y >= 0.0);
                assert!(rect.max.x <= size.x && rect.max.y <= size.y);

                let handle = screen_handle_rect(rect);
                let content = screen_content_rect(rect);
                let status = screen_status_rect(rect);
                assert!(!handle.intersects(content));
                assert!(!status.intersects(content));
                assert!(!handle.intersects(status));
            }
        }
    }

    #[test]
    fn dragging_offline_screen_to_empty_slot_requests_position_update() {
        let mut harness = app_harness();
        add_active_screen(
            harness.state_mut(),
            3,
            Position::Right,
            "Mac-mini-M4",
            false,
        );
        harness.step();

        drag_screen(
            &mut harness,
            "拖动 Mac-mini-M4 调整屏幕方向",
            Vec2::new(-406.0, 0.0),
        );

        assert_eq!(
            harness.state().bridge.try_test_request(),
            Some(FrontendRequest::UpdatePosition(3, Position::Left))
        );
        assert_eq!(
            harness
                .state()
                .pending_positions
                .get(&3)
                .map(|pending| pending.target),
            Some(Position::Left)
        );
        assert!(harness.state().bridge.try_test_request().is_none());
    }

    #[test]
    fn occupied_invalid_and_current_slot_drops_do_not_send_updates() {
        let mut occupied = app_harness();
        add_active_screen(
            occupied.state_mut(),
            3,
            Position::Right,
            "Mac-mini-M4",
            true,
        );
        add_active_screen(occupied.state_mut(), 4, Position::Left, "Windows-PC", true);
        occupied.step();
        drag_screen(
            &mut occupied,
            "拖动 Mac-mini-M4 调整屏幕方向",
            Vec2::new(-406.0, 0.0),
        );
        assert!(occupied.state().bridge.try_test_request().is_none());
        assert_eq!(
            occupied
                .state()
                .notice
                .as_ref()
                .map(|notice| notice.text.as_str()),
            Some("左侧已有启用设备")
        );

        let mut invalid = app_harness();
        add_active_screen(invalid.state_mut(), 3, Position::Right, "Mac-mini-M4", true);
        invalid.step();
        drag_screen(
            &mut invalid,
            "拖动 Mac-mini-M4 调整屏幕方向",
            Vec2::new(30.0, 0.0),
        );
        drag_screen(
            &mut invalid,
            "拖动 Mac-mini-M4 调整屏幕方向",
            Vec2::new(0.0, 240.0),
        );
        assert!(invalid.state().bridge.try_test_request().is_none());
        assert!(invalid.state().pending_positions.is_empty());
    }

    #[test]
    fn disconnected_and_pending_screen_nodes_cannot_be_dragged() {
        let mut disconnected = app_harness();
        disconnected.state_mut().connected = false;
        add_active_screen(
            disconnected.state_mut(),
            3,
            Position::Right,
            "Mac-mini-M4",
            true,
        );
        disconnected.step();
        drag_screen(
            &mut disconnected,
            "拖动 Mac-mini-M4 调整屏幕方向",
            Vec2::new(-406.0, 0.0),
        );
        assert!(disconnected.state().bridge.try_test_request().is_none());

        let mut pending = app_harness();
        add_active_screen(pending.state_mut(), 3, Position::Right, "Mac-mini-M4", true);
        pending.state_mut().pending_positions.insert(
            3,
            PendingPosition {
                target: Position::Right,
                started: Instant::now(),
            },
        );
        pending.step();
        drag_screen(
            &mut pending,
            "拖动 Mac-mini-M4 调整屏幕方向",
            Vec2::new(-406.0, 0.0),
        );
        assert!(pending.state().bridge.try_test_request().is_none());
        assert_eq!(
            pending.state().pending_positions[&3].target,
            Position::Right
        );
    }

    #[test]
    fn pending_directions_reserve_their_target_slot() {
        let mut state = UiState::new();
        state.apply(FrontendEvent::Created(
            3,
            lan_mouse_ipc::ClientConfig {
                pos: Position::Right,
                ..Default::default()
            },
            lan_mouse_ipc::ClientState {
                active: true,
                ..Default::default()
            },
        ));
        let pending = HashMap::from([(
            3,
            PendingPosition {
                target: Position::Left,
                started: Instant::now(),
            },
        )]);

        assert!(position_occupied(&state, &pending, Position::Left, None));
        assert!(position_occupied(&state, &pending, Position::Right, None));
        assert!(!position_occupied(
            &state,
            &pending,
            Position::Left,
            Some(3)
        ));
        assert!(position_occupied(&state, &pending, Position::Left, Some(8)));
    }

    #[test]
    fn disconnected_ui_rejects_commands_immediately() {
        let mut harness = app_harness();
        let app = harness.state_mut();
        app.connected = false;

        assert!(!app.send(FrontendRequest::UpdatePosition(3, Position::Left)));
        assert!(app.bridge.try_test_request().is_none());
        assert_eq!(
            app.notice.as_ref().map(|notice| notice.text.as_str()),
            Some("后台服务尚未连接，请稍后重试")
        );
    }

    #[test]
    fn pending_direction_is_confirmed_by_state_event() {
        let mut harness = app_harness();
        let app = harness.state_mut();
        app.pending_positions.insert(
            3,
            PendingPosition {
                target: Position::Left,
                started: Instant::now(),
            },
        );
        app.apply_frontend_event(FrontendEvent::State(
            3,
            lan_mouse_ipc::ClientConfig {
                hostname: Some("mac".into()),
                pos: Position::Left,
                ..Default::default()
            },
            lan_mouse_ipc::ClientState {
                active: true,
                ..Default::default()
            },
        ));

        assert!(!app.pending_positions.contains_key(&3));
        assert_eq!(app.state.clients[&3].config.pos, Position::Left);
    }

    #[test]
    fn disconnect_and_timeout_roll_back_pending_directions() {
        let mut harness = app_harness();
        let app = harness.state_mut();
        app.connected = true;
        app.pending_positions.insert(
            3,
            PendingPosition {
                target: Position::Left,
                started: Instant::now(),
            },
        );
        app.bridge
            .inject_test_event(BridgeEvent::Disconnected("test disconnect".into()));
        app.drain_bridge();
        assert!(!app.connected);
        assert!(app.pending_positions.is_empty());

        app.pending_positions.insert(
            4,
            PendingPosition {
                target: Position::Top,
                started: Instant::now() - Duration::from_secs(4),
            },
        );
        app.drain_bridge();
        assert!(app.pending_positions.is_empty());
        assert_eq!(app.bridge.try_test_request(), Some(FrontendRequest::Sync));
    }
}
