use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "camera-app",
    version,
    about = "webcam viewer with lens effects"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// open the viewer window (the default)
    Gui,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Gui) {
        Command::Gui => run_gui(),
    }
}

fn run_gui() -> Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("camera-app")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "camera-app",
        options,
        Box::new(|cc| Ok(Box::new(camera_app::ui::App::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
