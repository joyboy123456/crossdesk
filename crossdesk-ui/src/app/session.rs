//! Who is driving whom: the session board on top of the pages, the live
//! card in the sidebar, and the tray/title mirror of the same state.
//!
//! Every surface reads [`Session`] so they can never disagree. Direction is
//! always drawn left to right, controller first: "A 控制 B".

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Shape,
    Stroke, StrokeKind, Vec2,
};
use lan_mouse_ipc::{ClientHandle, ControlMode, FrontendRequest};

use crate::app::CrossDeskApp;
use crate::app::widgets::{danger_button, icon_glyph, icon_tile};
use crate::model::Session;
use crate::theme::{self, Palette};

/// A machine's part in the current session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// its mouse and keyboard are driving the other machine
    Controller,
    /// it is being driven
    Controlled,
}

impl Role {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Controller => "控制端",
            Self::Controlled => "被控端",
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Controller => "mouse-pointer-2",
            Self::Controlled => "monitor",
        }
    }
}

/// The signal color of a session: indigo while this machine drives another,
/// green while it is driven, amber while switching.
pub(crate) fn session_color(palette: Palette, session: &Session) -> Color32 {
    match session {
        Session::Outgoing { .. } => palette.accent,
        Session::Incoming { .. } => palette.success,
        Session::Switching => palette.warning,
        Session::Idle => palette.text_muted,
    }
}

/// This machine's role in `session`.
pub(crate) fn local_role(session: &Session) -> Option<Role> {
    match session {
        Session::Outgoing { .. } => Some(Role::Controller),
        Session::Incoming { .. } => Some(Role::Controlled),
        Session::Idle | Session::Switching => None,
    }
}

/// The role of the configured device `handle` in `session`.
pub(crate) fn peer_role(session: &Session, handle: ClientHandle) -> Option<Role> {
    match session {
        Session::Outgoing { handle: target, .. } if *target == handle => Some(Role::Controlled),
        Session::Incoming {
            handle: Some(source),
            ..
        } if *source == handle => Some(Role::Controller),
        _ => None,
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Repaint cadence while a session animates: smooth enough for the flowing
/// signal, cheap enough not to compete with the input pipeline for CPU.
const LIVE_FRAME: Duration = Duration::from_millis(33);

/// Shorten `text` with an ellipsis until it fits `max_width`.
fn fit_text(painter: &egui::Painter, text: &str, font: FontId, max_width: f32) -> String {
    let fits = |candidate: &str| {
        painter
            .layout_no_wrap(candidate.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
            <= max_width
    };
    if fits(text) {
        return text.to_owned();
    }
    let mut chars = text.chars().collect::<Vec<_>>();
    while !chars.is_empty() {
        chars.pop();
        let candidate = format!("{}…", chars.iter().collect::<String>());
        if fits(&candidate) {
            return candidate;
        }
    }
    "…".to_owned()
}

impl CrossDeskApp {
    /// Follow session changes: restart the clock and mirror the state into
    /// the tray tooltip and the window title, which stay visible while the
    /// window is hidden or behind other windows.
    pub(crate) fn track_session(&mut self, ctx: &egui::Context) {
        let session = self.state.session();
        if self
            .session_since
            .as_ref()
            .is_some_and(|(current, _)| *current == session)
        {
            return;
        }
        let summary = session.summary();
        let title = summary.as_deref().map_or_else(
            || "CrossDesk".to_owned(),
            |text| format!("CrossDesk · {text}"),
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
        if let Some(tray) = &self.tray {
            tray.set_status(&title);
        }
        self.session_since = Some((session, Instant::now()));
    }

    fn session_elapsed(&self) -> Duration {
        self.session_since
            .as_ref()
            .map_or(Duration::ZERO, |(_, since)| since.elapsed())
    }

    /// Whether the session board is worth its space on the current page:
    /// always on the device view, elsewhere only while a session is live
    /// and "立即断开" must stay one click away.
    pub(crate) fn session_board_visible(&self) -> bool {
        self.page == crate::app::Page::Devices || self.state.session() != Session::Idle
    }

    pub(crate) fn session_board(&mut self, ui: &mut egui::Ui) {
        let session = self.state.session();
        if session.is_live() {
            self.live_session_board(ui, &session);
        } else {
            self.idle_session_board(ui, &session);
        }
    }

    fn idle_session_board(&mut self, ui: &mut egui::Ui, session: &Session) {
        let palette = theme::palette(ui);
        let (title, detail, color, icon_name) = if *session == Session::Switching {
            (
                "正在切换",
                "正在安全释放当前控制会话",
                palette.warning,
                "loader",
            )
        } else {
            match self.state.control_mode {
                ControlMode::ReceiveOnly => (
                    "等待被控制",
                    "本机仅接受已授权设备的控制",
                    palette.success,
                    "inbox",
                ),
                ControlMode::SendOnly => (
                    "已启用，等待鼠标滑入",
                    "移动鼠标到设备边缘开始控制",
                    palette.accent,
                    "mouse-pointer-2",
                ),
                ControlMode::Bidirectional => (
                    "已启用，等待鼠标滑入",
                    "双向自动已启用，同一时刻角色互斥",
                    palette.accent,
                    "arrow-left-right",
                ),
            }
        };
        let mode_label = match self.state.control_mode {
            ControlMode::ReceiveOnly => "仅被控制",
            ControlMode::SendOnly => "仅控制",
            ControlMode::Bidirectional => "双向自动",
        };
        theme::full_card(
            ui,
            theme::card_frame(ui).inner_margin(egui::Margin::symmetric(14, 12)),
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    icon_tile(ui, icon_name, 38.0, color, false);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(title).size(14.5).strong().color(palette.text));
                        ui.label(
                            RichText::new(detail)
                                .size(theme::CAPTION_SIZE + 1.0)
                                .color(palette.text_muted),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        theme::status_badge(ui, palette.text_muted, "空闲")
                            .on_hover_text("每台电脑都在使用自己的键鼠");
                        theme::status_badge(ui, palette.text_muted, mode_label)
                            .on_hover_text("在「设置 · 控制方式」中修改");
                    });
                });
            },
        );
    }

    fn live_session_board(&mut self, ui: &mut egui::Ui, session: &Session) {
        let palette = theme::palette(ui);
        let color = session_color(palette, session);
        let (headline, detail, status) = match session {
            Session::Outgoing { peer, .. } => (
                format!("正在控制 {peer}"),
                format!("本机的鼠标和键盘正在操作 {peer}"),
                "控制中",
            ),
            Session::Incoming {
                peer, addr, known, ..
            } => (
                format!("正在被 {peer} 控制"),
                if *known {
                    format!("连接地址 {addr}")
                } else {
                    "远端键鼠正在控制本机".to_owned()
                },
                "被控中",
            ),
            Session::Idle | Session::Switching => return,
        };
        let (controller, controlled) = match session {
            Session::Outgoing { peer, .. } => {
                ((self.local_hostname.clone(), true), (peer.clone(), false))
            }
            Session::Incoming { peer, .. } => {
                ((peer.clone(), false), (self.local_hostname.clone(), true))
            }
            Session::Idle | Session::Switching => return,
        };
        let elapsed = format_elapsed(self.session_elapsed());
        let time = ui.ctx().input(|input| input.time) as f32;
        let pulse = 0.5 + 0.5 * (time * 2.4).sin();

        let mut disconnect = false;
        let frame = theme::card_frame(ui)
            .inner_margin(egui::Margin::symmetric(16, 14))
            .stroke(Stroke::new(1.5, color.gamma_multiply(0.75)));
        let response = theme::full_card(ui, frame, |ui| {
            // a wash of the session color behind everything in the card
            let wash = ui.painter().add(Shape::Noop);

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                live_pill(ui, status, color, pulse);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(&headline)
                            .size(16.0)
                            .strong()
                            .color(palette.text),
                    );
                    ui.label(
                        RichText::new(&detail)
                            .size(theme::CAPTION_SIZE + 1.0)
                            .color(palette.text_secondary),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    if danger_button(ui, Some("unplug"), "立即断开").clicked() {
                        disconnect = true;
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(
                            RichText::new("已持续")
                                .size(theme::CAPTION_SIZE)
                                .color(palette.text_muted),
                        );
                        ui.label(theme::mono_text(&elapsed, 15.0).color(color));
                    });
                });
            });
            ui.add_space(12.0);

            let width = ui.available_width();
            let (flow, _) = ui.allocate_exact_size(Vec2::new(width, 66.0), Sense::hover());
            paint_flow(ui, flow, &controller, &controlled, color, time, pulse);

            let card = ui.min_rect().expand2(Vec2::new(16.0, 14.0));
            ui.painter().set(
                wash,
                theme::gradient_rect_shape(
                    card,
                    theme::CARD_RADIUS as f32,
                    color.gamma_multiply(0.16),
                    color.gamma_multiply(0.02),
                    Vec2::new(1.0, 0.6),
                ),
            );
        });
        theme::glow_stroke(
            ui.painter(),
            response.response.rect,
            theme::CARD_RADIUS,
            color.gamma_multiply(0.4 + 0.4 * pulse),
        );
        ui.ctx().request_repaint_after(LIVE_FRAME);

        if disconnect {
            self.send(FrontendRequest::DisconnectControl);
        }
    }

    /// The always-visible session card in the sidebar, above the footer.
    pub(crate) fn sidebar_session_card(&self, ui: &mut egui::Ui) {
        let session = self.state.session();
        let palette = theme::palette(ui);
        let color = session_color(palette, &session);
        let (status, line) = match &session {
            Session::Outgoing { peer, .. } => ("控制中", format!("→ {peer}")),
            Session::Incoming { peer, .. } => ("被控中", format!("← {peer}")),
            Session::Switching => ("正在切换", "释放当前会话".to_owned()),
            Session::Idle => return,
        };
        let size = Vec2::new(ui.available_width(), 58.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        let summary = session.summary().unwrap_or_default();
        response.on_hover_text(summary);

        let time = ui.ctx().input(|input| input.time) as f32;
        let pulse = 0.5 + 0.5 * (time * 2.4).sin();
        let painter = ui.painter();
        theme::glow_stroke(
            painter,
            rect,
            theme::CARD_RADIUS,
            color.gamma_multiply(0.3 + 0.4 * pulse),
        );
        painter.rect(
            rect,
            CornerRadius::same(theme::CARD_RADIUS),
            theme::lerp_color(palette.surface, color, 0.12),
            Stroke::new(1.0, color.gamma_multiply(0.7)),
            StrokeKind::Inside,
        );
        let dot = Pos2::new(rect.left() + 18.0, rect.top() + 19.0);
        painter.circle_filled(dot, 6.0 + 2.0 * pulse, color.gamma_multiply(0.25));
        painter.circle_filled(dot, 4.0, color);
        painter.text(
            Pos2::new(rect.left() + 32.0, dot.y),
            Align2::LEFT_CENTER,
            status,
            FontId::proportional(theme::BODY_SIZE),
            color,
        );
        if session.is_live() {
            painter.text(
                Pos2::new(rect.right() - 12.0, dot.y),
                Align2::RIGHT_CENTER,
                format_elapsed(self.session_elapsed()),
                FontId::monospace(theme::CAPTION_SIZE),
                palette.text_secondary,
            );
        }
        let font = FontId::proportional(theme::CAPTION_SIZE + 1.0);
        painter.text(
            Pos2::new(rect.left() + 14.0, rect.bottom() - 16.0),
            Align2::LEFT_CENTER,
            fit_text(painter, &line, font.clone(), rect.width() - 28.0),
            font,
            palette.text,
        );
        ui.ctx().request_repaint_after(LIVE_FRAME);
    }
}

/// Solid pill with a breathing dot: "● 控制中".
fn live_pill(ui: &mut egui::Ui, text: &str, color: Color32, pulse: f32) {
    let palette = theme::palette(ui);
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::proportional(theme::BODY_SIZE - 0.5),
        palette.on_accent,
    );
    let size = Vec2::new(galley.size().x + 36.0, 30.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(255), color);
    let dot = Pos2::new(rect.left() + 14.0, rect.center().y);
    ui.painter().circle_filled(
        dot,
        4.0 + 2.5 * pulse,
        palette.on_accent.gamma_multiply(0.3),
    );
    ui.painter().circle_filled(dot, 3.5, palette.on_accent);
    let height = galley.size().y;
    ui.painter().galley(
        Pos2::new(rect.left() + 25.0, rect.center().y - height / 2.0),
        galley,
        palette.on_accent,
    );
}

/// Controller node, the input signal, controlled node.
fn paint_flow(
    ui: &egui::Ui,
    rect: Rect,
    controller: &(String, bool),
    controlled: &(String, bool),
    color: Color32,
    time: f32,
    pulse: f32,
) {
    let palette = theme::palette(ui);
    let painter = ui.painter();
    let node_width = ((rect.width() - 170.0) / 2.0).clamp(150.0, 280.0);
    let left = Rect::from_min_size(rect.min, Vec2::new(node_width, rect.height()));
    let right = Rect::from_min_size(
        Pos2::new(rect.right() - node_width, rect.top()),
        Vec2::new(node_width, rect.height()),
    );
    paint_node(ui, left, controller, Role::Controller, color, pulse);
    paint_node(ui, right, controlled, Role::Controlled, color, pulse);

    // the signal: halo, core, travelling packets and an arrow head
    let start = left.right_center() + Vec2::new(10.0, 0.0);
    let end = right.left_center() - Vec2::new(14.0, 0.0);
    painter.line_segment([start, end], Stroke::new(8.0, color.gamma_multiply(0.14)));
    painter.line_segment([start, end], Stroke::new(2.5, color.gamma_multiply(0.85)));
    for index in 0..4 {
        let t = (time * 0.8 + index as f32 / 4.0).fract();
        let point = start + (end - start) * t;
        let fade = (t * (1.0 - t) * 4.0).clamp(0.0, 1.0);
        painter.circle_filled(point, 5.0, color.gamma_multiply(0.25 * fade));
        painter.circle_filled(point, 2.8, palette.on_accent.gamma_multiply(fade));
    }
    painter.add(Shape::convex_polygon(
        vec![
            end + Vec2::new(10.0, 0.0),
            end + Vec2::new(-2.0, -7.0),
            end + Vec2::new(-2.0, 7.0),
        ],
        color,
        Stroke::NONE,
    ));

    // what travels, above the line; how, below it
    let mid = Pos2::new((start.x + end.x) / 2.0, start.y);
    let (mouse, mouse_family) = icon_glyph("mouse");
    let (keyboard, keyboard_family) = icon_glyph("keyboard");
    let label = painter.layout_no_wrap(
        "键鼠".to_owned(),
        FontId::proportional(theme::CAPTION_SIZE),
        palette.text_secondary,
    );
    let label_width = label.size().x;
    let total = 14.0 + 4.0 + 14.0 + 6.0 + label_width;
    let mut x = mid.x - total / 2.0;
    let y = mid.y - 16.0;
    painter.text(
        Pos2::new(x + 7.0, y),
        Align2::CENTER_CENTER,
        mouse,
        theme::icon_font(&mouse_family, 13.0),
        color,
    );
    x += 18.0;
    painter.text(
        Pos2::new(x + 7.0, y),
        Align2::CENTER_CENTER,
        keyboard,
        theme::icon_font(&keyboard_family, 13.0),
        color,
    );
    x += 20.0;
    let label_height = label.size().y;
    painter.galley(
        Pos2::new(x, y - label_height / 2.0),
        label,
        palette.text_secondary,
    );
    painter.text(
        Pos2::new(mid.x, mid.y + 16.0),
        Align2::CENTER_CENTER,
        "局域网直连 · 加密",
        FontId::proportional(theme::CAPTION_SIZE - 0.5),
        palette.text_muted,
    );
}

/// One end of the flow: a device chip naming the machine and its role.
fn paint_node(
    ui: &egui::Ui,
    rect: Rect,
    (name, local): &(String, bool),
    role: Role,
    color: Color32,
    pulse: f32,
) {
    let palette = theme::palette(ui);
    let painter = ui.painter();
    let controlled = role == Role::Controlled;
    if controlled {
        theme::glow_stroke(painter, rect, 12, color.gamma_multiply(0.45 + 0.45 * pulse));
    }
    painter.rect(
        rect,
        CornerRadius::same(12),
        if controlled {
            theme::lerp_color(palette.surface_raised, color, 0.10)
        } else {
            palette.surface_raised
        },
        Stroke::new(
            if controlled { 2.0 } else { 1.0 },
            color.gamma_multiply(if controlled { 1.0 } else { 0.5 }),
        ),
        StrokeKind::Inside,
    );

    let tile = Rect::from_center_size(
        Pos2::new(rect.left() + 32.0, rect.center().y),
        Vec2::splat(38.0),
    );
    theme::gradient_rect(
        painter,
        tile,
        11.0,
        color,
        theme::lerp_color(color, palette.accent_hot, 0.5),
        Vec2::splat(1.0),
    );
    let (glyph, family) = icon_glyph(if *local { "laptop" } else { "monitor" });
    painter.text(
        tile.center(),
        Align2::CENTER_CENTER,
        glyph.to_string(),
        theme::icon_font(&family, 17.0),
        palette.on_accent,
    );

    let text_left = tile.right() + 12.0;
    let name_font = FontId::proportional(14.5);
    painter.text(
        Pos2::new(text_left, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        fit_text(
            painter,
            name,
            name_font.clone(),
            rect.right() - text_left - 10.0,
        ),
        name_font,
        palette.text,
    );
    let (role_glyph, role_family) = icon_glyph(role.icon());
    painter.text(
        Pos2::new(text_left + 6.0, rect.center().y + 12.0),
        Align2::CENTER_CENTER,
        role_glyph.to_string(),
        theme::icon_font(&role_family, 11.0),
        color,
    );
    painter.text(
        Pos2::new(text_left + 16.0, rect.center().y + 12.0),
        Align2::LEFT_CENTER,
        format!(
            "{} · {}",
            role.label(),
            if *local { "本机" } else { "对端" }
        ),
        FontId::proportional(theme::CAPTION_SIZE),
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_reads_like_a_clock() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "00:05");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "02:05");
        assert_eq!(format_elapsed(Duration::from_secs(3725)), "1:02:05");
    }

    #[test]
    fn roles_follow_the_direction_of_input() {
        let outgoing = Session::Outgoing {
            handle: 3,
            peer: "Mac".into(),
        };
        assert_eq!(local_role(&outgoing), Some(Role::Controller));
        assert_eq!(peer_role(&outgoing, 3), Some(Role::Controlled));
        assert_eq!(peer_role(&outgoing, 4), None);

        let incoming = Session::Incoming {
            handle: Some(4),
            peer: "PC".into(),
            addr: "10.0.0.2:4242".parse().expect("valid address"),
            known: true,
        };
        assert_eq!(local_role(&incoming), Some(Role::Controlled));
        assert_eq!(peer_role(&incoming, 4), Some(Role::Controller));
        assert_eq!(local_role(&Session::Idle), None);
    }
}
