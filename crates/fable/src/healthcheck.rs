//! `fable healthcheck`: probe `/healthz` without needing curl (spec §2 D-5).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::process::ExitCode;

use fable_core::BootstrapConfig;

use crate::cli::HealthcheckArgs;

/// The URL probed by default: `/healthz` on the bind port, via loopback when
/// bound to all interfaces.
pub fn default_url(bind: SocketAddr) -> String {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}/healthz", SocketAddr::new(ip, bind.port()))
}

pub async fn healthcheck(config: &BootstrapConfig, args: &HealthcheckArgs) -> ExitCode {
    let url = args
        .url
        .clone()
        .unwrap_or_else(|| default_url(config.http.bind));
    let result = async {
        let client = reqwest::Client::builder().timeout(args.timeout()).build()?;
        client.get(&url).send().await?.error_for_status()
    }
    .await;
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("unhealthy: {url}: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url_uses_loopback_for_unspecified_bind() {
        assert_eq!(
            default_url("0.0.0.0:8000".parse().unwrap()),
            "http://127.0.0.1:8000/healthz"
        );
        assert_eq!(
            default_url("[::]:8080".parse().unwrap()),
            "http://[::1]:8080/healthz"
        );
        assert_eq!(
            default_url("10.1.2.3:9000".parse().unwrap()),
            "http://10.1.2.3:9000/healthz"
        );
    }
}
