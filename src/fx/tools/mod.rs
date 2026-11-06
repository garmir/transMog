//! the tooooools.app effects, one module per effect in the catalog order

pub mod ascii;
pub mod bevel;
pub mod cellular_automata;
pub mod crt;
pub mod displace;
pub mod distort;
pub mod dithering;
pub mod dots;
pub mod edge;
pub mod gradients;
pub mod patterns;
pub mod recolor;
pub mod scatter;
pub mod stippling;

/// a plain grid, or one whose odd rows are shifted half a cell
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grid {
    Regular,
    Benday,
}

impl Grid {
    pub const ALL: [Grid; 2] = [Grid::Regular, Grid::Benday];

    pub fn label(self) -> &'static str {
        match self {
            Grid::Regular => "regular",
            Grid::Benday => "benday",
        }
    }
}
