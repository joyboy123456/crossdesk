mod app;
mod bridge;
#[cfg(target_os = "macos")]
mod macos_privacy;
mod model;
mod scan;
mod theme;
mod tray;

use eframe::egui;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UiError {
    #[error("CrossDesk frontend failed: {0}")]
    Run(String),
}

pub fn run(local_commit: [u8; 8], owns_service: bool) -> Result<(), UiError> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CrossDesk")
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([760.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "CrossDesk",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::CrossDeskApp::new(
                cc,
                local_commit,
                owns_service,
            )))
        }),
    )
    .map_err(|error| UiError::Run(error.to_string()))
}
