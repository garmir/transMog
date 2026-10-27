use crate::capture::{FormatInfo, SourceFormat};

use super::{App, Selection};

pub(crate) enum Event {
    Rescan,
    SelectDevice(usize),
    Select(Selection),
    Toggle,
}

/// mjpeg 1280x720 at 30 first, then the largest yuyv size that offers
/// 30 fps, then whatever comes first
pub(crate) fn preferred_selection(formats: &[FormatInfo]) -> Selection {
    let with_30 = |fps: &[u32]| fps.iter().position(|&f| f == 30);

    for (fi, format) in formats.iter().enumerate() {
        if format.format != SourceFormat::Mjpeg {
            continue;
        }
        for (si, size) in format.sizes.iter().enumerate() {
            if size.width == 1280 && size.height == 720 {
                if let Some(fps) = with_30(&size.fps) {
                    return Selection {
                        format: fi,
                        size: si,
                        fps,
                    };
                }
            }
        }
    }

    let mut best: Option<(u64, Selection)> = None;
    for (fi, format) in formats.iter().enumerate() {
        if format.format != SourceFormat::Yuyv {
            continue;
        }
        for (si, size) in format.sizes.iter().enumerate() {
            let Some(fps) = with_30(&size.fps) else {
                continue;
            };
            let area = u64::from(size.width) * u64::from(size.height);
            if best.is_none_or(|(a, _)| area > a) {
                best = Some((
                    area,
                    Selection {
                        format: fi,
                        size: si,
                        fps,
                    },
                ));
            }
        }
    }

    best.map(|(_, s)| s).unwrap_or_default()
}

fn format_label(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Yuyv => "YUYV",
        SourceFormat::Mjpeg => "MJPEG",
    }
}

pub(crate) fn show_top_bar(ui: &mut egui::Ui, app: &App) -> Option<Event> {
    let mut event = None;
    ui.horizontal_wrapped(|ui| {
        let device_text = app
            .selected_device
            .and_then(|i| app.devices.get(i))
            .map(|d| format!("{} ({})", d.name, d.path.display()))
            .unwrap_or_else(|| "no device".to_owned());
        ui.add_enabled_ui(!app.devices.is_empty(), |ui| {
            egui::ComboBox::from_id_salt("device")
                .selected_text(device_text)
                .show_ui(ui, |ui| {
                    for (i, d) in app.devices.iter().enumerate() {
                        let label = format!("{} ({})", d.name, d.path.display());
                        if ui
                            .selectable_label(app.selected_device == Some(i), label)
                            .clicked()
                            && app.selected_device != Some(i)
                        {
                            event = Some(Event::SelectDevice(i));
                        }
                    }
                });
        });

        let mut selection = app.selection;
        let format = app.formats.get(selection.format);
        let size = format.and_then(|f| f.sizes.get(selection.size));

        ui.add_enabled_ui(!app.formats.is_empty(), |ui| {
            egui::ComboBox::from_id_salt("format")
                .selected_text(format.map(|f| format_label(f.format)).unwrap_or("-"))
                .show_ui(ui, |ui| {
                    for (i, f) in app.formats.iter().enumerate() {
                        if ui
                            .selectable_label(selection.format == i, format_label(f.format))
                            .clicked()
                            && selection.format != i
                        {
                            selection = Selection {
                                format: i,
                                size: 0,
                                fps: 0,
                            };
                        }
                    }
                });

            egui::ComboBox::from_id_salt("size")
                .selected_text(
                    size.map(|s| format!("{}x{}", s.width, s.height))
                        .unwrap_or_else(|| "-".to_owned()),
                )
                .show_ui(ui, |ui| {
                    if let Some(f) = format {
                        for (i, s) in f.sizes.iter().enumerate() {
                            if ui
                                .selectable_label(
                                    selection.size == i,
                                    format!("{}x{}", s.width, s.height),
                                )
                                .clicked()
                                && selection.size != i
                            {
                                selection.size = i;
                                selection.fps = 0;
                            }
                        }
                    }
                });

            let fps_text = size
                .and_then(|s| s.fps.get(selection.fps))
                .map(|f| format!("{f} fps"))
                .unwrap_or_else(|| "-".to_owned());
            egui::ComboBox::from_id_salt("fps")
                .selected_text(fps_text)
                .show_ui(ui, |ui| {
                    if let Some(s) = size {
                        for (i, f) in s.fps.iter().enumerate() {
                            if ui
                                .selectable_label(selection.fps == i, format!("{f} fps"))
                                .clicked()
                            {
                                selection.fps = i;
                            }
                        }
                    }
                });
        });

        if selection != app.selection {
            event = Some(Event::Select(selection));
        }

        let capturing = app.capture.is_some();
        let can_start = app.capture_config().is_some();
        if ui
            .add_enabled(
                capturing || can_start,
                egui::Button::new(if capturing { "stop" } else { "start" }),
            )
            .clicked()
        {
            event = Some(Event::Toggle);
        }

        match &app.capture {
            Some(capture) => {
                let seq = capture.latest().map(|f| f.seq).unwrap_or(0);
                ui.monospace(format!("{:5.1} fps  seq {seq}", capture.measured_fps()));
            }
            None => {
                ui.monospace("  -.- fps  seq -");
            }
        }

        if ui.button("rescan").clicked() {
            event = Some(Event::Rescan);
        }
    });
    event
}
