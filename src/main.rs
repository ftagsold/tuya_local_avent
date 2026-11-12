mod bridge_json;
mod discover;
mod dp;
mod fetch_password;
mod ha;
mod lan_control;
mod login_check;
mod media;
mod offer_test;
mod onvif;
mod probe;
mod rtsp_gate;
mod run;
mod set_dp;
mod tuya;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "Philips Avent (Tuya) monitor bridge for Home Assistant and UniFi Protect")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the full bridge: video to UniFi Protect, control to Home Assistant.
    Run,
    /// Write one data point to the monitor and watch the pushes (control-write test).
    SetDp {
        #[arg(long, env = "AVENT_DEVICE_ID")]
        device_id: String,
        #[arg(long, env = "AVENT_LOCAL_KEY", hide_env_values = true)]
        local_key: String,
        #[arg(long)]
        ip: Option<String>,
        #[arg(long)]
        dp: u32,
        #[arg(long)]
        value: String,
        #[arg(long, default_value_t = 20)]
        listen_secs: u64,
    },
    /// Find the monitor, detect its protocol and prove the local key. Read-only.
    Probe {
        #[arg(long, env = "AVENT_DEVICE_ID")]
        device_id: Option<String>,
        #[arg(long, env = "AVENT_LOCAL_KEY", hide_env_values = true)]
        local_key: Option<String>,
        /// Skip discovery and connect to this address.
        #[arg(long)]
        ip: Option<String>,
        #[arg(long, default_value_t = 15)]
        scan_secs: u64,
        #[arg(long, default_value_t = 60)]
        listen_secs: u64,
    },
    /// One-time signed login that fetches the per-device P2P password (needs app keys).
    FetchPassword {
        #[arg(long, env = "AVENT_EMAIL")]
        email: String,
        #[arg(long, env = "AVENT_PASSWORD", hide_env_values = true)]
        password: String,
        /// Data center: eu | us | in | cn.
        #[arg(long, env = "AVENT_REGION", default_value = "eu")]
        region: String,
        /// Phone calling code for the account (e.g. 49, 39, 1).
        #[arg(long, env = "AVENT_COUNTRY_CODE", default_value = "39")]
        country_code: String,
        #[arg(long, env = "AVENT_DEVICE_ID")]
        device_id: String,
        #[arg(long, env = "AVENT_LOCAL_KEY", hide_env_values = true)]
        local_key: Option<String>,
        #[arg(long, env = "AVENT_APP_KEY", hide_env_values = true)]
        app_key: String,
        #[arg(long, env = "AVENT_SIGNING_KEY", hide_env_values = true)]
        signing_key: String,
        #[arg(long, env = "AVENT_CH_KEY", hide_env_values = true)]
        ch_key: String,
    },
    /// Check the Tuya Mobile API accepts the app signing values (RSA-token call
    /// only; no account password or MFA).
    TokenCheck {
        #[arg(long, env = "AVENT_EMAIL")]
        email: String,
        #[arg(long, env = "AVENT_REGION", default_value = "eu")]
        region: String,
        #[arg(long, env = "AVENT_COUNTRY_CODE", default_value = "49")]
        country_code: String,
        #[arg(long, env = "AVENT_APP_KEY", hide_env_values = true)]
        app_key: String,
        #[arg(long, env = "AVENT_SIGNING_KEY", hide_env_values = true)]
        signing_key: String,
        #[arg(long, env = "AVENT_CH_KEY", hide_env_values = true)]
        ch_key: String,
    },
    /// Send a receive-only LAN video offer on a 3.3 session; report the answer.
    OfferTest {
        #[arg(long, env = "AVENT_DEVICE_ID")]
        device_id: String,
        #[arg(long, env = "AVENT_LOCAL_KEY", hide_env_values = true)]
        local_key: String,
        /// Account uid (the offer's "from"), e.g. from login-check.
        #[arg(long, env = "AVENT_UID")]
        uid: String,
        #[arg(long)]
        ip: Option<String>,
    },
    /// Log in to Tuya Smart, list devices, show the stream config's fields (redacted).
    LoginCheck {
        #[arg(long, env = "AVENT_EMAIL")]
        email: String,
        #[arg(long, env = "AVENT_PASSWORD", hide_env_values = true)]
        password: String,
        #[arg(long, env = "AVENT_REGION", default_value = "eu-central")]
        region: String,
        #[arg(long, env = "AVENT_DEVICE_ID")]
        device_id: Option<String>,
        #[arg(long, env = "AVENT_LOCAL_KEY", hide_env_values = true)]
        local_key: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match Cli::parse().command {
        Command::Run => run::run(run::Env::from_env()?).await,
        Command::SetDp { device_id, local_key, ip, dp, value, listen_secs } => {
            set_dp::run(set_dp::Args { device_id, local_key, ip, dp, value, listen_secs }).await
        }
        Command::Probe { device_id, local_key, ip, scan_secs, listen_secs } => {
            probe::run(probe::Args { device_id, local_key, ip, scan_secs, listen_secs }).await
        }
        Command::FetchPassword { email, password, region, country_code, device_id, local_key, app_key, signing_key, ch_key } => {
            fetch_password::run(fetch_password::Args {
                email, password, region, country_code, device_id, local_key, app_key, signing_key, ch_key,
            })
            .await
        }
        Command::TokenCheck { email, region, country_code, app_key, signing_key, ch_key } => {
            let keys = tuya::mobile::AppKeys { signing_key, app_key, ch_key };
            let client = tuya::mobile::MobileClient::new(keys, &region, &country_code)?;
            match client.check_client(&email).await {
                Ok(t) => {
                    let keys = t.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default();
                    println!("client ACCEPTED. token fields: {keys}");
                    Ok(())
                }
                Err(e) => {
                    println!("client REJECTED: {e:#}");
                    Ok(())
                }
            }
        }
        Command::OfferTest { device_id, local_key, uid, ip } => {
            offer_test::run(offer_test::Args { device_id, local_key, uid, ip }).await
        }
        Command::LoginCheck { email, password, region, device_id, local_key } => {
            login_check::run(login_check::Args { email, password, region, device_id, local_key }).await
        }
    }
}
