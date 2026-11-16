use super::Deployment;

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
