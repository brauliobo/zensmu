mod app;
mod elevate;
mod views;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("zensmu — AMD Ryzen SMU Control & Telemetry")
            .with_inner_size([1380.0, 860.0])
            .with_min_inner_size([1000.0, 660.0]),
        ..Default::default()
    };

    eframe::run_native(
        "zensmu",
        options,
        Box::new(|cc| Ok(Box::new(app::ZensmuApp::new(cc)))),
    )
}
