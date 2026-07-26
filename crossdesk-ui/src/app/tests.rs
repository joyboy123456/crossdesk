//! UI tests driving the real widget tree through egui_kittest.

use super::*;
use crate::app::dialogs::PendingPosition;
use crate::app::geometry::{
    SLOT_DROP_TOLERANCE, position_occupied, screen_content_rect, screen_handle_rect,
    screen_layout_geometry, screen_slot_at, screen_status_rect,
};
use eframe::egui::{Pos2, Rect, Theme, Vec2};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};
use lan_mouse_ipc::Position;
use std::time::{Duration, Instant};

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
