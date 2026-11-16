use super::{Arc, Deployment, PayoutState, Serialize, MSG_DEPLOYMENT_MISMATCH};

pub fn load_deployment(params_path: &str, roster_path: Option<&str>) -> Result<Deployment, String> {
    let params = std::fs::read_to_string(params_path)
        .map_err(|e| format!("Could not read the bridge parameters file {params_path}: {e}"))?;
    let roster = match roster_path.filter(|p| !p.trim().is_empty()) {
        Some(p) => Some(
            std::fs::read_to_string(p)
                .map_err(|e| format!("Could not read the signer roster file {p}: {e}"))?,
        ),
        None => None,
    };
    Deployment::load(&params, roster.as_deref()).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDto {
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentDto {
    pub network: String,
    pub threshold: usize,
    pub admins: Vec<AdminDto>,
}

pub(super) fn admin_label(dep: &Deployment, i: usize) -> String {
    dep.signers
        .as_ref()
        .and_then(|s| s.get(i))
        .map(|s| s.label.clone())
        .unwrap_or_else(|| format!("Admin {}", i + 1))
}

pub(super) fn deployment_dto(dep: &Deployment) -> DeploymentDto {
    DeploymentDto {
        network: dep.network.to_core_arg().to_string(),
        threshold: dep.threshold,
        admins: (0..dep.admin_keys.len())
            .map(|i| AdminDto {
                label: admin_label(dep, i),
            })
            .collect(),
    }
}

/// After sign-in: the coordinator must use the same params.toml (same admins, threshold and
/// covenants), or nothing it stores could be signed safely.
pub(super) fn check_coordinator_deployment(
    dep: &Deployment,
    cfg: &crate::infrastructure::payout_coordinator::CoordinatorConfig,
) -> Result<(), String> {
    if cfg.deployment_fingerprint != dep.fingerprint() {
        return Err(MSG_DEPLOYMENT_MISMATCH.into());
    }
    Ok(())
}

impl PayoutState {
    /// (Re)loads the deployment; drops the admin identity and session if it changed.
    pub async fn load(&self, dep: Deployment) -> DeploymentDto {
        let dto = deployment_dto(&dep);
        let mut g = self.inner.lock().await;
        let changed = g
            .deployment
            .as_ref()
            .is_none_or(|d| d.fingerprint() != dep.fingerprint() || d.signers != dep.signers);
        if changed {
            g.me = None;
            g.app = None;
            g.challenge = None;
            g.coordinator = None;
        }
        g.deployment = Some(Arc::new(dep));
        dto
    }

    pub async fn deployment(&self) -> Result<Arc<Deployment>, String> {
        self.inner
            .lock()
            .await
            .deployment
            .clone()
            .ok_or_else(|| "Set the bridge parameters file in Node settings first.".to_string())
    }
}
