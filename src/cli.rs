use std::{net::SocketAddr, path::PathBuf, time::Duration};

use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the HTTP server.
    Serve(ServeArgs),
    /// Exit 0 when the local server answers `/_sol/health` (for container healthchecks).
    Healthcheck(HealthcheckArgs),
    /// Print the OpenAPI document as JSON.
    Openapi,
}

#[derive(Debug, Clone, Args)]
pub struct ServeArgs {
    #[arg(long, env = "ORBIT_BIND", default_value = "0.0.0.0:8080")]
    pub bind: SocketAddr,
    #[arg(long, env = "ORBIT_DATA_DIR", default_value = "data")]
    pub data_dir: PathBuf,
    /// Where apps fetch Sol's signing keys. Sol itself ignores it.
    #[arg(long, env = "SOL_INTERNAL_URL", default_value = "http://sol:8080")]
    pub sol_url: String,
}

#[derive(Debug, Args)]
pub struct HealthcheckArgs {
    /// Same variable as `serve`, so the check follows the configured port.
    #[arg(long, env = "ORBIT_BIND", default_value = "0.0.0.0:8080")]
    pub bind: SocketAddr,
}

pub async fn healthcheck(args: &HealthcheckArgs) -> anyhow::Result<()> {
    let url = format!("http://127.0.0.1:{}/_sol/health", args.bind.port());
    let res = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(3))
        .send()
        .await?;
    anyhow::ensure!(res.status().is_success(), "{url} answered {}", res.status());
    Ok(())
}
