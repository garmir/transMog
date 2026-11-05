use std::path::Path;

use crate::capture::TapStatus;
use crate::controls::preset::Readback;

pub(crate) enum Event {
    Apply(String),
    Save(String),
    Reload,
    Tap(bool),
}

/// the preset picked in the combo box and the name typed for a save
#[derive(Default)]
pub(crate) struct State {
    pub choice: Option<String>,
    pub save_name: String,
}

pub(crate) fn show_presets(
    ui: &mut egui::Ui,
    state: &mut State,
    presets: &[String],
    readback: &[Readback],
    readback_format: Option<&(String, String)>,
) -> Option<Event> {
    let mut event = None;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("preset")
            .selected_text(state.choice.as_deref().unwrap_or("-"))
            .show_ui(ui, |ui| {
                for name in presets {
                    let on = state.choice.as_deref() == Some(name.as_str());
                    if ui.selectable_label(on, name).clicked() {
                        state.choice = Some(name.clone());
                    }
                }
            });
        if ui
            .add_enabled(state.choice.is_some(), egui::Button::new("apply"))
            .clicked()
        {
            event = state.choice.clone().map(Event::Apply);
        }
        // "rescan" is the device list's button already
        if ui.button("reload").clicked() {
            event = Some(Event::Reload);
        }
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.save_name)
                .hint_text("name")
                .desired_width(120.0),
        );
        if ui
            .add_enabled(
                !state.save_name.is_empty(),
                egui::Button::new("save current"),
            )
            .clicked()
        {
            event = Some(Event::Save(state.save_name.clone()));
        }
    });

    if readback.is_empty() && readback_format.is_none() {
        return event;
    }
    ui.label("read back after the last apply");
    egui::Grid::new("readback").striped(true).show(ui, |ui| {
        if let Some((wanted, got)) = readback_format {
            ui.monospace("format");
            ui.monospace(wanted);
            mark(ui, wanted == got, got);
            ui.end_row();
        }
        for r in readback {
            ui.monospace(&r.name);
            ui.monospace(r.wanted.to_string());
            let got = r.got.map_or("-".to_string(), |v| v.to_string());
            match &r.error {
                Some(e) if !r.ok() => mark(ui, false, &format!("{got} ({e})")),
                _ => mark(ui, r.ok(), &got),
            }
            ui.end_row();
        }
    });
    event
}

fn mark(ui: &mut egui::Ui, ok: bool, text: &str) {
    if ok {
        ui.monospace(text);
    } else {
        ui.colored_label(ui.visuals().warn_fg_color, text);
    }
}

pub(crate) fn show_tap(
    ui: &mut egui::Ui,
    status: Option<&TapStatus>,
    dir: &Path,
    overlay: &mut bool,
) -> Option<Event> {
    let mut event = None;
    let mut on = status.is_some();
    if ui
        .checkbox(&mut on, format!("tap frames to {}", dir.display()))
        .changed()
    {
        event = Some(Event::Tap(on));
    }
    if let Some(s) = status {
        ui.monospace(format!("{} frames, {} errors", s.written, s.errors));
        if let Some(e) = &s.last_error {
            ui.colored_label(ui.visuals().warn_fg_color, e);
        }
    }
    ui.checkbox(overlay, "overlay from the tap's overlay file");
    event
}
