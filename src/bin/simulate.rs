use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use rotastellar_agent::{Agent, AgentConfig, SimulatedSatellite, WorkloadSpec};

#[derive(Parser)]
#[command(name = "rotastellar-agent")]
#[command(about = "RotaStellar Operator Agent — execute orbital compute workloads")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a simulated execution from a CAE plan JSON file
    Simulate {
        /// Path to the CAE plan JSON file (containing plan_data + events)
        #[arg(long)]
        plan: PathBuf,

        /// Replay speed multiplier (1 = real-time, 100 = 100x faster)
        #[arg(long, default_value = "100")]
        speed: f64,

        /// Console API URL to report events to
        #[arg(long, default_value = "https://console.rotastellar.com")]
        api_url: String,

        /// API key for authentication
        #[arg(long, env = "ROTASTELLAR_API_KEY")]
        api_key: Option<String>,

        /// Deployment ID to report events against
        #[arg(long)]
        deployment_id: Option<String>,
    },

    /// Start the agent in poll mode (long-running daemon)
    Run {
        /// Agent ID (usually the satellite NORAD ID)
        #[arg(long, env = "ROTASTELLAR_AGENT_ID")]
        agent_id: String,

        /// Console API URL
        #[arg(long, default_value = "https://console.rotastellar.com")]
        api_url: String,

        /// API key for authentication
        #[arg(long, env = "ROTASTELLAR_API_KEY")]
        api_key: String,

        /// Poll interval in seconds
        #[arg(long, default_value = "30")]
        poll_interval: u64,

        /// Simulation speed (for simulated workloads)
        #[arg(long, default_value = "100")]
        speed: f64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Simulate {
            plan,
            speed,
            api_url,
            api_key,
            deployment_id,
        } => {
            let plan_json = std::fs::read_to_string(&plan)?;
            let workload: WorkloadSpec = serde_json::from_str(&plan_json)?;

            let config = AgentConfig {
                agent_id: "simulated".into(),
                api_url,
                api_key: api_key.unwrap_or_default(),
                poll_interval_s: 30,
            };

            let agent = SimulatedSatellite::new(config, speed)?;

            // Override deployment_id if provided
            let mut workload = workload;
            if let Some(did) = deployment_id {
                workload.deployment_id = did;
            }

            agent.execute(&workload).await?;
            println!("Simulation complete.");
        }

        Commands::Run {
            agent_id,
            api_url,
            api_key,
            poll_interval,
            speed,
        } => {
            let config = AgentConfig {
                agent_id,
                api_url,
                api_key,
                poll_interval_s: poll_interval,
            };

            let agent = SimulatedSatellite::new(config, speed)?;
            agent.start().await?;
        }
    }

    Ok(())
}
