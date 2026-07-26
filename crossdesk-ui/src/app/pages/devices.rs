//! The device list and the drag-and-drop screen layout editor.

use std::time::Instant;

use eframe::egui::{
    self, Align, Align2, CornerRadius, CursorIcon, FontId, Frame, Id, Layout, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Vec2, WidgetInfo, WidgetType,
};
use lan_mouse_ipc::FrontendRequest;

use crate::app::CrossDeskApp;
use crate::app::dialogs::{Editor, PendingPosition};
use crate::app::geometry::{position_occupied, screen_layout_geometry, screen_slot_at};
use crate::app::widgets::{ScreenPresentation, icon, icon_button, paint_screen};
use crate::model::{DeviceDraft, UiClient, displayed_position, position_label};
use crate::theme;

impl CrossDeskApp {
    pub(crate) fn devices_page(&mut self, ui: &mut egui::Ui) {
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
}
