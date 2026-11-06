//! rectification: each visualiser screen as the camera sees it, mapped
//! back to straight frame pixels. the corner fiducials are refined to
//! sub-pixel corners and followed from frame to frame, every screen gets
//! its own homography and the screens share one radial lens fit; the
//! marks' regions are then resampled into tiles and written beside the
//! qa tap with the source frame's timestamp and map, so the decoders
//! read straight, evenly sampled cells

pub mod edges;
pub mod map;
pub mod out;
pub mod track;
pub mod warp;
mod worker;

pub use map::{Geometry, Lens, ScreenMap};
pub use out::Record;
pub use warp::Tile;
pub use worker::{Rectify, Report, ScreenView, DEFAULT_SCALE, HISTORY};
