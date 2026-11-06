//! cellular automata: the frame seeds a grid of cells, the chosen rule
//! runs `steps` generations on the gpu between two r8 textures with a
//! row-sum texture in between, and the live cells are painted black on
//! white. the grid is re-seeded from every frame, as the site does

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Classic,
    LargerThanLife,
    Mncab,
    Mncc,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Classic, Kind::LargerThanLife, Kind::Mncab, Kind::Mncc];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Classic => "classic",
            Kind::LargerThanLife => "larger than life",
            Kind::Mncab => "mncab",
            Kind::Mncc => "mncc",
        }
    }

    /// matches the constants in cellular_automata.wgsl
    fn index(self) -> u32 {
        match self {
            Kind::Classic => 0,
            Kind::LargerThanLife => 1,
            Kind::Mncab => 2,
            Kind::Mncc => 3,
        }
    }

    /// the square half-widths the rule needs summed, one per channel
    /// of the row-sum texture
    fn radii(self) -> [u32; 4] {
        match self {
            Kind::Classic => [1, 0, 0, 0],
            Kind::LargerThanLife => [5, 0, 0, 0],
            Kind::Mncab => [1, 2, 0, 0],
            Kind::Mncc => [1, 2, 3, 4],
        }
    }
}

/// mirrors `Params` in cellular_automata.wgsl: one row of scalars and
/// six vec4 rows, 112 bytes
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub threshold: f32,
    pub cell: u32,
    pub kind: u32,
    pub _pad: u32,
    pub radii: [u32; 4],
    pub classic: [f32; 4],
    pub ltl: [f32; 4],
    pub mncab: [f32; 4],
    pub mncc_lo: [f32; 4],
    pub mncc_hi: [f32; 4],
}

pub const THRESHOLD_RANGE: std::ops::RangeInclusive<u32> = 0..=255;
pub const CELL_RANGE: std::ops::RangeInclusive<u32> = 1..=10;
pub const STEPS_RANGE: std::ops::RangeInclusive<u32> = 1..=50;
pub const CLASSIC_RANGE: std::ops::RangeInclusive<u32> = 0..=8;
pub const LTL_RANGE: std::ops::RangeInclusive<u32> = 0..=200;
pub const UNIT_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;

const DEFAULT_THRESHOLD: u32 = 128;
const DEFAULT_CELL: u32 = 2;
const DEFAULT_STEPS: u32 = 1;
/// survive lo, survive hi, birth lo, birth hi
const DEFAULT_CLASSIC: [u32; 4] = [1, 8, 3, 3];
/// birth lo, birth hi, survive lo, survive hi
const DEFAULT_LTL: [u32; 4] = [15, 91, 47, 102];
const DEFAULT_MNCAB: [f32; 2] = [0.35, 0.70];
const DEFAULT_MNCC_LO: [f32; 4] = [0.262, 0.342, 0.342, 0.889];
const DEFAULT_MNCC_HI: [f32; 4] = [0.903, 0.378, 0.382, 0.978];

const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
/// four row sums of up to 11 cells each: exact in f16
const SUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// a texture at cell-grid resolution with its cached bind group
struct Grid {
    view: wgpu::TextureView,
    input: InputGroupCache,
}

fn grid_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    label: &str,
) -> Grid {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    Grid {
        view: texture.create_view(&Default::default()),
        input: InputGroupCache::new(),
    }
}

/// create_fullscreen_pipeline writes rgba8; the grid passes need the
/// same pipeline aimed at an r8 or rgba16f target
fn grid_pipeline(
    device: &wgpu::Device,
    label: &str,
    module: &wgpu::ShaderModule,
    entry: &str,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

pub struct CellularAutomata {
    enabled: bool,
    threshold: u32,
    cell: u32,
    steps: u32,
    kind: Kind,
    classic: [u32; 4],
    ltl: [u32; 4],
    mncab: [f32; 2],
    mncc_lo: [f32; 4],
    mncc_hi: [f32; 4],
    seed: wgpu::RenderPipeline,
    hsum: wgpu::RenderPipeline,
    step: wgpu::RenderPipeline,
    show: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
    states: [Grid; 2],
    sums: Grid,
    grid: (u32, u32),
}

impl CellularAutomata {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> CellularAutomata {
        let module = build_shader(
            device,
            "cellular automata",
            include_str!("cellular_automata.wgsl"),
        );
        let uniform = UniformBuffer::<Uniform>::new(device, "cellular automata params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cellular automata"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let step_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cellular automata step"),
            bind_group_layouts: &[
                Some(input_layout),
                Some(&uniform.layout),
                Some(input_layout),
            ],
            immediate_size: 0,
        });
        CellularAutomata {
            enabled: true,
            threshold: DEFAULT_THRESHOLD,
            cell: DEFAULT_CELL,
            steps: DEFAULT_STEPS,
            kind: Kind::Classic,
            classic: DEFAULT_CLASSIC,
            ltl: DEFAULT_LTL,
            mncab: DEFAULT_MNCAB,
            mncc_lo: DEFAULT_MNCC_LO,
            mncc_hi: DEFAULT_MNCC_HI,
            seed: grid_pipeline(
                device,
                "cellular automata seed",
                &module,
                "fs_seed",
                &layout,
                STATE_FORMAT,
            ),
            hsum: grid_pipeline(
                device,
                "cellular automata row sums",
                &module,
                "fs_hsum",
                &layout,
                SUM_FORMAT,
            ),
            step: grid_pipeline(
                device,
                "cellular automata step",
                &module,
                "fs_step",
                &step_layout,
                STATE_FORMAT,
            ),
            show: create_fullscreen_pipeline(
                device,
                "cellular automata show",
                &module,
                "fs_show",
                &layout,
            ),
            uniform,
            input: InputGroupCache::new(),
            states: [
                grid_texture(device, 1, 1, STATE_FORMAT, "cellular automata a"),
                grid_texture(device, 1, 1, STATE_FORMAT, "cellular automata b"),
            ],
            sums: grid_texture(device, 1, 1, SUM_FORMAT, "cellular automata row sums"),
            grid: (1, 1),
        }
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, t: u32) {
        self.threshold = t.min(*THRESHOLD_RANGE.end());
    }

    pub fn cell_size(&self) -> u32 {
        self.cell
    }

    pub fn set_cell_size(&mut self, px: u32) {
        self.cell = px.clamp(*CELL_RANGE.start(), *CELL_RANGE.end());
    }

    pub fn steps(&self) -> u32 {
        self.steps
    }

    pub fn set_steps(&mut self, n: u32) {
        self.steps = n.clamp(*STEPS_RANGE.start(), *STEPS_RANGE.end());
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
    }

    /// back to the site's defaults; the enabled flag is left alone
    pub fn reset(&mut self) {
        self.threshold = DEFAULT_THRESHOLD;
        self.cell = DEFAULT_CELL;
        self.steps = DEFAULT_STEPS;
        self.kind = Kind::Classic;
        self.classic = DEFAULT_CLASSIC;
        self.ltl = DEFAULT_LTL;
        self.mncab = DEFAULT_MNCAB;
        self.mncc_lo = DEFAULT_MNCC_LO;
        self.mncc_hi = DEFAULT_MNCC_HI;
    }

    fn uniform(&self) -> Uniform {
        Uniform {
            threshold: self.threshold as f32,
            cell: self.cell,
            kind: self.kind.index(),
            _pad: 0,
            radii: self.kind.radii(),
            classic: self.classic.map(|v| v as f32),
            ltl: self.ltl.map(|v| v as f32),
            mncab: [self.mncab[0], self.mncab[1], 0.0, 0.0],
            mncc_lo: self.mncc_lo,
            mncc_hi: self.mncc_hi,
        }
    }

    fn ensure_grid(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let grid = (
            width.div_ceil(self.cell).max(1),
            height.div_ceil(self.cell).max(1),
        );
        if grid == self.grid {
            return;
        }
        self.grid = grid;
        self.states = [
            grid_texture(device, grid.0, grid.1, STATE_FORMAT, "cellular automata a"),
            grid_texture(device, grid.0, grid.1, STATE_FORMAT, "cellular automata b"),
        ];
        self.sums = grid_texture(
            device,
            grid.0,
            grid.1,
            SUM_FORMAT,
            "cellular automata row sums",
        );
        tracing::debug!(width = grid.0, height = grid.1, "cell grid recreated");
    }
}

fn range_row(
    ui: &mut egui::Ui,
    label: &str,
    lo: &mut u32,
    hi: &mut u32,
    range: std::ops::RangeInclusive<u32>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(lo).range(range.clone()));
        ui.label("to");
        ui.add(egui::DragValue::new(hi).range(range));
    });
}

fn unit_row(ui: &mut egui::Ui, label: &str, lo: &mut f32, hi: &mut f32) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            egui::DragValue::new(lo)
                .range(UNIT_RANGE)
                .speed(0.001)
                .fixed_decimals(3),
        );
        ui.label("to");
        ui.add(
            egui::DragValue::new(hi)
                .range(UNIT_RANGE)
                .speed(0.001)
                .fixed_decimals(3),
        );
    });
}

impl Pass for CellularAutomata {
    fn name(&self) -> &'static str {
        "cellular automata"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.threshold, THRESHOLD_RANGE).text("threshold"));
        ui.add(
            egui::Slider::new(&mut self.cell, CELL_RANGE)
                .suffix(" px")
                .text("cell size"),
        );
        ui.add(egui::Slider::new(&mut self.steps, STEPS_RANGE).text("steps"));
        ui.horizontal(|ui| {
            ui.label("type");
            egui::ComboBox::from_id_salt("cellular automata type")
                .selected_text(self.kind.label())
                .show_ui(ui, |ui| {
                    for kind in Kind::ALL {
                        ui.selectable_value(&mut self.kind, kind, kind.label());
                    }
                });
        });
        match self.kind {
            Kind::Classic => {
                let [s_lo, s_hi, b_lo, b_hi] = &mut self.classic;
                range_row(ui, "survive", s_lo, s_hi, CLASSIC_RANGE);
                range_row(ui, "birth", b_lo, b_hi, CLASSIC_RANGE);
            }
            Kind::LargerThanLife => {
                let [b_lo, b_hi, s_lo, s_hi] = &mut self.ltl;
                range_row(ui, "birth", b_lo, b_hi, LTL_RANGE);
                range_row(ui, "survive", s_lo, s_hi, LTL_RANGE);
            }
            Kind::Mncab => {
                ui.add(
                    egui::Slider::new(&mut self.mncab[0], UNIT_RANGE)
                        .fixed_decimals(2)
                        .text("threshold 1"),
                );
                ui.add(
                    egui::Slider::new(&mut self.mncab[1], UNIT_RANGE)
                        .fixed_decimals(2)
                        .text("threshold 2"),
                );
            }
            Kind::Mncc => {
                for i in 0..4 {
                    let label = format!("n{}", i + 1);
                    let (lo, hi) = (&mut self.mncc_lo[i], &mut self.mncc_hi[i]);
                    unit_row(ui, &label, lo, hi);
                }
            }
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
        self.ensure_grid(ctx.device, ctx.width, ctx.height);
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "cellular automata seed",
            &self.seed,
            &[input_group, &self.uniform.group],
            &self.states[0].view,
        );
        let mut current = 0;
        for _ in 0..self.steps {
            let next = 1 - current;
            let (from, to) = if current == 0 {
                let (a, b) = self.states.split_at_mut(1);
                (&mut a[0], &b[0])
            } else {
                let (a, b) = self.states.split_at_mut(1);
                (&mut b[0], &a[0])
            };
            let state_group =
                from.input
                    .get_with(ctx.device, ctx.input_layout, ctx.sampler, &from.view);
            draw_fullscreen(
                encoder,
                "cellular automata row sums",
                &self.hsum,
                &[state_group, &self.uniform.group],
                &self.sums.view,
            );
            let sums_group = self.sums.input.get_with(
                ctx.device,
                ctx.input_layout,
                ctx.sampler,
                &self.sums.view,
            );
            draw_fullscreen(
                encoder,
                "cellular automata step",
                &self.step,
                &[sums_group, &self.uniform.group, state_group],
                &to.view,
            );
            current = next;
        }
        let last = &mut self.states[current];
        let group = last
            .input
            .get_with(ctx.device, ctx.input_layout, ctx.sampler, &last.view);
        draw_fullscreen(
            encoder,
            "cellular automata show",
            &self.show,
            &[group, &self.uniform.group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_112_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 112);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, threshold), 0);
        assert_eq!(std::mem::offset_of!(Uniform, radii), 16);
        assert_eq!(std::mem::offset_of!(Uniform, classic), 32);
        assert_eq!(std::mem::offset_of!(Uniform, ltl), 48);
        assert_eq!(std::mem::offset_of!(Uniform, mncab), 64);
        assert_eq!(std::mem::offset_of!(Uniform, mncc_lo), 80);
        assert_eq!(std::mem::offset_of!(Uniform, mncc_hi), 96);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 112);
    }

    #[test]
    fn radii_cover_each_rule() {
        assert_eq!(Kind::Classic.radii(), [1, 0, 0, 0]);
        assert_eq!(Kind::LargerThanLife.radii(), [5, 0, 0, 0]);
        assert_eq!(Kind::Mncc.radii(), [1, 2, 3, 4]);
    }

    fn count_black(pixels: &[u8], width: usize, x_range: std::ops::Range<usize>) -> usize {
        pixels
            .chunks_exact(4)
            .enumerate()
            .filter(|(i, px)| x_range.contains(&(i % width)) && px[0] < 128)
            .count()
    }

    /// a solid black block survives the classic rule (every interior
    /// cell has 8 live neighbours, inside survive 1..8) and after one
    /// step the white half has only the seam column born (3 neighbours)
    /// plus its torus twin: 2 columns of 16 cells of 4 px. every rule
    /// type is run so each shader path compiles on the driver
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_black_half_stays_alive() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let ca = CellularAutomata::new(&device, renderer.input_layout());
        renderer.push_pass(Box::new(ca));
        let frame = half_frame(64, 32);
        renderer.upload(&device, &queue, &frame);
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        let left = count_black(&pixels, 64, 0..32);
        let right = count_black(&pixels, 64, 32..64);
        assert!(left > 32 * 32 / 2, "left half has {left} black pixels");
        assert!(right < left, "right half has {right} black pixels");
        assert!(right < 32 * 32 / 4, "right half has {right} black pixels");

        for kind in [Kind::LargerThanLife, Kind::Mncab, Kind::Mncc] {
            let mut ca = CellularAutomata::new(&device, renderer.input_layout());
            ca.set_kind(kind);
            ca.set_cell_size(1);
            renderer.passes_mut()[0].pass = Box::new(ca);
            renderer.render(&device, &queue);
            let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
            assert_eq!(pixels.len(), 64 * 32 * 4, "{kind:?}");
        }
    }
}
