#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    Open,
    Quit,
}

#[cfg(any(windows, target_os = "macos"))]
pub struct TrayController {
    _tray: tray_icon::TrayIcon,
    open_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
}

#[cfg(any(windows, target_os = "macos"))]
impl TrayController {
    pub fn new() -> Result<Self, tray_icon::Error> {
        use tray_icon::{
            Icon, TrayIconBuilder,
            menu::{Menu, MenuItem, PredefinedMenuItem},
        };

        let menu = Menu::new();
        let open = MenuItem::new("打开 CrossDesk", true, None);
        let quit = MenuItem::new("退出 CrossDesk", true, None);
        menu.append(&open).expect("append tray open item");
        menu.append(&PredefinedMenuItem::separator())
            .expect("append tray separator");
        menu.append(&quit).expect("append tray quit item");

        let icon = Icon::from_rgba(tray_pixels(), 32, 32).expect("valid tray icon pixels");
        let tray = TrayIconBuilder::new()
            .with_tooltip("CrossDesk")
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .build()?;

        Ok(Self {
            _tray: tray,
            open_id: open.id().clone(),
            quit_id: quit.id().clone(),
        })
    }

    pub fn poll(&self) -> Option<TrayAction> {
        let event = tray_icon::menu::MenuEvent::receiver().try_recv().ok()?;
        if event.id == self.open_id {
            Some(TrayAction::Open)
        } else if event.id == self.quit_id {
            Some(TrayAction::Quit)
        } else {
            None
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub struct TrayController;

#[cfg(not(any(windows, target_os = "macos")))]
impl TrayController {
    pub fn new() -> Result<Self, std::convert::Infallible> {
        Ok(Self)
    }

    pub fn poll(&self) -> Option<TrayAction> {
        None
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn tray_pixels() -> Vec<u8> {
    let mut pixels = vec![0; 32 * 32 * 4];
    paint_rect(&mut pixels, 3, 5, 19, 14, [37, 99, 235, 255]);
    paint_rect(&mut pixels, 10, 13, 19, 14, [15, 23, 42, 255]);
    paint_rect(&mut pixels, 12, 15, 15, 10, [248, 250, 252, 255]);
    pixels
}

#[cfg(any(windows, target_os = "macos"))]
fn paint_rect(pixels: &mut [u8], x: usize, y: usize, width: usize, height: usize, color: [u8; 4]) {
    for row in y..(y + height).min(32) {
        for column in x..(x + width).min(32) {
            let offset = (row * 32 + column) * 4;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
}
