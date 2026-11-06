//! recolor: a gradient map over brightness, hue or saturation, with
//! perlin noise, posterize and repetition on the way in

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Map {
    Brightness,
    Hue,
    Saturation,
}

impl Map {
    pub const ALL: [Map; 3] = [Map::Brightness, Map::Hue, Map::Saturation];

    pub fn label(self) -> &'static str {
        match self {
            Map::Brightness => "brightness",
            Map::Hue => "hue",
            Map::Saturation => "saturation",
        }
    }

    /// matches the constants in recolor.wgsl
    fn index(self) -> u32 {
        match self {
            Map::Brightness => 0,
            Map::Hue => 1,
            Map::Saturation => 2,
        }
    }
}

/// one gradient stop: a position 0..100 and an srgb colour
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stop {
    pub position: f32,
    pub colour: [u8; 3],
}

pub const MAX_STOPS: usize = 8;
pub const MIN_STOPS: usize = 2;

/// mirrors `Params` in recolor.wgsl: eight vec4 stops (rgb, position
/// 0..1) then the scalars, 160 bytes
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub stops: [[f32; 4]; MAX_STOPS],
    pub count: u32,
    pub posterize: f32,
    pub intensity: f32,
    pub scale: f32,
    pub gamma: f32,
    pub reps: f32,
    pub map: u32,
    pub _pad: u32,
}

pub const POSTERIZE_RANGE: std::ops::RangeInclusive<u32> = 2..=255;
pub const INTENSITY_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;
pub const SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.01..=1.0;
pub const GAMMA_RANGE: std::ops::RangeInclusive<f32> = 0.1..=5.0;
pub const REPS_RANGE: std::ops::RangeInclusive<u32> = 1..=10;
pub const POSITION_RANGE: std::ops::RangeInclusive<f32> = 0.0..=100.0;

const DEFAULT_STOPS: [Stop; 3] = [
    Stop {
        position: 0.0,
        colour: [0x00, 0x27, 0x8a],
    },
    Stop {
        position: 50.0,
        colour: [0xfe, 0x76, 0xec],
    },
    Stop {
        position: 100.0,
        colour: [0xfe, 0xff, 0xfa],
    },
];

pub struct Recolor {
    enabled: bool,
    posterize: u32,
    intensity: f32,
    scale: f32,
    gamma: f32,
    reps: u32,
    map: Map,
    /// in the order the ui shows them; sorted by position only when
    /// the uniform is built, so a row stays put while it is dragged
    stops: Vec<Stop>,
    pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
}

impl Recolor {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Recolor {
        let module = build_shader(device, "recolor", include_str!("recolor.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "recolor params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("recolor"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "recolor", &module, "fs_main", &layout);
        Recolor {
            enabled: true,
            posterize: 255,
            intensity: 0.0,
            scale: 0.3,
            gamma: 1.0,
            reps: 1,
            map: Map::Brightness,
            stops: DEFAULT_STOPS.to_vec(),
            pipeline,
            uniform,
            input: InputGroupCache::new(),
        }
    }

    pub fn map(&self) -> Map {
        self.map
    }

    pub fn set_map(&mut self, map: Map) {
        self.map = map;
    }

    pub fn posterize(&self) -> u32 {
        self.posterize
    }

    pub fn set_posterize(&mut self, levels: u32) {
        self.posterize = levels.clamp(*POSTERIZE_RANGE.start(), *POSTERIZE_RANGE.end());
    }

    /// noise intensity, scale and gamma
    pub fn noise(&self) -> (f32, f32, f32) {
        (self.intensity, self.scale, self.gamma)
    }

    pub fn set_noise(&mut self, intensity: f32, scale: f32, gamma: f32) {
        self.intensity = intensity.clamp(*INTENSITY_RANGE.start(), *INTENSITY_RANGE.end());
        self.scale = scale.clamp(*SCALE_RANGE.start(), *SCALE_RANGE.end());
        self.gamma = gamma.clamp(*GAMMA_RANGE.start(), *GAMMA_RANGE.end());
    }

    pub fn reps(&self) -> u32 {
        self.reps
    }

    pub fn set_reps(&mut self, reps: u32) {
        self.reps = reps.clamp(*REPS_RANGE.start(), *REPS_RANGE.end());
    }

    pub fn stops(&self) -> &[Stop] {
        &self.stops
    }

    /// keeps 2..8 stops; extra stops are dropped, a short list is
    /// padded with the defaults
    pub fn set_stops(&mut self, stops: &[Stop]) {
        self.stops = stops.iter().take(MAX_STOPS).copied().collect();
        for stop in DEFAULT_STOPS.iter().skip(self.stops.len()) {
            if self.stops.len() >= MIN_STOPS {
                break;
            }
            self.stops.push(*stop);
        }
        for stop in &mut self.stops {
            stop.position = stop
                .position
                .clamp(*POSITION_RANGE.start(), *POSITION_RANGE.end());
        }
    }

    /// a new stop halfway between the two highest stops, with the
    /// colour the gradient already has there, so adding changes nothing
    /// until it is edited
    pub fn add_stop(&mut self) {
        if self.stops.len() >= MAX_STOPS {
            return;
        }
        let mut sorted = self.stops.clone();
        sorted.sort_by(|a, b| a.position.total_cmp(&b.position));
        let (a, b) = (sorted[sorted.len() - 2], sorted[sorted.len() - 1]);
        let colour = std::array::from_fn(|i| {
            ((f32::from(a.colour[i]) + f32::from(b.colour[i])) * 0.5).round() as u8
        });
        self.stops.push(Stop {
            position: (a.position + b.position) * 0.5,
            colour,
        });
    }

    pub fn remove_stop(&mut self, index: usize) {
        if self.stops.len() > MIN_STOPS && index < self.stops.len() {
            self.stops.remove(index);
        }
    }

    pub fn reset(&mut self) {
        self.posterize = 255;
        self.intensity = 0.0;
        self.scale = 0.3;
        self.gamma = 1.0;
        self.reps = 1;
        self.map = Map::Brightness;
        self.stops = DEFAULT_STOPS.to_vec();
    }

    fn uniform(&self) -> Uniform {
        let mut sorted = self.stops.clone();
        sorted.sort_by(|a, b| a.position.total_cmp(&b.position));
        let mut stops = [[0.0; 4]; MAX_STOPS];
        for (slot, stop) in stops.iter_mut().zip(&sorted) {
            let [r, g, b] = stop.colour.map(|c| f32::from(c) / 255.0);
            *slot = [r, g, b, stop.position / 100.0];
        }
        Uniform {
            stops,
            count: sorted.len() as u32,
            posterize: self.posterize as f32,
            intensity: self.intensity,
            scale: self.scale,
            gamma: self.gamma,
            reps: self.reps as f32,
            map: self.map.index(),
            _pad: 0,
        }
    }
}

impl Pass for Recolor {
    fn name(&self) -> &'static str {
        "recolor"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("map");
            egui::ComboBox::from_id_salt("recolor map")
                .selected_text(self.map.label())
                .show_ui(ui, |ui| {
                    for map in Map::ALL {
                        ui.selectable_value(&mut self.map, map, map.label());
                    }
                });
        });
        ui.add(egui::Slider::new(&mut self.posterize, POSTERIZE_RANGE).text("posterize"));
        ui.add(egui::Slider::new(&mut self.intensity, INTENSITY_RANGE).text("noise intensity"));
        ui.add(egui::Slider::new(&mut self.scale, SCALE_RANGE).text("noise scale"));
        ui.add(egui::Slider::new(&mut self.gamma, GAMMA_RANGE).text("noise gamma"));
        ui.add(egui::Slider::new(&mut self.reps, REPS_RANGE).text("repetitions"));
        ui.label("stops");
        let mut remove = None;
        for (i, stop) in self.stops.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut stop.position)
                        .range(POSITION_RANGE)
                        .speed(0.5)
                        .suffix("%"),
                );
                ui.color_edit_button_srgb(&mut stop.colour);
                if ui.small_button("remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.remove_stop(i);
        }
        if ui
            .add_enabled(self.stops.len() < MAX_STOPS, egui::Button::new("add stop"))
            .clicked()
        {
            self.add_stop();
        }
        if ui.button("reset").clicked() {
            self.reset();
        }
    }

    fn encode(
        &mut self,
        ctx: &PassCtx,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
    ) {
        self.uniform.write(ctx.queue, &self.uniform());
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "recolor",
            &self.pipeline,
            &[input_group, &self.uniform.group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_160_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 160);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, stops), 0);
        assert_eq!(std::mem::offset_of!(Uniform, count), 128);
        assert_eq!(std::mem::offset_of!(Uniform, posterize), 132);
        assert_eq!(std::mem::offset_of!(Uniform, intensity), 136);
        assert_eq!(std::mem::offset_of!(Uniform, scale), 140);
        assert_eq!(std::mem::offset_of!(Uniform, gamma), 144);
        assert_eq!(std::mem::offset_of!(Uniform, reps), 148);
        assert_eq!(std::mem::offset_of!(Uniform, map), 152);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 160);
    }

    /// black maps to the first stop and white, which posterize pushes
    /// just past 1, to the last
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_black_and_white_hit_the_end_stops() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let mut recolor = Recolor::new(&device, renderer.input_layout());
        assert_eq!(recolor.stops().len(), 3);
        recolor.add_stop();
        assert_eq!(recolor.stops().len(), 4);
        assert_eq!(recolor.stops()[3].position, 75.0);
        recolor.remove_stop(3);
        renderer.push_pass(Box::new(recolor));
        let (w, h) = (64, 32);
        renderer.upload(&device, &queue, &half_frame(w, h));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), w, h);
        for y in 0..h {
            for x in 0..w {
                let px = &pixels[((y * w + x) * 4) as usize..][..4];
                let expected: [i32; 4] = if x < w / 2 {
                    [0x00, 0x27, 0x8a, 255]
                } else {
                    [0xfe, 0xff, 0xfa, 255]
                };
                for (a, e) in px.iter().zip(expected) {
                    assert!(
                        (i32::from(*a) - e).abs() <= 1,
                        "pixel ({x}, {y}) is {px:?}, expected {expected:?}"
                    );
                }
            }
        }
    }
}
