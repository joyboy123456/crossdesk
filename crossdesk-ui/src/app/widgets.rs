//! Drawing helpers shared by the pages.
//!
//! Custom-painted controls report their role and label through
//! `widget_info`, so they stay reachable for screen readers and the tests
//! exactly like egui's built-in widgets.

use eframe::egui::{
    self, Align2, Button, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Vec2, WidgetInfo, WidgetType,
};
use iconflow::{Pack, Size, Style, try_icon};

use crate::app::Page;
use crate::app::geometry::{screen_content_rect, screen_handle_rect, screen_status_rect};
use crate::app::session::Role;
use crate::theme;

pub(crate) fn nav_item(
    ui: &mut egui::Ui,
    current: Page,
    page: Page,
    icon_name: &str,
    label: &str,
    badge: Option<(usize, bool)>,
) -> bool {
    let palette = theme::palette(ui);
    let selected = current == page;
    let t = ui
        .ctx()
        .animate_bool_with_time(Id::new(("nav", label)), selected, 0.18);
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
    response = response.on_hover_cursor(CursorIcon::PointingHand);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));

    let hover_t =
        ui.ctx()
            .animate_bool_with_time(Id::new(("nav-hover", label)), response.hovered(), 0.12);
    let radius = CornerRadius::same(theme::WIDGET_RADIUS);
    if hover_t > 0.01 && !selected {
        ui.painter()
            .rect_filled(rect, radius, palette.surface.gamma_multiply(hover_t));
    }
    if t > 0.01 {
        ui.painter()
            .rect_filled(rect, radius, palette.surface_raised.gamma_multiply(t));
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, palette.border.gamma_multiply(t)),
            StrokeKind::Inside,
        );
        // Gradient selection bar on the leading edge.
        let bar = Rect::from_center_size(
            Pos2::new(rect.left() + 1.5, rect.center().y),
            Vec2::new(3.0, 18.0 * t),
        );
        theme::gradient_rect(
            ui.painter(),
            bar,
            1.5,
            palette.accent,
            palette.accent_hot,
            Vec2::Y,
        );
    }

    let emphasized = t > 0.5 || response.hovered();
    let (glyph, family) = icon_glyph(icon_name);
    ui.painter().text(
        Pos2::new(rect.left() + 22.0, rect.center().y),
        Align2::CENTER_CENTER,
        glyph.to_string(),
        theme::icon_font(&family, 16.0),
        theme::lerp_color(palette.text_muted, palette.accent, t.max(hover_t * 0.5)),
    );
    ui.painter().text(
        Pos2::new(rect.left() + 42.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(theme::BODY_SIZE + 0.5),
        if emphasized {
            palette.text
        } else {
            palette.text_secondary
        },
    );

    if let Some((count, urgent)) = badge.filter(|(count, _)| *count > 0) {
        let text = if count > 99 {
            "99+".to_owned()
        } else {
            count.to_string()
        };
        let galley = ui.painter().layout_no_wrap(
            text,
            FontId::proportional(theme::CAPTION_SIZE),
            if urgent {
                palette.on_accent
            } else {
                palette.text_secondary
            },
        );
        let pill = Rect::from_center_size(
            Pos2::new(rect.right() - 12.0 - galley.size().x / 2.0, rect.center().y),
            Vec2::new((galley.size().x + 12.0).max(20.0), 18.0),
        );
        if urgent {
            theme::gradient_rect(
                ui.painter(),
                pill,
                9.0,
                palette.accent,
                palette.accent_hot,
                Vec2::X,
            );
        } else {
            ui.painter()
                .rect_filled(pill, CornerRadius::same(9), palette.border);
        }
        ui.painter().galley(
            pill.center() - galley.size() / 2.0,
            galley,
            palette.text_secondary,
        );
    }

    response.clicked()
}

/// A rounded square holding an icon, tinted with `color`; `solid` fills it
/// with the signature gradient instead.
pub(crate) fn icon_tile(
    ui: &mut egui::Ui,
    icon_name: &str,
    size: f32,
    color: Color32,
    solid: bool,
) {
    let palette = theme::palette(ui);
    let (tile, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let radius = (size * 0.3).round();
    if solid {
        // the brand color runs the full signature gradient; any other color
        // keeps its own hue and only leans towards cyan
        let end = if color == palette.accent {
            palette.accent_hot
        } else {
            theme::lerp_color(color, palette.accent_hot, 0.35)
        };
        theme::gradient_rect(ui.painter(), tile, radius, color, end, Vec2::splat(1.0));
    } else {
        ui.painter().rect(
            tile,
            CornerRadius::same(radius as u8),
            color.gamma_multiply(0.12),
            Stroke::new(1.0, color.gamma_multiply(0.26)),
            StrokeKind::Inside,
        );
    }
    let (glyph, family) = icon_glyph(icon_name);
    ui.painter().text(
        tile.center(),
        Align2::CENTER_CENTER,
        glyph.to_string(),
        theme::icon_font(&family, size * 0.46),
        if solid { palette.on_accent } else { color },
    );
}

/// The one prominent action of a view, filled with the signature gradient.
pub(crate) fn primary_button(
    ui: &mut egui::Ui,
    icon_name: Option<&str>,
    label: &str,
) -> egui::Response {
    button_with_fill(ui, icon_name, label, None)
}

/// Solid red button for destructive actions.
pub(crate) fn danger_button(
    ui: &mut egui::Ui,
    icon_name: Option<&str>,
    label: &str,
) -> egui::Response {
    let danger = theme::palette(ui).danger;
    button_with_fill(ui, icon_name, label, Some(danger))
}

fn button_with_fill(
    ui: &mut egui::Ui,
    icon_name: Option<&str>,
    label: &str,
    solid: Option<Color32>,
) -> egui::Response {
    let palette = theme::palette(ui);
    let text_galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(theme::BODY_SIZE),
        palette.on_accent,
    );
    let icon_galley = icon_name.map(|name| {
        let (glyph, family) = icon_glyph(name);
        ui.painter().layout_no_wrap(
            glyph.to_string(),
            theme::icon_font(&family, 14.0),
            palette.on_accent,
        )
    });
    let icon_width = icon_galley
        .as_ref()
        .map_or(0.0, |galley| galley.size().x + 6.0);
    let size = Vec2::new(text_galley.size().x + icon_width + 28.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    let response = response.on_hover_cursor(CursorIcon::PointingHand);

    let hover_t = ui.ctx().animate_bool_with_time(
        Id::new(("primary-hover", label)),
        response.hovered(),
        0.12,
    );
    let pressed = response.is_pointer_button_down_on();
    let radius = theme::WIDGET_RADIUS as f32;
    match solid {
        Some(color) => {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(theme::WIDGET_RADIUS),
                theme::lerp_color(color, Color32::WHITE, 0.08 * hover_t),
            );
        }
        None => {
            if hover_t > 0.01 {
                theme::glow_stroke(
                    ui.painter(),
                    rect,
                    theme::WIDGET_RADIUS,
                    palette.glow.gamma_multiply(0.7 * hover_t),
                );
            }
            theme::gradient_rect(
                ui.painter(),
                rect,
                radius,
                theme::lerp_color(palette.accent, Color32::WHITE, 0.08 * hover_t),
                palette.accent_hot,
                Vec2::new(1.0, 0.4),
            );
        }
    }
    if pressed {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(theme::WIDGET_RADIUS),
            Color32::from_black_alpha(30),
        );
    }

    let mut x = rect.center().x - (text_galley.size().x + icon_width) / 2.0;
    if let Some(galley) = icon_galley {
        let width = galley.size().x;
        ui.painter().galley(
            Pos2::new(x, rect.center().y - galley.size().y / 2.0),
            galley,
            palette.on_accent,
        );
        x += width + 6.0;
    }
    let text_height = text_galley.size().y;
    ui.painter().galley(
        Pos2::new(x, rect.center().y - text_height / 2.0),
        text_galley,
        palette.on_accent,
    );
    response
}

/// A quiet bordered button for secondary actions.
pub(crate) fn secondary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        Button::new(RichText::new(label).size(theme::BODY_SIZE))
            .min_size(Vec2::new(0.0, 32.0))
            .corner_radius(CornerRadius::same(theme::WIDGET_RADIUS)),
    )
}

/// The switch knob itself, no label.
fn paint_switch(ui: &egui::Ui, rect: Rect, on: bool, id: Id) {
    let palette = theme::palette(ui);
    let t = ui.ctx().animate_bool_with_time(id, on, 0.14);
    let track = Rect::from_center_size(rect.center(), Vec2::new(34.0, 20.0));
    if t > 0.01 {
        theme::gradient_rect(
            ui.painter(),
            track,
            10.0,
            palette.accent.gamma_multiply(t),
            palette.accent_hot.gamma_multiply(t),
            Vec2::X,
        );
    }
    if t < 0.99 {
        ui.painter().rect(
            track,
            CornerRadius::same(10),
            palette.border.gamma_multiply(1.0 - t),
            Stroke::new(1.0, palette.border_strong.gamma_multiply(1.0 - t)),
            StrokeKind::Inside,
        );
    }
    let knob_x = egui::lerp((track.left() + 10.0)..=(track.right() - 10.0), t);
    let knob = Pos2::new(knob_x, track.center().y);
    ui.painter().circle_filled(
        knob + Vec2::new(0.0, 1.0),
        7.5,
        Color32::from_black_alpha(40),
    );
    ui.painter().circle_filled(knob, 7.0, Color32::WHITE);
}

/// A compact labelled switch, e.g. the per-device "启用".
pub(crate) fn toggle_switch(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    let palette = theme::palette(ui);
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(theme::BODY_SIZE - 0.5),
        palette.text_secondary,
    );
    let size = Vec2::new(galley.size().x + 8.0 + 34.0, 28.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), *value, label));
    let text_height = galley.size().y;
    ui.painter().galley(
        Pos2::new(rect.left(), rect.center().y - text_height / 2.0),
        galley,
        palette.text_secondary,
    );
    let switch = Rect::from_min_max(Pos2::new(rect.right() - 34.0, rect.top()), rect.max);
    paint_switch(ui, switch, *value, response.id.with("knob"));
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A full-width settings row whose whole area toggles `value`.
pub(crate) fn toggle_row(
    ui: &mut egui::Ui,
    value: &mut bool,
    title: &str,
    description: &str,
) -> egui::Response {
    let palette = theme::palette(ui);
    let size = Vec2::new(ui.available_width(), theme::ROW_HEIGHT + 12.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), *value, title));
    ui.painter().text(
        Pos2::new(rect.left(), rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(theme::BODY_SIZE),
        palette.text,
    );
    ui.painter().text(
        Pos2::new(rect.left(), rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        description,
        FontId::proportional(theme::CAPTION_SIZE + 0.5),
        palette.text_muted,
    );
    let switch = Rect::from_center_size(
        Pos2::new(rect.right() - 17.0, rect.center().y),
        Vec2::new(34.0, 20.0),
    );
    paint_switch(ui, switch, *value, response.id.with("knob"));
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A selectable option card (radio semantics): icon, title, description.
/// `extra` renders next to the title, e.g. a badge.
pub(crate) fn option_card(
    ui: &mut egui::Ui,
    selected: bool,
    icon_name: &str,
    title: &str,
    description: &str,
    extra: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let palette = theme::palette(ui);
    let size = Vec2::new(ui.available_width(), 58.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::RadioButton, ui.is_enabled(), selected, title)
    });
    let t = ui
        .ctx()
        .animate_bool_with_time(response.id.with("selected"), selected, 0.15);
    let hover_t =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
    let radius = CornerRadius::same(theme::WIDGET_RADIUS + 2);
    ui.painter().rect(
        rect,
        radius,
        theme::lerp_color(
            theme::lerp_color(palette.surface, palette.surface_raised, hover_t),
            palette.accent_soft,
            t,
        ),
        Stroke::new(
            1.0 + t * 0.5,
            theme::lerp_color(palette.border, palette.accent, t),
        ),
        StrokeKind::Inside,
    );

    let icon_center = Pos2::new(rect.left() + 28.0, rect.center().y);
    let tile = Rect::from_center_size(icon_center, Vec2::splat(32.0));
    let accent = theme::lerp_color(palette.text_muted, palette.accent, t);
    ui.painter()
        .rect_filled(tile, CornerRadius::same(9), accent.gamma_multiply(0.12));
    let (glyph, family) = icon_glyph(icon_name);
    ui.painter().text(
        icon_center,
        Align2::CENTER_CENTER,
        glyph.to_string(),
        theme::icon_font(&family, 15.0),
        accent,
    );

    let title_galley = ui.painter().layout_no_wrap(
        title.to_owned(),
        FontId::proportional(theme::BODY_SIZE),
        palette.text,
    );
    let title_pos = Pos2::new(rect.left() + 54.0, rect.center().y - 9.0);
    let title_width = title_galley.size().x;
    ui.painter().galley(
        title_pos - Vec2::new(0.0, title_galley.size().y / 2.0),
        title_galley,
        palette.text,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 54.0, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        description,
        FontId::proportional(theme::CAPTION_SIZE + 0.5),
        palette.text_muted,
    );

    // radio mark
    let mark = Pos2::new(rect.right() - 22.0, rect.center().y);
    ui.painter().circle_stroke(
        mark,
        8.0,
        Stroke::new(
            1.5,
            theme::lerp_color(palette.border_strong, palette.accent, t),
        ),
    );
    if t > 0.01 {
        ui.painter().circle_filled(mark, 4.5 * t, palette.accent);
    }

    let extra_rect = Rect::from_min_size(
        Pos2::new(title_pos.x + title_width + 8.0, title_pos.y - 11.0),
        Vec2::new(120.0, 22.0),
    );
    // a detached child: `scope_builder` would advance the parent cursor to
    // this rect and let the next card overlap this one
    extra(
        &mut ui.new_child(
            egui::UiBuilder::new()
                .max_rect(extra_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        ),
    );

    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A two-state segmented control; each segment is its own button.
pub(crate) fn segmented_icons(
    ui: &mut egui::Ui,
    segments: [(&str, &str, bool); 2],
) -> Option<usize> {
    let palette = theme::palette(ui);
    let size = Vec2::new(ui.available_width(), 30.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(theme::WIDGET_RADIUS),
        palette.bg,
        Stroke::new(1.0, palette.border),
        StrokeKind::Inside,
    );
    let mut clicked = None;
    let half = rect.width() / 2.0;
    for (index, (icon_name, label, active)) in segments.into_iter().enumerate() {
        let segment = Rect::from_min_size(
            rect.min + Vec2::new(half * index as f32, 0.0),
            Vec2::new(half, rect.height()),
        )
        .shrink(3.0);
        let response = ui
            .interact(segment, Id::new(("segment", label)), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(label);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
        let t = ui
            .ctx()
            .animate_bool_with_time(response.id.with("active"), active, 0.15);
        if t > 0.01 {
            ui.painter().rect(
                segment,
                CornerRadius::same(theme::WIDGET_RADIUS - 3),
                palette.surface_raised.gamma_multiply(t),
                Stroke::new(1.0, palette.border_strong.gamma_multiply(t)),
                StrokeKind::Inside,
            );
        }
        let (glyph, family) = icon_glyph(icon_name);
        ui.painter().text(
            segment.center(),
            Align2::CENTER_CENTER,
            glyph.to_string(),
            theme::icon_font(&family, 14.0),
            theme::lerp_color(
                if response.hovered() {
                    palette.text_secondary
                } else {
                    palette.text_muted
                },
                palette.text,
                t,
            ),
        );
        if response.clicked() {
            clicked = Some(index);
        }
    }
    clicked
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
    /// the screen's part in the live session, with the session color
    pub(crate) role: Option<(Role, Color32)>,
}

/// A screen on the layout canvas, drawn as a small monitor: a bezel, a
/// display with a gradient "wallpaper", and a camera notch.
pub(crate) fn paint_screen(
    painter: &egui::Painter,
    rect: Rect,
    screen: ScreenPresentation<'_>,
    ui: &egui::Ui,
) {
    let palette = theme::palette(ui);
    let radius = theme::CARD_RADIUS;

    // lift while dragged
    if screen.dragging {
        painter.rect_filled(
            rect.translate(Vec2::new(0.0, 8.0)).expand(2.0),
            CornerRadius::same(radius + 2),
            Color32::from_black_alpha(60),
        );
    }
    let time = ui.ctx().input(|input| input.time) as f32;
    let pulse = 0.5 + 0.5 * (time * 2.4).sin();
    match screen.role {
        // the driven screen breathes, so it is found at a glance
        Some((Role::Controlled, color)) => theme::glow_stroke(
            painter,
            rect.expand(1.0),
            radius,
            color.gamma_multiply(0.55 + 0.45 * pulse),
        ),
        Some((Role::Controller, color)) => {
            theme::glow_stroke(painter, rect, radius, color.gamma_multiply(0.45));
        }
        None if screen.selected || screen.dragging => {
            theme::glow_stroke(painter, rect, radius, palette.glow);
        }
        None => {}
    }

    // bezel
    painter.rect(
        rect,
        CornerRadius::same(radius),
        palette.surface_raised,
        Stroke::new(
            if screen.role.is_some() {
                2.0
            } else if screen.selected || screen.dragging {
                1.5
            } else {
                1.0
            },
            if let Some((_, color)) = screen.role {
                color
            } else if screen.dragging || screen.selected {
                palette.accent
            } else if screen.pending {
                palette.warning
            } else {
                palette.border_strong
            },
        ),
        StrokeKind::Inside,
    );

    // display
    let display = rect.shrink(5.0);
    let display_radius = (radius - 5) as f32;
    painter.rect_filled(display, CornerRadius::same(radius - 5), palette.bg_canvas);
    // the driven display lights up in the session color instead of its
    // usual wallpaper, so the two never blend into a muddy mix
    let (from, to) = if let Some((Role::Controlled, color)) = screen.role {
        (color.gamma_multiply(0.40), color.gamma_multiply(0.12))
    } else if screen.local {
        (
            palette.accent.gamma_multiply(0.42),
            palette.accent_hot.gamma_multiply(0.26),
        )
    } else if screen.online {
        (
            palette.accent_hot.gamma_multiply(0.20),
            palette.accent.gamma_multiply(0.12),
        )
    } else {
        (
            palette.text_muted.gamma_multiply(0.10),
            palette.text_muted.gamma_multiply(0.03),
        )
    };
    theme::gradient_rect(
        painter,
        display,
        display_radius,
        from,
        to,
        Vec2::new(1.0, 0.8),
    );
    // glassy sheen across the top half
    theme::gradient_rect(
        painter,
        Rect::from_min_max(display.min, Pos2::new(display.right(), display.center().y)),
        display_radius,
        Color32::from_white_alpha(10),
        Color32::TRANSPARENT,
        Vec2::Y,
    );
    // camera notch
    painter.circle_filled(
        Pos2::new(rect.center().x, rect.top() + 2.5),
        1.3,
        palette.border_strong,
    );

    if !screen.local {
        let handle_rect = screen_handle_rect(rect);
        if screen.drag_hovered && screen.drag_enabled {
            painter.rect_filled(
                handle_rect,
                CornerRadius::same(theme::WIDGET_RADIUS - 2),
                palette.accent.gamma_multiply(0.14),
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
            theme::icon_font(&family, 14.0),
            handle_color,
        );
    }

    let content_rect = if screen.local {
        rect.shrink2(Vec2::new(16.0, 8.0))
    } else {
        screen_content_rect(rect)
    };
    let name_color = if screen.online || screen.local {
        palette.text
    } else {
        palette.text_secondary
    };
    painter.text(
        Pos2::new(content_rect.center().x, rect.center().y - 7.0),
        Align2::CENTER_CENTER,
        screen.name,
        FontId::proportional(14.0),
        name_color,
    );
    let caption = if screen.local {
        format!("{} · 本机", screen.number)
    } else {
        screen.number.to_owned()
    };
    painter.text(
        Pos2::new(content_rect.center().x, rect.center().y + 13.0),
        Align2::CENTER_CENTER,
        caption,
        FontId::proportional(theme::CAPTION_SIZE),
        if let Some((_, color)) = screen.role {
            color
        } else if screen.local {
            palette.accent
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
            7.0,
            palette.warning.gamma_multiply(0.15 + 0.25 * pulse),
        );
        painter.circle_filled(
            dot_center,
            3.5,
            palette.warning.gamma_multiply(0.55 + 0.45 * pulse),
        );
    } else {
        let color = if screen.online {
            palette.success
        } else {
            palette.warning
        };
        painter.circle_filled(dot_center, 5.5, color.gamma_multiply(0.22));
        painter.circle_filled(dot_center, 3.0, color);
    }

    if let Some((role, color)) = screen.role {
        paint_role_tag(painter, rect, role, color, palette);
    }
}

/// "控制端" / "被控端" tab sitting on a screen's top edge.
fn paint_role_tag(
    painter: &egui::Painter,
    rect: Rect,
    role: Role,
    color: Color32,
    palette: theme::Palette,
) {
    let text = painter.layout_no_wrap(
        role.label().to_owned(),
        FontId::proportional(theme::CAPTION_SIZE),
        palette.on_accent,
    );
    let size = Vec2::new(text.size().x + 30.0, 20.0);
    let tag = Rect::from_min_size(
        Pos2::new(rect.left() + 12.0, rect.top() - size.y / 2.0),
        size,
    );
    painter.rect(
        tag,
        CornerRadius::same(255),
        color,
        Stroke::new(2.0, palette.bg_canvas),
        StrokeKind::Outside,
    );
    let (glyph, family) = icon_glyph(role.icon());
    painter.text(
        Pos2::new(tag.left() + 12.0, tag.center().y),
        Align2::CENTER_CENTER,
        glyph.to_string(),
        theme::icon_font(&family, 10.5),
        palette.on_accent,
    );
    let height = text.size().y;
    painter.galley(
        Pos2::new(tag.left() + 21.0, tag.center().y - height / 2.0),
        text,
        palette.on_accent,
    );
}

pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    name: &str,
    tooltip: &str,
    color: Color32,
) -> egui::Response {
    let palette = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), tooltip));
    let hover_t = ui.ctx().animate_bool_with_time(
        Id::new(("icon-btn-hover", name, rect.min.x as i32, rect.min.y as i32)),
        response.hovered(),
        0.1,
    );
    if hover_t > 0.01 {
        ui.painter().rect(
            rect,
            CornerRadius::same(theme::WIDGET_RADIUS - 2),
            palette.surface_raised.gamma_multiply(hover_t),
            Stroke::new(1.0, palette.border.gamma_multiply(hover_t)),
            StrokeKind::Inside,
        );
    }
    let (glyph, family) = icon_glyph(name);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph.to_string(),
        theme::icon_font(&family, 15.0),
        color,
    );
    response
        .on_hover_cursor(CursorIcon::PointingHand)
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

pub(crate) fn short_fingerprint(fingerprint: &str) -> String {
    if fingerprint.chars().count() > 24 {
        format!("{}…", fingerprint.chars().take(24).collect::<String>())
    } else {
        fingerprint.to_owned()
    }
}
