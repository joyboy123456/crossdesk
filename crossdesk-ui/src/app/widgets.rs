//! Drawing helpers shared by the pages.

use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, CursorIcon, FontId, Id, Layout, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Vec2, WidgetInfo, WidgetType,
};
use iconflow::{Pack, Size, Style, try_icon};

use crate::app::Page;
use crate::app::geometry::{screen_content_rect, screen_handle_rect, screen_status_rect};
use crate::theme;

pub(crate) fn nav_item(
    ui: &mut egui::Ui,
    current: Page,
    page: Page,
    icon_name: &str,
    label: &str,
) -> bool {
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

pub(crate) struct ScreenPresentation<'a> {
    pub(crate) number: &'a str,
    pub(crate) name: &'a str,
    pub(crate) online: bool,
    pub(crate) local: bool,
    pub(crate) selected: bool,
    pub(crate) pending: bool,
    pub(crate) drag_enabled: bool,
    pub(crate) drag_hovered: bool,
    pub(crate) dragging: bool,
}

pub(crate) fn paint_screen(
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

pub(crate) fn settings_row(ui: &mut egui::Ui, label: &str, value: &str, color: Option<Color32>) {
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

pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    name: &str,
    tooltip: &str,
    color: Color32,
) -> egui::Response {
    ui.add(Button::new(icon(name, 16.0, color)).frame(false))
        .on_hover_text(tooltip)
}

pub(crate) fn icon_glyph(name: &str) -> (char, String) {
    let icon =
        try_icon(Pack::Lucide, name, Style::Regular, Size::Regular).expect("required Lucide icon");
    (
        char::from_u32(icon.codepoint).unwrap_or('?'),
        icon.family.to_owned(),
    )
}

pub(crate) fn icon(name: &str, size: f32, color: Color32) -> RichText {
    let (glyph, family) = icon_glyph(name);
    RichText::new(glyph.to_string())
        .font(theme::icon_font(&family, size))
        .color(color)
}

pub(crate) fn short_fingerprint(fingerprint: &str) -> String {
    if fingerprint.chars().count() > 24 {
        format!("{}…", fingerprint.chars().take(24).collect::<String>())
    } else {
        fingerprint.to_owned()
    }
}
