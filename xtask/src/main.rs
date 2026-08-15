use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
mod util;

#[derive(Parser, Debug)]
#[command(name = "cargo-xtask", bin_name = "cargo xtask")]
#[command(about = "Automation tasks, stress testing, benchmarks, and CI helpers for evp")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the stress test suite and generate performance comparison report
    #[command(alias = "stress-test")]
    Stress(commands::stress::StressArgs),

    /// Run full local CI validation (fmt, clippy, tests, smoke test)
    Ci(commands::ci::CiArgs),

    /// Run the canonical render benchmark harness
    Bench(commands::bench::BenchArgs),

    /// Batch render examples or summarize their performance stats
    Examples(commands::examples::ExamplesArgs),

    /// Manage prebuilt libghostty artifacts via Docker Bake or native Zig compilation
    Ghostty(commands::ghostty::GhosttyArgs),
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Stress(args) => commands::stress::run(args),
        Commands::Ci(args) => commands::ci::run(args),
        Commands::Bench(args) => commands::bench::run(args),
        Commands::Examples(args) => commands::examples::run(args),
        Commands::Ghostty(args) => commands::ghostty::run(args),
    }
}
