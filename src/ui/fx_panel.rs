use crate::fx::catalog;
use crate::render::Renderer;

enum Action {
    Up(usize),
    Down(usize),
    Remove(usize),
}

/// the pass stack: an add menu and the bypass toggle on top, then one
/// row per pass with its enable checkbox, order buttons and widgets
pub(crate) fn show(ui: &mut egui::Ui, renderer: &mut Renderer, device: &wgpu::Device) {
    let mut add = None;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("add pass")
            .selected_text("add")
            .show_ui(ui, |ui| {
                let mut group = "";
                for entry in catalog::entries() {
                    if entry.group != group {
                        group = entry.group;
                        ui.weak(group);
                    }
                    if ui.selectable_label(false, entry.name).clicked() {
                        add = Some(entry);
                    }
                }
            });
        let mut bypass = renderer.bypass();
        if ui.checkbox(&mut bypass, "bypass all").changed() {
            renderer.set_bypass(bypass);
        }
    });
    if let Some(entry) = add {
        let pass = (entry.make)(device, renderer.input_layout());
        renderer.push_pass(pass);
    }

    let passes = renderer.passes_mut();
    if passes.is_empty() {
        ui.weak("no passes");
        return;
    }
    let count = passes.len();
    let mut action = None;
    for (i, slot) in passes.iter_mut().enumerate() {
        ui.push_id(slot.id, |ui| {
            ui.horizontal(|ui| {
                let mut on = slot.pass.enabled();
                if ui.checkbox(&mut on, slot.pass.name()).changed() {
                    slot.pass.set_enabled(on);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("remove").clicked() {
                        action = Some(Action::Remove(i));
                    }
                    if ui
                        .add_enabled(i + 1 < count, egui::Button::new("down").small())
                        .clicked()
                    {
                        action = Some(Action::Down(i));
                    }
                    if ui
                        .add_enabled(i > 0, egui::Button::new("up").small())
                        .clicked()
                    {
                        action = Some(Action::Up(i));
                    }
                });
            });
            ui.indent("pass", |ui| slot.pass.ui(ui));
        });
    }
    match action {
        Some(Action::Up(i)) => passes.swap(i, i - 1),
        Some(Action::Down(i)) => passes.swap(i, i + 1),
        Some(Action::Remove(i)) => {
            passes.remove(i);
        }
        None => {}
    }
}
