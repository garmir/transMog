//! every pass the stack ui can add, with the constructor that builds it

use crate::render::Pass;

use super::lens::fisheye::Fisheye;
use super::preprocess::Preprocess;
use super::tools;

pub struct Entry {
    pub name: &'static str,
    /// "lens", "preprocess" or "tooooools"
    pub group: &'static str,
    pub make: fn(&wgpu::Device, &wgpu::BindGroupLayout) -> Box<dyn Pass>,
}

pub const GROUPS: [&str; 3] = ["lens", "preprocess", "tooooools"];

/// in ui order: the lens, the shared preprocessing, then the
/// tooooools effects in the order the site lists them
pub fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "fisheye",
            group: "lens",
            make: |d, l| Box::new(Fisheye::new(d, l)),
        },
        Entry {
            name: "preprocess",
            group: "preprocess",
            make: |d, l| Box::new(Preprocess::new(d, l)),
        },
        Entry {
            name: "stippling",
            group: "tooooools",
            make: |d, l| Box::new(tools::stippling::Stippling::new(d, l)),
        },
        Entry {
            name: "dots",
            group: "tooooools",
            make: |d, l| Box::new(tools::dots::Dots::new(d, l)),
        },
        Entry {
            name: "patterns",
            group: "tooooools",
            make: |d, l| Box::new(tools::patterns::Patterns::new(d, l)),
        },
        Entry {
            name: "edge",
            group: "tooooools",
            make: |d, l| Box::new(tools::edge::Edge::new(d, l)),
        },
        Entry {
            name: "distort",
            group: "tooooools",
            make: |d, l| Box::new(tools::distort::Distort::new(d, l)),
        },
        Entry {
            name: "displace",
            group: "tooooools",
            make: |d, l| Box::new(tools::displace::Displace::new(d, l)),
        },
        Entry {
            name: "dithering",
            group: "tooooools",
            make: |d, l| Box::new(tools::dithering::Dithering::new(d, l)),
        },
        Entry {
            name: "bevel",
            group: "tooooools",
            make: |d, l| Box::new(tools::bevel::Bevel::new(d, l)),
        },
        Entry {
            name: "recolor",
            group: "tooooools",
            make: |d, l| Box::new(tools::recolor::Recolor::new(d, l)),
        },
        Entry {
            name: "scatter",
            group: "tooooools",
            make: |d, l| Box::new(tools::scatter::Scatter::new(d, l)),
        },
        Entry {
            name: "cellular automata",
            group: "tooooools",
            make: |d, l| Box::new(tools::cellular_automata::CellularAutomata::new(d, l)),
        },
        Entry {
            name: "gradients",
            group: "tooooools",
            make: |d, l| Box::new(tools::gradients::Gradients::new(d, l)),
        },
        Entry {
            name: "crt",
            group: "tooooools",
            make: |d, l| Box::new(tools::crt::Crt::new(d, l)),
        },
        Entry {
            name: "ascii",
            group: "tooooools",
            make: |d, l| Box::new(tools::ascii::Ascii::new(d, l)),
        },
    ]
}

pub fn find(name: &str) -> Option<Entry> {
    entries().into_iter().find(|e| e.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_groups_known() {
        let entries = entries();
        for (i, e) in entries.iter().enumerate() {
            assert!(
                GROUPS.contains(&e.group),
                "{} has group {}",
                e.name,
                e.group
            );
            assert!(
                !entries[..i].iter().any(|o| o.name == e.name),
                "{} listed twice",
                e.name
            );
        }
    }

    #[test]
    fn tools_follow_the_site_order() {
        let tools: Vec<&str> = entries()
            .iter()
            .filter(|e| e.group == "tooooools")
            .map(|e| e.name)
            .collect();
        assert_eq!(
            tools,
            [
                "stippling",
                "dots",
                "patterns",
                "edge",
                "distort",
                "displace",
                "dithering",
                "bevel",
                "recolor",
                "scatter",
                "cellular automata",
                "gradients",
                "crt",
                "ascii",
            ]
        );
    }

    /// every constructor compiles its shaders on the driver and renders
    /// a frame; only the preprocess at defaults has to pass grey through
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_every_entry_builds_and_renders() {
        use crate::render::tests::{headless_device, render_grey};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        for entry in entries() {
            let mut renderer = Renderer::new(&device, &queue);
            let pass = (entry.make)(&device, renderer.input_layout());
            assert_eq!(pass.name(), entry.name);
            assert!(pass.enabled(), "{} starts enabled", entry.name);
            renderer.push_pass(pass);
            let pixels = render_grey(&device, &queue, &mut renderer, 8, 4);
            assert_eq!(pixels.len(), 8 * 4 * 4, "{}", entry.name);
            if entry.group == "preprocess" {
                assert!(
                    pixels
                        .chunks_exact(4)
                        .all(|px| (i32::from(px[0]) - 130).abs() <= 2),
                    "{} changed a grey frame",
                    entry.name
                );
            }
        }
    }
}
