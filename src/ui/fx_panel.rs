use crate::render::Pass;

/// one enable checkbox per pass, with the pass's own widgets indented
/// below it
pub(crate) fn show(ui: &mut egui::Ui, passes: &mut [Box<dyn Pass>]) {
    if passes.is_empty() {
        ui.weak("no passes");
        return;
    }
    for (i, pass) in passes.iter_mut().enumerate() {
        let mut on = pass.enabled();
        if ui.checkbox(&mut on, pass.name()).changed() {
            pass.set_enabled(on);
        }
        ui.indent(("pass", i), |ui| pass.ui(ui));
    }
}
