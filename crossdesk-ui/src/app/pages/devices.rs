//! The device list and the drag-and-drop screen layout editor.

use std::time::Instant;

use eframe::egui::{
    self, Align, Align2, CornerRadius, CursorIcon, FontId, Id, Layout, Pos2, Rect, RichText, Sense,
    Stroke, StrokeKind, Vec2, WidgetInfo, WidgetType,
};
use lan_mouse_ipc::{FrontendRequest, Position};

use crate::app::CrossDeskApp;
use crate::app::dialogs::{Editor, PendingPosition};
use crate::app::geometry::{position_occupied, screen_layout_geometry, screen_slot_at};
use crate::app::session::{self, Role};
use crate::app::widgets::{
    ScreenPresentation, icon_button, icon_glyph, icon_tile, paint_screen, primary_button,
    toggle_switch,
};
use crate::model::{DeviceDraft, UiClient, displayed_position, position_label};
use crate::theme;

const CANVAS_HEIGHT: f32 = 320.0;

/// How a link between the local screen and a peer is drawn.
#[derive(Clone, Copy, PartialEq)]
enum LinkState {
    /// input is flowing over this link right now, in the session color;
    /// `toward_peer` when this machine is the one driving
    Live {
        color: egui::Color32,
        toward_peer: bool,
    },
    Online,
    Offline,
}

impl CrossDeskApp {
    pub(crate) fn devices_page(&mut self, ui: &mut egui::Ui) {
        self.screen_layout(ui);
        ui.add_space(8.0);

        let palette = theme::palette(ui);
        ui.horizontal(|ui| {
            theme::section_label(ui, "设备");
            if !self.state.clients.is_empty() {
                ui.label(
                    RichText::new(self.state.clients.len().to_string())
                        .size(theme::CAPTION_SIZE)
                        .color(palette.text_muted),
                );
            }
        });
        if self.state.clients.is_empty() {
            self.empty_devices(ui);
        } else {
            let clients = self.state.clients.values().cloned().collect::<Vec<_>>();
            for client in clients {
                self.device_row(ui, &client);
                ui.add_space(8.0);
            }
        }
    }

    fn empty_devices(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        let mut scan = false;
        theme::full_card(ui, theme::card_frame(ui), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                icon_tile(ui, "radar", 42.0, palette.accent, false);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(
                        RichText::new("还没有已配置的设备")
                            .size(14.0)
                            .strong()
                            .color(palette.text),
                    );
                    ui.label(
                        RichText::new("在另一台电脑上启动 CrossDesk，再扫描局域网把它加进来")
                            .size(theme::CAPTION_SIZE + 1.0)
                            .color(palette.text_muted),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    scan = primary_button(ui, Some("radar"), "扫描局域网").clicked();
                });
            });
        });
        if scan {
            self.open_scanner(ui.ctx());
        }
    }

    fn screen_layout(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        let width = ui.available_width().max(1.0);
        let (canvas, _) = ui.allocate_exact_size(Vec2::new(width, CANVAS_HEIGHT), Sense::hover());
        let painter = ui.painter_at(canvas);
        painter.rect_filled(
            canvas,
            CornerRadius::same(theme::CANVAS_RADIUS),
            palette.bg_canvas,
        );
        theme::dot_grid(
            &painter,
            canvas.shrink(10.0),
            palette.border_strong.gamma_multiply(0.45),
            20.0,
        );
        theme::radial_glow(
            &painter,
            canvas.center(),
            Vec2::new(canvas.width() * 0.42, canvas.height() * 0.62),
            palette.accent.gamma_multiply(0.10),
        );
        painter.rect_stroke(
            canvas,
            CornerRadius::same(theme::CANVAS_RADIUS),
            Stroke::new(1.0, palette.border),
            StrokeKind::Inside,
        );
        self.paint_canvas_chrome(&painter, canvas, palette);

        let (center, slots) = screen_layout_geometry(canvas);

        for (pos, rect) in slots {
            if !position_occupied(&self.state, &self.pending_positions, pos, None) {
                painter.rect_filled(
                    rect,
                    CornerRadius::same(theme::CARD_RADIUS),
                    palette.surface.gamma_multiply(0.25),
                );
                theme::dashed_rect_stroke(
                    &painter,
                    rect,
                    Stroke::new(1.0, palette.border_strong.gamma_multiply(0.8)),
                );
                let arrow = match pos {
                    Position::Left => "arrow-left",
                    Position::Right => "arrow-right",
                    Position::Top => "arrow-up",
                    Position::Bottom => "arrow-down",
                };
                let (glyph, family) = icon_glyph(arrow);
                painter.text(
                    rect.center() - Vec2::new(0.0, 9.0),
                    Align2::CENTER_CENTER,
                    glyph.to_string(),
                    theme::icon_font(&family, 14.0),
                    palette.text_muted.gamma_multiply(0.7),
                );
                painter.text(
                    rect.center() + Vec2::new(0.0, 10.0),
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
            let visual_rect = self.animated_screen_rect(ui, client, base_rect, &response);
            interactions.push((
                client,
                display_position,
                pending.is_some(),
                drag_enabled,
                base_rect,
                visual_rect,
                response,
            ));
        }

        // Highlight the slot under the dragged card (red when occupied).
        let drag_target =
            interactions
                .iter()
                .find_map(|(client, _, _, _, base_rect, _, response)| {
                    response
                        .dragged()
                        .then(|| {
                            base_rect.center() + response.total_drag_delta().unwrap_or_default()
                        })
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
                painter.rect_filled(
                    rect.expand(2.0),
                    CornerRadius::same(theme::CARD_RADIUS),
                    color.gamma_multiply(0.08 * t),
                );
                painter.rect_stroke(
                    rect.expand(2.0),
                    CornerRadius::same(theme::CARD_RADIUS),
                    Stroke::new(2.0, color.gamma_multiply(t)),
                    StrokeKind::Inside,
                );
            }
        }

        // Links from this machine to each peer, under the screens.
        let session = self.state.session();
        let session_color = session::session_color(palette, &session);
        let mut animate = false;
        for (client, position, _, _, _, visual_rect, response) in &interactions {
            if response.dragged() {
                continue;
            }
            let state = match session::peer_role(&session, client.handle) {
                Some(role) => {
                    animate = true;
                    LinkState::Live {
                        color: session_color,
                        toward_peer: role == Role::Controlled,
                    }
                }
                None if client.state.alive => LinkState::Online,
                None => LinkState::Offline,
            };
            paint_link(ui, &painter, center, *visual_rect, *position, state);
        }
        if animate {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
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
                role: session::local_role(&session).map(|role| (role, session_color)),
            },
            ui,
        );

        // Paint the dragged screen last so it floats above the others.
        interactions.sort_by_key(|(.., response)| response.dragged());
        for (client, _, is_pending, drag_enabled, base_rect, visual_rect, response) in interactions
        {
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
                    role: session::peer_role(&session, client.handle)
                        .map(|role| (role, session_color)),
                },
                ui,
            );

            if response.drag_stopped() {
                let drag_center_id = Id::new(("screen-drag-center", client.handle));
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

    /// Where a screen is drawn this frame: under the pointer while dragged,
    /// otherwise easing towards its slot.
    fn animated_screen_rect(
        &self,
        ui: &egui::Ui,
        client: &UiClient,
        base_rect: Rect,
        response: &egui::Response,
    ) -> Rect {
        let anim_x = Id::new(("screen-anim-x", client.handle));
        let anim_y = Id::new(("screen-anim-y", client.handle));
        let drag_center_id = Id::new(("screen-drag-center", client.handle));
        if response.dragged() {
            let drag_center = base_rect.center() + response.total_drag_delta().unwrap_or_default();
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
        }
    }

    /// Caption, drag hint and legend in the canvas corners.
    fn paint_canvas_chrome(&self, painter: &egui::Painter, canvas: Rect, palette: theme::Palette) {
        let inset = Vec2::new(18.0, 16.0);
        painter.text(
            canvas.left_top() + inset,
            Align2::LEFT_TOP,
            "拓扑",
            FontId::proportional(theme::CAPTION_SIZE),
            palette.text_muted,
        );
        painter.text(
            canvas.right_top() + Vec2::new(-inset.x, inset.y),
            Align2::RIGHT_TOP,
            if self.connected {
                "拖动屏幕调整方向"
            } else {
                "服务未连接，暂不可调整"
            },
            FontId::proportional(theme::CAPTION_SIZE),
            palette.text_muted,
        );

        let mut x = canvas.left() + inset.x;
        let y = canvas.bottom() - inset.y - 5.0;
        for (color, label) in [(palette.success, "在线"), (palette.warning, "等待连接")] {
            painter.circle_filled(Pos2::new(x + 3.0, y), 3.0, color);
            let galley = painter.layout_no_wrap(
                label.to_owned(),
                FontId::proportional(theme::CAPTION_SIZE),
                palette.text_muted,
            );
            let width = galley.size().x;
            painter.galley(
                Pos2::new(x + 11.0, y - galley.size().y / 2.0),
                galley,
                palette.text_muted,
            );
            x += 11.0 + width + 14.0;
        }
        // a live link, drawn the way it appears on the canvas
        painter.line_segment(
            [Pos2::new(x, y), Pos2::new(x + 18.0, y)],
            Stroke::new(2.0, palette.accent),
        );
        painter.circle_filled(Pos2::new(x + 11.0, y), 2.2, palette.on_accent);
        painter.text(
            Pos2::new(x + 24.0, y),
            Align2::LEFT_CENTER,
            "光点流向 = 控制方向",
            FontId::proportional(theme::CAPTION_SIZE),
            palette.text_muted,
        );
    }

    fn device_row(&mut self, ui: &mut egui::Ui, client: &UiClient) {
        let palette = theme::palette(ui);
        let selected = self.selected == Some(client.handle);
        let session = self.state.session();
        let role = session::peer_role(&session, client.handle);
        let role_color = session::session_color(palette, &session);
        let selected_t = ui.ctx().animate_bool_with_time(
            Id::new(("device-row-selected", client.handle)),
            selected,
            0.15,
        );
        let status = if role == Some(Role::Controlled) {
            (role_color, "受本机控制")
        } else if role == Some(Role::Controller) {
            (role_color, "正在控制本机")
        } else if !client.state.active {
            (palette.text_muted, "已停用")
        } else if client.state.alive {
            (palette.success, "在线")
        } else {
            (palette.warning, "等待连接")
        };
        let (fill, stroke) = if role.is_some() {
            (
                theme::lerp_color(palette.surface, role_color, 0.10),
                Stroke::new(1.5, role_color),
            )
        } else {
            (
                theme::lerp_color(palette.surface, palette.accent_soft, selected_t * 0.7),
                Stroke::new(
                    1.0,
                    theme::lerp_color(
                        palette.border,
                        palette.accent.gamma_multiply(0.7),
                        selected_t,
                    ),
                ),
            )
        };
        let frame = theme::card_frame(ui)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .fill(fill)
            .stroke(stroke);
        theme::full_card(ui, frame, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                icon_tile(
                    ui,
                    if client.state.active {
                        "monitor"
                    } else {
                        "monitor-off"
                    },
                    40.0,
                    status.0,
                    role.is_some(),
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    // the row is as tall as its badges, so the name centers on them
                    ui.spacing_mut().interact_size.y = 22.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let name = ui
                            .add(
                                egui::Label::new(
                                    RichText::new(
                                        client.config.hostname.as_deref().unwrap_or("未命名设备"),
                                    )
                                    .size(14.0)
                                    .strong()
                                    .color(palette.text),
                                )
                                .sense(Sense::click()),
                            )
                            .on_hover_cursor(CursorIcon::PointingHand);
                        if name.clicked() {
                            self.selected = Some(client.handle);
                        }
                        theme::status_badge(ui, status.0, status.1);
                        if client
                            .state
                            .peer_commit
                            .is_some_and(|commit| commit != self.local_commit)
                        {
                            theme::status_badge(ui, palette.warning, "版本不同")
                                .on_hover_text("两端构建版本不一致");
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let mut meta = vec![
                            position_label(client.config.pos).to_owned(),
                            format!(":{}", client.config.port),
                        ];
                        if !client.config.fix_ips.is_empty() {
                            meta.push(
                                client
                                    .config
                                    .fix_ips
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            );
                        }
                        ui.label(
                            theme::mono_text(meta.join("  ·  "), theme::CAPTION_SIZE + 0.5)
                                .color(palette.text_muted),
                        );
                        if client.state.resolving {
                            ui.spinner();
                        }
                    });
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
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
                    if icon_button(ui, "refresh-cw", "重新解析主机", palette.text_muted).clicked()
                    {
                        self.send(FrontendRequest::ResolveDns(client.handle));
                    }
                    ui.add_space(8.0);

                    let mut active = client.state.active;
                    if toggle_switch(ui, &mut active, "启用").changed() {
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
        });
    }
}

/// The connector between the local screen and a peer at `position`.
fn paint_link(
    ui: &egui::Ui,
    painter: &egui::Painter,
    local: Rect,
    peer: Rect,
    position: Position,
    state: LinkState,
) {
    let palette = theme::palette(ui);
    let (start, end) = match position {
        Position::Left => (local.left_center(), peer.right_center()),
        Position::Right => (local.right_center(), peer.left_center()),
        Position::Top => (local.center_top(), peer.center_bottom()),
        Position::Bottom => (local.center_bottom(), peer.center_top()),
    };
    if start.distance(end) < 2.0 {
        return;
    }
    match state {
        LinkState::Offline => {
            for shape in egui::Shape::dashed_line(
                &[start, end],
                Stroke::new(1.5, palette.text_muted.gamma_multiply(0.6)),
                3.0,
                3.0,
            ) {
                painter.add(shape);
            }
        }
        LinkState::Online => {
            painter.line_segment([start, end], Stroke::new(1.5, palette.border_strong));
        }
        LinkState::Live { color, toward_peer } => {
            // a soft wide halo under a bright core
            painter.line_segment([start, end], Stroke::new(7.0, color.gamma_multiply(0.2)));
            painter.line_segment([start, end], Stroke::new(2.5, color));
            // packets travel from the controller to the controlled screen
            let (from, to) = if toward_peer {
                (start, end)
            } else {
                (end, start)
            };
            let time = ui.ctx().input(|input| input.time) as f32;
            for index in 0..3 {
                let t = (time * 0.9 + index as f32 / 3.0).fract();
                let point = from + (to - from) * t;
                let fade = (t * (1.0 - t) * 4.0).clamp(0.0, 1.0);
                painter.circle_filled(point, 3.0, palette.on_accent.gamma_multiply(fade));
            }
        }
    }
    let end_color = match state {
        LinkState::Live { color, .. } => color,
        LinkState::Online => palette.border_strong,
        LinkState::Offline => palette.text_muted.gamma_multiply(0.6),
    };
    for point in [start, end] {
        painter.circle_filled(point, 3.0, palette.bg_canvas);
        painter.circle_stroke(point, 3.0, Stroke::new(1.5, end_color));
    }
}
