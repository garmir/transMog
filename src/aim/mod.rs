//! aim mode: finds the visualiser's screens in the raw camera frames
//! by their timing marks and says how to point the camera so both stay
//! in shot. the analysis runs on its own thread a few times a second;
//! every other frame only has its stamps read where the last analysis
//! put them

pub mod calibration;
pub mod detect;
pub mod fit;
pub mod geom;
pub mod hint;
pub mod luma;
pub mod marks;
pub mod track;
mod worker;

pub use detect::{analyse, Edges, Name, Screen};
pub use fit::{FitRun, Limits, Ptz, Step};
pub use hint::{plan, Hint, Plan, Target};
pub use luma::Luma;
pub use track::Track;
pub use worker::{Aim, Report, ScreenReport};
