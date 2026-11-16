use anyhow::Context;

/// Reads the Payout Administrators from strata-bridge `params.toml`. The file is the trust root:
/// only its admin keys can sign in as Payout Administrator, and only its covenants' connectors are
/// accepted in payout transactions.
pub(crate) fn load_payout_deployment(
    config: &crate::config::Config,
) -> anyhow::Result<Option<payout_admin_core::Deployment>> {
    let Some(path) = &config.payout_admin_params_path else {
        tracing::info!("PAYOUT_ADMIN_PARAMS_PATH not set — Admin Block Payouts disabled");
        return Ok(None);
    };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read PAYOUT_ADMIN_PARAMS_PATH {path}"))?;
    let dep = payout_admin_core::Deployment::from_params_toml(&text)
        .with_context(|| format!("invalid strata-bridge params in {path}"))?;
    if dep.network != config.bitcoin_network {
        anyhow::bail!(
            "{path} is for {} but BITCOIN_NETWORK is {}",
            dep.network,
            config.bitcoin_network
        );
    }
    for w in &dep.warnings {
        tracing::warn!("payout params: {w}");
    }
    tracing::info!(
        admins = dep.admin_keys.len(),
        threshold = dep.threshold,
        covenants = dep.covenants.len(),
        fingerprint = %dep.fingerprint(),
        "Admin Block Payouts enabled"
    );
    Ok(Some(dep))
}
