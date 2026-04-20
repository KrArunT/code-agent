mod agent;
mod completion;
mod config;
mod provider;
mod sessions;
mod tools;
mod ui;
mod workers;

use agent::Agent;
use anyhow::Result;
use clap::Parser;
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::parse().resolve().await?;
    let mut agent = Agent::new(config)?;
    if agent.is_worker_mode() {
        return agent.run_worker().await;
    }
    if agent.is_tui_enabled() {
        agent.run_tui().await
    } else {
        agent.run().await
    }
}
