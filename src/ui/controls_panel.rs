use crate::controls::{ControlInfo, ControlKind};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Set(u32, i64),
    Reset(u32),
    ResetAll,
}

/// draws the cached controls grouped by class and returns what the user
/// asked for; the caller applies the actions and re-queries, so the
/// widgets never hold state of their own
pub(crate) fn show(ui: &mut egui::Ui, infos: &[ControlInfo]) -> Vec<Action> {
    let mut actions = Vec::new();
    if infos.is_empty() {
        ui.weak("no controls (start a capture)");
        return actions;
    }
    if ui.button("reset all").clicked() {
        actions.push(Action::ResetAll);
    }

    let mut classes: Vec<&str> = Vec::new();
    for info in infos {
        if !classes.contains(&info.class.as_str()) {
            classes.push(&info.class);
        }
    }

    for class in classes {
        egui::CollapsingHeader::new(class)
            .default_open(true)
            .show(ui, |ui| {
                for info in infos.iter().filter(|i| i.class == class) {
                    show_control(ui, info, &mut actions);
                }
            });
    }
    actions
}

fn show_control(ui: &mut egui::Ui, info: &ControlInfo, actions: &mut Vec<Action>) {
    let enabled = !info.inactive && !info.read_only;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(enabled, |ui| match &info.kind {
            ControlKind::Integer {
                min,
                max,
                step,
                default: _,
            } => {
                let mut value = info.value;
                let slider = egui::Slider::new(&mut value, *min..=*max)
                    .step_by((*step).max(1) as f64)
                    .text(&info.name);
                // changed() also fires when the slider snaps a value the
                // driver reported off its grid, which would loop forever
                if ui.add(slider).changed() && value != info.value {
                    actions.push(Action::Set(info.id, value));
                }
            }
            ControlKind::Boolean { default: _ } => {
                let mut on = info.value != 0;
                if ui.checkbox(&mut on, &info.name).changed() {
                    actions.push(Action::Set(info.id, i64::from(on)));
                }
            }
            ControlKind::Menu { items, default: _ } => {
                let current = items
                    .iter()
                    .find(|(v, _)| *v == info.value)
                    .map(|(_, name)| name.as_str())
                    .unwrap_or("?");
                egui::ComboBox::from_id_salt(info.id)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (value, name) in items {
                            if ui.selectable_label(*value == info.value, name).clicked()
                                && *value != info.value
                            {
                                actions.push(Action::Set(info.id, *value));
                            }
                        }
                    });
                ui.label(&info.name);
            }
            ControlKind::Button => {
                if ui.button(&info.name).clicked() {
                    actions.push(Action::Set(info.id, 1));
                }
            }
            ControlKind::Other => {
                ui.weak(format!("{}: {}", info.name, info.value));
            }
        });
        let resettable = enabled && !matches!(info.kind, ControlKind::Button | ControlKind::Other);
        if ui
            .add_enabled(resettable, egui::Button::new("default").small())
            .clicked()
        {
            actions.push(Action::Reset(info.id));
        }
    });
}
