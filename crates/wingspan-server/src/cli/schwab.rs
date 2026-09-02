//! `wingspan schwab status` — is the token good, and for how much longer?
//!
//! Reads the token file and prints what it found — reads, and nothing else:
//! the token is produced and refreshed by other applications on the server and
//! wingspan never writes, moves or chmods it. Never prints a token, and the
//! account number is masked by its own `Display`.

use std::process::ExitCode;
use std::sync::Arc;

use wingspan_marketdata::clock::system_clock;
use wingspan_marketdata::Health;

use crate::config::Config;
use crate::providers;

pub async fn status(config: &Config) -> anyhow::Result<ExitCode> {
    let clock = system_clock();
    let providers = providers::build(config, &clock);
    let Some(client) = providers.schwab else {
        println!("Schwab is not configured.");
        for note in &providers.notes {
            println!("  {note}");
        }
        if config.schwab.token_path.is_none() {
            println!("  set [schwab] token_path in wingspan.toml (see docs/SCHWAB.md)");
        }
        return Ok(ExitCode::FAILURE);
    };
    let status = client.probe().await;
    print_status(&status);
    Ok(match status.health() {
        Health::Unavailable(_) => ExitCode::FAILURE,
        Health::Ready | Health::Degraded(_) => ExitCode::SUCCESS,
    })
}

fn print_status(status: &Arc<wingspan_marketdata::SchwabStatus>) {
    println!("Schwab");
    println!("  token file      {}", status.token_path.display());
    println!(
        "  format          {}",
        status
            .token_format
            .map_or("unknown", wingspan_marketdata::TokenFormat::label)
    );
    println!("  loaded          {}", yes_no(status.token_loaded));
    if status.token_world_readable {
        println!(
            "  permissions     readable beyond its owner — tighten it where \
             the file is produced"
        );
    } else if status.token_loaded {
        println!("  permissions     ok");
    }
    println!("  access          read-only (wingspan never writes this file)");
    println!("  refresh enabled {}", yes_no(status.refresh_enabled));
    if let Some(account) = &status.account_number {
        println!("  account         {account}");
    }
    if let Some(expires) = status.access_expires_at {
        println!("  access expires  {expires}");
    }
    if let Some(created) = status.refresh_created_at {
        println!(
            "  refresh token   created {created}{}",
            status
                .refresh_age_days
                .map_or_else(String::new, |d| format!(" ({d:.1} days old of 7)"))
        );
    }
    if let Some(warning) = &status.refresh_warning {
        println!("  ⚠ {warning}");
    }
    println!(
        "  budget          {} of {} request(s) used in the window",
        status.requests_used, status.budget_limit
    );
    if let Some(success) = status.last_success {
        println!("  last success    {success}");
    }
    if let Some(error) = &status.last_error {
        println!("  last error      {error}");
    }
    println!("  health          {}", describe(&status.health()));
}

fn describe(health: &Health) -> String {
    match health {
        Health::Ready => "ready".to_string(),
        Health::Degraded(reason) => format!("degraded — {reason}"),
        Health::Unavailable(reason) => format!("unavailable — {reason}"),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
