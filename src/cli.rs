use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "ozma", version, about = "A general purpose TUI LLM client")]
pub struct Cli {
    /// Tick rate in ticks per second (background logic frequency)
    #[arg(long, default_value_t = 4.0)]
    pub tick_rate: f64,

    /// Frame rate in frames per second (render frequency)
    #[arg(long, default_value_t = 30.0)]
    pub frame_rate: f64,
}
