//! Slipstream command-line interface.

mod commands;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "slipstream",
    version,
    about = "Slipstream: transaction-footprint analysis, contention scoring and CAP-0063 scheduling",
    long_about = "Slipstream measures how efficiently a Soroban smart-contract's transaction footprints \
                  parallelize under Stellar's phased execution model."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Statically analyze contract sources with the detector suite.
    Scan {
        /// A contract source file or directory of `.rs` files.
        path: PathBuf,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Profile a recorded transaction set (JSON fixture for now).
    Profile {
        /// Path to a transaction-set JSON fixture.
        #[arg(long)]
        fixture: PathBuf,
    },
    /// Simulate scheduling over a synthetic transaction set.
    Simulate {
        /// Number of synthetic transactions to generate.
        #[arg(long, default_value_t = 128)]
        transactions: usize,
        /// Number of distinct write keys in the synthetic set.
        #[arg(long, default_value_t = 16)]
        distinct: usize,
        /// Seed for the deterministic generator.
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
    /// Compare two contract implementations (e.g. naive vs optimized).
    Diff {
        /// Left implementation (file or directory).
        left: PathBuf,
        /// Right implementation (file or directory).
        right: PathBuf,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { path, json } => commands::scan(&path, json),
        Command::Profile { fixture } => commands::profile(&fixture),
        Command::Simulate {
            transactions,
            distinct,
            seed,
        } => commands::simulate(transactions, distinct, seed),
        Command::Diff { left, right, json } => commands::diff(&left, &right, json),
    }
}
