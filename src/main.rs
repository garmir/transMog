use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use transmog::capture;
use transmog::controls::preset;
use transmog::controls::Controls;

#[derive(Parser)]
#[command(name = "transmog", version, about = "webcam viewer with lens effects")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// open the viewer window (the default)
    Gui {
        /// write every captured buffer to a ring of slot files in DIR
        /// ($XDG_RUNTIME_DIR/transmog/tap when no DIR is given)
        #[arg(long, num_args = 0..=1, value_name = "DIR")]
        tap: Option<Option<PathBuf>>,
        /// apply this preset once the camera is open
        #[arg(long, value_name = "NAME")]
        preset: Option<String>,
        /// start in aim mode
        #[arg(long)]
        aim: bool,
        /// where aim mode's "save calibration" writes
        /// ($XDG_CONFIG_HOME/transmog/calibration.json by default)
        #[arg(long, value_name = "FILE")]
        calibration: Option<PathBuf>,
    },
    /// list, save or apply camera presets without the window
    Preset {
        #[command(subcommand)]
        action: PresetAction,
    },
}

#[derive(Subcommand)]
enum PresetAction {
    /// the saved presets and the directory they live in
    List,
    /// save the camera's writable controls under NAME
    Save {
        name: String,
        #[arg(long)]
        device: Option<PathBuf>,
    },
    /// write a preset's controls and print what the camera reads back;
    /// the format line needs the window, which owns the stream
    Apply {
        name: String,
        #[arg(long)]
        device: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Gui {
        tap: None,
        preset: None,
        aim: false,
        calibration: None,
    }) {
        Command::Gui {
            tap,
            preset,
            aim,
            calibration,
        } => run_gui(
            tap.map(|dir| dir.unwrap_or_else(capture::tap::default_dir)),
            preset,
            aim,
            calibration,
        ),
        Command::Preset { action } => run_preset(action),
    }
}

fn run_gui(
    tap: Option<PathBuf>,
    preset: Option<String>,
    aim: bool,
    calibration: Option<PathBuf>,
) -> Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_app_id("transmog")
            .with_title("transMog")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "transMog",
        options,
        Box::new(move |cc| {
            let mut app = transmog::ui::App::new(cc);
            if tap.is_some() {
                app.set_tap(tap);
            }
            if let Some(name) = preset {
                app.apply_preset(&name);
            }
            if let Some(path) = calibration {
                app.set_calibration_path(path);
            }
            app.set_aim(aim);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

/// the named device, else the first c920, else the first capture device
fn pick_device(device: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = device {
        return Ok(path);
    }
    let devices = capture::list_devices();
    devices
        .iter()
        .find(|d| d.name.contains("C920"))
        .or(devices.first())
        .map(|d| d.path.clone())
        .context("no capture devices found")
}

fn run_preset(action: PresetAction) -> Result<()> {
    match action {
        PresetAction::List => {
            println!("# {}", preset::dir().display());
            for name in preset::list() {
                println!("{name}");
            }
        }
        PresetAction::Save { name, device } => {
            let controls = Controls::open(&pick_device(device)?)?;
            let saved = preset::Preset::from_current(None, &controls.query()?);
            println!("{}", preset::save(&name, &saved)?.display());
        }
        PresetAction::Apply { name, device } => {
            let path = pick_device(device)?;
            let loaded = preset::load(&name)?;
            if let Some(format) = &loaded.format {
                println!("# format {format} is applied by the window only");
            }
            let mut off = 0;
            for r in loaded.apply(&Controls::open(&path)?)? {
                let got = r.got.map_or("-".into(), |v| v.to_string());
                let note = r.error.as_deref().filter(|_| !r.ok()).unwrap_or("");
                println!("{} {} {got} {note}", r.name, r.wanted);
                off += usize::from(!r.ok());
            }
            if off > 0 {
                anyhow::bail!("{off} controls read back different");
            }
        }
    }
    Ok(())
}
