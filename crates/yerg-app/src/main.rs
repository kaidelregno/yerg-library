#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod artcache;
mod crashlog;
mod theme;
mod jobs;
mod playback;
mod queue;
mod util;
mod views;

fn main() -> eframe::Result {
    crashlog::install();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,wgpu=warn,naga=warn"))
        .init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 780.0])
            .with_min_inner_size([820.0, 520.0])
            .with_title("Yerg Library")
            .with_app_id("dev.yerg.library"),
        // OpenGL (eframe's default). Measured on Windows/AMD: wgpu used 2.4x
        // (Direct3D 12) to 3.6x (Vulkan) the private memory and 4-9x the GPU
        // memory of glow when idle, so wgpu is not enabled.
        ..Default::default()
    };
    let result = eframe::run_native(
        "Yerg Library",
        options,
        Box::new(|cc| {
            let app = app::YergApp::new(cc)?;
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = &result {
        crashlog::record_error(&format!("could not run: {e}"));
    }
    crashlog::clean_exit();
    result
}
