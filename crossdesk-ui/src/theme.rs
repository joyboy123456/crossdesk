//! Design tokens and shared painting helpers for both light and dark themes.
//!
//! The look: tinted "ink" surfaces instead of neutral greys, one signature
//! indigo -> cyan gradient used sparingly (brand, primary actions, the active
//! link), soft layered shadows for depth, and monospace for machine values
//! (ports, addresses, fingerprints).

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Frame, Mesh, Pos2, Rect, RichText, Sense,
    Shadow, Shape, Stroke, StrokeKind, Theme, Vec2,
};

pub const TITLE_SIZE: f32 = 26.0;
pub const HEADING_SIZE: f32 = 16.0;
pub const BODY_SIZE: f32 = 13.5;
pub const CAPTION_SIZE: f32 = 11.0;

pub const CARD_RADIUS: u8 = 14;
pub const CANVAS_RADIUS: u8 = 18;
pub const WIDGET_RADIUS: u8 = 10;

/// Minimum height of a row inside a [`group_frame`].
pub const ROW_HEIGHT: f32 = 40.0;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub bg_canvas: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_hot: Color32,
    pub accent_soft: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub glow: Color32,
    /// drop shadow under cards and dialogs
    pub shadow: Color32,
    /// 1px top-edge highlight that gives dark surfaces a lit bevel
    pub highlight: Color32,
    /// text and icons drawn on the accent gradient
    pub on_accent: Color32,
}

impl Palette {
    pub fn of(ui: &egui::Ui) -> Self {
        if ui.visuals().dark_mode {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: Color32::from_rgb(12, 13, 18),
            bg_canvas: Color32::from_rgb(8, 9, 13),
            surface: Color32::from_rgb(19, 21, 28),
            surface_raised: Color32::from_rgb(26, 28, 38),
            border: Color32::from_rgb(35, 38, 51),
            border_strong: Color32::from_rgb(54, 58, 76),
            text: Color32::from_rgb(236, 238, 245),
            text_secondary: Color32::from_rgb(161, 167, 186),
            text_muted: Color32::from_rgb(108, 115, 137),
            accent: Color32::from_rgb(129, 140, 248),
            accent_hot: Color32::from_rgb(34, 211, 238),
            accent_soft: Color32::from_rgb(28, 30, 56),
            success: Color32::from_rgb(52, 211, 153),
            warning: Color32::from_rgb(251, 191, 36),
            danger: Color32::from_rgb(248, 113, 113),
            glow: Color32::from_rgb(129, 140, 248),
            shadow: Color32::from_black_alpha(90),
            highlight: Color32::from_white_alpha(9),
            on_accent: Color32::WHITE,
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color32::from_rgb(245, 246, 250),
            bg_canvas: Color32::from_rgb(236, 238, 245),
            surface: Color32::WHITE,
            surface_raised: Color32::WHITE,
            border: Color32::from_rgb(225, 228, 238),
            border_strong: Color32::from_rgb(203, 208, 222),
            text: Color32::from_rgb(15, 18, 34),
            text_secondary: Color32::from_rgb(72, 79, 104),
            text_muted: Color32::from_rgb(120, 127, 150),
            accent: Color32::from_rgb(79, 70, 229),
            accent_hot: Color32::from_rgb(8, 145, 178),
            accent_soft: Color32::from_rgb(238, 240, 255),
            success: Color32::from_rgb(5, 150, 105),
            warning: Color32::from_rgb(180, 83, 9),
            danger: Color32::from_rgb(220, 38, 38),
            glow: Color32::from_rgb(79, 70, 229),
            shadow: Color32::from_rgba_premultiplied(22, 28, 60, 22),
            highlight: Color32::TRANSPARENT,
            on_accent: Color32::WHITE,
        }
    }
}

pub fn palette(ui: &egui::Ui) -> Palette {
    Palette::of(ui)
}

pub fn title_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into())
        .size(TITLE_SIZE)
        .strong()
        .extra_letter_spacing(-0.4)
}

pub fn heading_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(HEADING_SIZE).strong()
}

pub fn caption_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into())
        .size(CAPTION_SIZE)
        .extra_letter_spacing(1.4)
        .strong()
}

/// Monospace text for machine values: ports, addresses, fingerprints.
pub fn mono_text(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into()).font(FontId::monospace(size))
}

/// Small uppercase-style heading above a group of cards.
pub fn section_label(ui: &mut egui::Ui, text: impl Into<String>) {
    let palette = palette(ui);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(caption_text(text).color(palette.text_muted));
    });
    ui.add_space(2.0);
}

fn soft_shadow(palette: Palette) -> Shadow {
    Shadow {
        offset: [0, 2],
        blur: 14,
        spread: 0,
        color: palette.shadow,
    }
}

/// Rounded card frame used by list rows, forms and status panels.
pub fn card_frame(ui: &egui::Ui) -> Frame {
    let palette = palette(ui);
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(18)
        .shadow(soft_shadow(palette))
}

/// A card holding several [`group_row`]s separated by hairlines, in the
/// manner of a system settings pane.
pub fn group_frame(ui: &egui::Ui) -> Frame {
    card_frame(ui).inner_margin(egui::Margin::symmetric(16, 4))
}

/// Show a full-width card. Frames hug their content since egui 0.35; cards in
/// a column must line up instead.
pub fn full_card<R>(
    ui: &mut egui::Ui,
    frame: Frame,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let response = frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add_contents(ui)
    });
    paint_top_highlight(ui, response.response.rect);
    response
}

/// The lit top edge of a dark surface.
pub fn paint_top_highlight(ui: &egui::Ui, rect: Rect) {
    let palette = palette(ui);
    if palette.highlight == Color32::TRANSPARENT {
        return;
    }
    let inset = CARD_RADIUS as f32;
    ui.painter().hline(
        (rect.left() + inset)..=(rect.right() - inset),
        rect.top() + 1.0,
        Stroke::new(1.0, palette.highlight),
    );
}

/// One row of a [`group_frame`]: a title (and optional description) on the
/// left, `add_right` laid out right-to-left on the right, both vertically
/// centered on a row of at least [`ROW_HEIGHT`].
pub fn group_row(
    ui: &mut egui::Ui,
    title: &str,
    description: Option<&str>,
    add_right: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let palette = palette(ui);
    let height = if description.is_some() {
        ROW_HEIGHT + 12.0
    } else {
        ROW_HEIGHT
    };
    const LINE_GAP: f32 = 2.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());

    // A vertical stack cannot be centered by its parent layout before it
    // knows its own height, so measure the text and place it explicitly.
    let title_text = RichText::new(title).size(BODY_SIZE).color(palette.text);
    let description_text = description.map(|description| {
        RichText::new(description)
            .size(CAPTION_SIZE + 0.5)
            .color(palette.text_muted)
    });
    let line_height = |text: &RichText| {
        egui::WidgetText::from(text.clone())
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Body,
            )
            .size()
            .y
    };
    let block_height = line_height(&title_text)
        + description_text
            .as_ref()
            .map_or(0.0, |text| LINE_GAP + line_height(text));
    let left = Rect::from_min_size(
        Pos2::new(rect.left(), rect.center().y - block_height / 2.0),
        Vec2::new(rect.width(), block_height),
    );
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(left)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    text_ui.spacing_mut().item_spacing.y = LINE_GAP;
    let title_response = text_ui.label(title_text);
    if let Some(text) = description_text {
        text_ui.label(text);
    }

    let mut right_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    add_right(&mut right_ui);
    title_response
}

/// Hairline between two [`group_row`]s.
pub fn group_divider(ui: &mut egui::Ui) {
    let palette = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, palette.border),
    );
}

/// Small pill-shaped status badge: drawn dot plus text.
///
/// Painted rather than laid out, so it keeps its natural size in any parent
/// layout (a nested layout would stretch to the available width, and inherit
/// a right-to-left direction that puts the dot after the text).
pub fn status_badge(ui: &mut egui::Ui, color: Color32, text: &str) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::proportional(CAPTION_SIZE), color);
    let size = Vec2::new(galley.size().x + 26.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    ui.painter().rect(
        rect,
        CornerRadius::same(255),
        color.gamma_multiply(0.10),
        Stroke::new(1.0, color.gamma_multiply(0.24)),
        StrokeKind::Inside,
    );
    let dot = Pos2::new(rect.left() + 11.0, rect.center().y);
    ui.painter()
        .circle_filled(dot, 4.5, color.gamma_multiply(0.22));
    ui.painter().circle_filled(dot, 2.6, color);
    let text_height = galley.size().y;
    ui.painter().galley(
        Pos2::new(rect.left() + 18.0, rect.center().y - text_height / 2.0),
        galley,
        color,
    );
    response
}

/// Linear interpolation between two opaque colors.
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// Interpolation that stays correct for translucent (premultiplied) colors.
fn lerp_premultiplied(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// Drawn status dot replacing textual `●` markers.
pub fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 5.0, color.gamma_multiply(0.22));
    ui.painter().circle_filled(rect.center(), 2.8, color);
}

/// Dashed 1px outline used for empty screen slots on the layout canvas.
pub fn dashed_rect_stroke(painter: &egui::Painter, rect: Rect, stroke: Stroke) {
    let inset = stroke.width / 2.0;
    let rect = rect.shrink(inset + 1.0);
    let points = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    for shape in Shape::dashed_line(&points, stroke, 5.0, 5.0) {
        painter.add(shape);
    }
}

/// Soft accent glow around a rect: layered strokes fading outwards.
pub fn glow_stroke(painter: &egui::Painter, rect: Rect, radius: u8, color: Color32) {
    for layer in 1..=5 {
        let expand = layer as f32 * 2.0;
        let alpha = 0.26 / (layer as f32).powf(1.6);
        painter.rect_stroke(
            rect.expand(expand),
            CornerRadius::same(radius.saturating_add(layer as u8 * 2)),
            Stroke::new(2.0, color.gamma_multiply(alpha)),
            StrokeKind::Outside,
        );
    }
}

/// One stop of the signature gradient: `t = 0.0` is the indigo end,
/// `t = 1.0` the cyan end.
pub fn accent_gradient_stop(palette: Palette, t: f32) -> Color32 {
    lerp_color(palette.accent, palette.accent_hot, t)
}

/// Outline of a rounded rect: `(point, outward normal)` pairs, clockwise
/// from the top-left corner.
fn rounded_outline(rect: Rect, radius: f32) -> Vec<(Pos2, Vec2)> {
    const SEGMENTS: usize = 8;
    let r = radius
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0)
        .max(0.0);
    let corners = [
        (Pos2::new(rect.left() + r, rect.top() + r), PI),
        (Pos2::new(rect.right() - r, rect.top() + r), PI + FRAC_PI_2),
        (Pos2::new(rect.right() - r, rect.bottom() - r), 0.0),
        (Pos2::new(rect.left() + r, rect.bottom() - r), FRAC_PI_2),
    ];
    let mut outline = Vec::with_capacity(corners.len() * (SEGMENTS + 1));
    for (center, start) in corners {
        for step in 0..=SEGMENTS {
            let angle = start + FRAC_PI_2 * step as f32 / SEGMENTS as f32;
            let normal = Vec2::angled(angle);
            outline.push((center + normal * r, normal));
        }
    }
    outline
}

/// Fill a rounded rect with a linear gradient from `from` to `to` along
/// `direction` (non-negative components, e.g. `Vec2::X` or `Vec2::splat(1.)`),
/// with a 1px feathered edge so it anti-aliases like egui's own shapes.
pub fn gradient_rect(
    painter: &egui::Painter,
    rect: Rect,
    radius: f32,
    from: Color32,
    to: Color32,
    direction: Vec2,
) {
    painter.add(gradient_rect_shape(rect, radius, from, to, direction));
}

/// The shape behind [`gradient_rect`], for painting into a reserved slot.
pub fn gradient_rect_shape(
    rect: Rect,
    radius: f32,
    from: Color32,
    to: Color32,
    direction: Vec2,
) -> Shape {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Shape::Noop;
    }
    let span = rect.width() * direction.x.abs() + rect.height() * direction.y.abs();
    let color_at = |pos: Pos2| {
        let t = if span > 0.0 {
            ((pos - rect.min).dot(direction) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        lerp_premultiplied(from, to, t)
    };

    let outline = rounded_outline(rect, radius);
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.center(), color_at(rect.center()));
    for &(pos, normal) in &outline {
        let inner = pos - normal * 0.5;
        mesh.colored_vertex(inner, color_at(inner));
        mesh.colored_vertex(pos + normal * 0.5, Color32::TRANSPARENT);
    }
    let count = outline.len() as u32;
    for i in 0..count {
        let j = (i + 1) % count;
        let (inner_i, outer_i) = (1 + 2 * i, 2 + 2 * i);
        let (inner_j, outer_j) = (1 + 2 * j, 2 + 2 * j);
        mesh.add_triangle(0, inner_i, inner_j);
        mesh.add_triangle(inner_i, outer_i, outer_j);
        mesh.add_triangle(inner_i, outer_j, inner_j);
    }
    Shape::mesh(mesh)
}

/// A smooth radial falloff from `color` at `center` to transparent at
/// `radius`. Unlike stacked translucent circles it shows no banding rings.
pub fn radial_glow(painter: &egui::Painter, center: Pos2, radius: Vec2, color: Color32) {
    const SEGMENTS: u32 = 48;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    // an inner ring keeps the falloff from looking like a cone
    for step in 0..SEGMENTS {
        let angle = TAU * step as f32 / SEGMENTS as f32;
        let dir = Vec2::angled(angle);
        mesh.colored_vertex(
            center + Vec2::new(dir.x * radius.x, dir.y * radius.y) * 0.45,
            color.gamma_multiply(0.45),
        );
        mesh.colored_vertex(
            center + Vec2::new(dir.x * radius.x, dir.y * radius.y),
            Color32::TRANSPARENT,
        );
    }
    for i in 0..SEGMENTS {
        let j = (i + 1) % SEGMENTS;
        let (inner_i, outer_i) = (1 + 2 * i, 2 + 2 * i);
        let (inner_j, outer_j) = (1 + 2 * j, 2 + 2 * j);
        mesh.add_triangle(0, inner_i, inner_j);
        mesh.add_triangle(inner_i, outer_i, outer_j);
        mesh.add_triangle(inner_i, outer_j, inner_j);
    }
    painter.add(Shape::mesh(mesh));
}

/// Fine dot grid across a rect — the "engineering blueprint" texture that
/// keeps large empty areas from feeling flat.
pub fn dot_grid(painter: &egui::Painter, rect: Rect, color: Color32, spacing: f32) {
    let clip = painter.clip_rect().intersect(rect);
    let start_x = (clip.left() / spacing).ceil() * spacing;
    let start_y = (clip.top() / spacing).ceil() * spacing;
    let mut y = start_y;
    while y < clip.bottom() {
        let mut x = start_x;
        while x < clip.right() {
            painter.circle_filled(Pos2::new(x, y), 0.8, color);
            x += spacing;
        }
        y += spacing;
    }
}

pub fn configure_style(ctx: &egui::Context) {
    for (theme, palette) in [
        (Theme::Dark, Palette::dark()),
        (Theme::Light, Palette::light()),
    ] {
        let mut style = (*ctx.style_of(theme)).clone();
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(14.0, 7.0);
        style.spacing.interact_size.y = 30.0;
        style.animation_time = 0.14;
        // A desktop control panel, not a document: plain labels must not
        // grab clicks meant for the clickable rows and cards around them.
        style.interaction.selectable_labels = false;

        let visuals = &mut style.visuals;
        visuals.panel_fill = palette.bg;
        visuals.window_fill = palette.surface_raised;
        visuals.window_stroke = Stroke::new(1.0, palette.border);
        visuals.window_shadow = Shadow {
            offset: [0, 16],
            blur: 48,
            spread: 0,
            color: palette.shadow,
        };
        visuals.popup_shadow = soft_shadow(palette);
        visuals.extreme_bg_color = palette.bg_canvas;
        visuals.text_edit_bg_color = Some(palette.bg_canvas);
        visuals.selection.bg_fill = palette.accent.gamma_multiply(0.35);
        visuals.selection.stroke = Stroke::new(1.0, palette.accent);
        visuals.hyperlink_color = palette.accent;

        let radius = CornerRadius::same(WIDGET_RADIUS);
        let widgets = &mut visuals.widgets;
        for state in [
            &mut widgets.noninteractive,
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
        ] {
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text_secondary);
        widgets.inactive.weak_bg_fill = palette.surface_raised;
        widgets.inactive.bg_fill = palette.surface_raised;
        widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border_strong);
        widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
        widgets.hovered.weak_bg_fill = palette.accent_soft;
        widgets.hovered.bg_fill = palette.accent_soft;
        widgets.hovered.bg_stroke = Stroke::new(1.0, palette.accent.gamma_multiply(0.6));
        widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
        widgets.active.weak_bg_fill = palette.accent_soft;
        widgets.active.bg_fill = palette.accent_soft;
        widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
        widgets.active.fg_stroke = Stroke::new(1.0, palette.accent);

        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.floating = true;
        ctx.set_style_of(theme, style);
    }
}

/// Font id for a Lucide icon glyph at a given size.
pub fn icon_font(family: &str, size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(family.into()))
}
