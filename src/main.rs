mod agent;
mod completion;
mod config;
mod provider;
mod tools;
mod ui;

use agent::Agent;
use anyhow::Result;
use clap::Parser;
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::parse().resolve().await?;
    let mut agent = Agent::new(config)?;
    if agent.is_tui_enabled() {
        agent.run_tui().await
    } else {
        agent.run().await
    }
}
