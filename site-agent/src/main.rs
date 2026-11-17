//! Starts the site agent. Settings come from environment variables; see
//! `deploy/site-agent.env.example` and `docs/site-setup.md`.

use std::sync::Arc;
use std::time::Duration;

use site_agent::central::HttpCentral;
use site_agent::config::Config;
use site_agent::control::{DeviceHints, NoHints, RadClient, RouterOsHints};
use site_agent::server::{router, AppState};
use site_agent::service::Agent;
use site_agent::spool::Spool;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("site-agent cannot start: {e}");
            std::process::exit(2);
        }
    };

    let hints: Arc<dyn DeviceHints> = match (&cfg.router_rest_url, &cfg.router_user, &cfg.router_pass) {
        (Some(u), Some(user), Some(pass)) => Arc::new(RouterOsHints::new(u, user, pass)),
        _ => Arc::new(NoHints),
    };
    let spool = Spool::open(&cfg.spool_path, 50_000).expect("open the offline queue");
    tracing::info!(queued = spool.len(), "offline queue loaded");

    let agent = Arc::new(Agent::new(
        Arc::new(HttpCentral::new(&cfg.central_url, &cfg.node_token)),
        Arc::new(RadClient { bin: cfg.radclient_bin.clone(), addr: cfg.coa_addr.clone(), secret: cfg.coa_secret.clone() }),
        hints,
        spool,
    ));

    // Background: retry the queue and send health numbers every 30 seconds.
    let bg = agent.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            bg.replay().await;
            bg.send_metrics(&[]).await;
        }
    });

    let app = router(AppState { agent, local_key: Arc::new(cfg.local_key.clone()) });
    let listener = tokio::net::TcpListener::bind(&cfg.listen).await.expect("bind the listen address");
    tracing::info!(listen = %cfg.listen, "site agent ready");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server runs");
}
