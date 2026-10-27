use crate::capture::{FormatInfo, SourceFormat};

use super::{App, Selection};

pub(crate) enum Event {
    Rescan,
    SelectDevice(usize),
    Select(Selection),
    Toggle,
}

/// one usb 2 isochronous endpoint moves at most 3072 bytes per 125 us
/// microframe, and yuyv is uncompressed, so a yuyv mode above this rate
/// is delivered at a fraction of its advertised fps
const USB2_ISO_BYTES_PER_SECOND: u64 = 3072 * 8000;

/// mjpeg 1280x720 at 30 first, then the yuyv mode with the highest fps
/// and then the largest area whose byte rate fits usb 2, then whatever
/// comes first
pub(crate) fn preferred_selection(formats: &[FormatInfo]) -> Selection {
    for (fi, format) in formats.iter().enumerate() {
        if format.format != SourceFormat::Mjpeg {
            continue;
        }
        for (si, size) in format.sizes.iter().enumerate() {
            if size.width == 1280 && size.height == 720 {
                if let Some(fps) = size.fps.iter().position(|&f| f == 30) {
                    return Selection {
                        format: fi,
                        size: si,
                        fps,
                    };
                }
            }
        }
    }

    let mut best: Option<((u32, u64), Selection)> = None;
    for (fi, format) in formats.iter().enumerate() {
        if format.format != SourceFormat::Yuyv {
            continue;
        }
        for (si, size) in format.sizes.iter().enumerate() {
            let area = u64::from(size.width) * u64::from(size.height);
            for (fpsi, &fps) in size.fps.iter().enumerate() {
                if area * 2 * u64::from(fps) > USB2_ISO_BYTES_PER_SECOND {
                    continue;
                }
                let key = (fps, area);
                if best.is_none_or(|(k, _)| key > k) {
                    best = Some((
                        key,
                        Selection {
                            format: fi,
                            size: si,
                            fps: fpsi,
                        },
                    ));
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::FrameSize;

    fn size(width: u32, height: u32, fps: &[u32]) -> FrameSize {
        FrameSize {
            width,
            height,
            fps: fps.to_vec(),
        }
    }

    /// the built-in camera's yuyv table: 1600x1200@30 is advertised but
    /// arrives at under 5 fps, 640x480@30 is the fastest mode that fits
    #[test]
    fn yuyv_only_camera_picks_a_mode_usb2_can_carry() {
        let formats = vec![FormatInfo {
            format: SourceFormat::Yuyv,
            sizes: vec![
                size(1600, 1200, &[30]),
                size(1280, 720, &[30, 25, 20, 15]),
                size(640, 480, &[30]),
                size(352, 288, &[30]),
                size(320, 240, &[30]),
                size(800, 600, &[15]),
            ],
        }];
        assert_eq!(
            preferred_selection(&formats),
            Selection {
                format: 0,
                size: 2,
                fps: 0
            }
        );
    }

    #[test]
    fn mjpeg_720p30_wins_over_yuyv() {
        let formats = vec![
            FormatInfo {
                format: SourceFormat::Yuyv,
                sizes: vec![size(640, 480, &[30])],
            },
            FormatInfo {
                format: SourceFormat::Mjpeg,
                sizes: vec![size(1920, 1080, &[30]), size(1280, 720, &[30, 24])],
            },
        ];
        assert_eq!(
            preferred_selection(&formats),
            Selection {
                format: 1,
                size: 1,
                fps: 0
            }
        );
    }

    #[test]
    fn nothing_fitting_falls_back_to_the_first_entry() {
        let formats = vec![FormatInfo {
            format: SourceFormat::Yuyv,
            sizes: vec![size(1920, 1080, &[30])],
        }];
        assert_eq!(preferred_selection(&formats), Selection::default());
    }
}
