//! battcurve — sample and visualize laptop battery charge / discharge curves.

mod cmd;
mod core;

use crate::core::storage::{self, Backend};
use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "battcurve", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Continuously sample the battery into the store (background logger).
    Log {
        #[arg(long, default_value = "10s")]
        interval: String,
        #[arg(long, default_value = "sqlite")]
        store: String,
        #[arg(long)]
        battery: Option<String>,
    },
    /// Capture a single charge/discharge session, then print a summary.
    Capture {
        #[arg(long, default_value = "10s")]
        interval: String,
        #[arg(long, default_value = "sqlite")]
        store: String,
        #[arg(long)]
        battery: Option<String>,
        /// Stop condition: ctrl-c | full | empty
        #[arg(long, default_value = "ctrl-c")]
        until: String,
    },
    /// htop-style live terminal monitor.
    Tui {
        #[arg(long, default_value = "sqlite")]
        store: String,
        #[arg(long)]
        battery: Option<String>,
    },
    /// Serve interactive analysis charts in the browser.
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
        #[arg(long, default_value = "sqlite")]
        store: String,
    },
    /// Print the resolved data file paths.
    Paths,
}

fn backend(s: &str) -> Result<Backend> {
    s.parse()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Log {
            interval,
            store,
            battery,
        } => cmd::log::run(cmd::parse_duration(&interval)?, backend(&store)?, battery),
        Command::Capture {
            interval,
            store,
            battery,
            until,
        } => cmd::capture::run(
            cmd::parse_duration(&interval)?,
            backend(&store)?,
            battery,
            until.parse()?,
        ),
        Command::Tui { store, battery } => cmd::tui::run(backend(&store)?, battery),
        Command::Serve { port, store } => cmd::serve::run(port, backend(&store)?).await,
        Command::Paths => {
            println!("data dir: {}", storage::data_dir()?.display());
            println!("sqlite:   {}", storage::default_db_path()?.display());
            println!("csv:      {}", storage::default_csv_path()?.display());
            Ok(())
        }
    }
}
