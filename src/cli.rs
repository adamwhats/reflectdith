//! Command line options: the engine's own, with each scene's flattened in.
use clap::Parser;

use crate::scenes::robot;

#[derive(Parser, Clone, Debug)]
#[command(about = "Dithered mesh / ROS 2 robot viewer")]
pub struct Cli {
    #[command(flatten)]
    pub robot: robot::Options,
    /// Start with the controls hidden; press Tab to show them.
    #[arg(long)]
    pub hide_gui: bool,
    /// Start with shadow strength at zero (also adjustable in the GUI).
    #[arg(long)]
    pub no_shadows: bool,
    /// Save a screenshot and exit after this many seconds (requires a display).
    #[arg(long)]
    pub capture_after: Option<u64>,
    /// Measure N frames after 120 warmup frames, waiting for the GPU each frame, then exit.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub benchmark_frames: Option<u32>,
}
