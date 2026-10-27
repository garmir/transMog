pub struct App;

impl App {
    pub fn new(_cc: &eframe::CreationContext) -> App {
        App
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label("camera-app");
        });
    }
}
