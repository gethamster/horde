use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AutomaticDelivery {
    pub enabled: bool,
    /// Absolute, operator-selected GitHub CLI executable; project delivery.program is ignored.
    pub gh_program: String,
    pub repositories: Vec<String>,
    pub bases: Vec<String>,
    pub environments: Vec<String>,
    pub deploy_workflows: Vec<String>,
    /// Every changed path must be below one of these repository-relative prefixes.
    pub allowed_paths: Vec<String>,
    pub required_checks: Vec<String>,
    pub minimum_approvals: usize,
    pub require_independent_review: bool,
    /// An endpoint returning JSON {"commit":"<merge SHA>","environment":"<name>"}.
    pub version_url: Option<String>,
    /// App-specific smoke endpoint returning JSON {"ok":true,"commit":...,"environment":...}.
    pub smoke_url: Option<String>,
    /// ID of an immutable held-out evaluation record in the local store.
    pub qualification_id: String,
    pub minimum_routine_probability: f64,
    pub minimum_review_confidence: f64,
    /// Explicit Adam CLI tag/artifact release contract, separate from application deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_release: Option<PackageRelease>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageRelease {
    pub repository: String,
    pub workflow: String,
}
impl AutomaticDelivery {
    pub(super) fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if !std::path::Path::new(&self.gh_program).is_absolute()
            || self.repositories.is_empty()
            || self.bases.is_empty()
            || self.package_release.is_none()
                && (self.environments.is_empty() || self.deploy_workflows.is_empty())
            || self.allowed_paths.is_empty()
            || self.required_checks.is_empty()
            || self.minimum_approvals == 0
            || !self.require_independent_review
            || self.qualification_id.is_empty()
            || !(0.5..=1.0).contains(&self.minimum_routine_probability)
            || !(0.5..=1.0).contains(&self.minimum_review_confidence)
        {
            bail!(
                "automatic delivery requires explicit scope, checks, review, approvals, and qualification"
            );
        }
        if let Some(package) = &self.package_release
            && (package.repository != "gethamster/adam"
                || package.workflow != "release.yml"
                || self.repositories != [package.repository.clone()]
                || self.bases != ["main"]
                || self.version_url.is_some()
                || self.smoke_url.is_some())
        {
            bail!(
                "package release requires the explicit Adam main/tag workflow contract without application assertions"
            );
        }
        for value in self
            .repositories
            .iter()
            .chain(&self.bases)
            .chain(&self.environments)
            .chain(&self.deploy_workflows)
            .chain(&self.required_checks)
        {
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                bail!("automatic delivery contains an invalid identifier");
            }
        }
        for path in &self.allowed_paths {
            if crate::store::scope(path)? != *path || path == "." {
                bail!("automatic delivery allowed paths must be normalized repository prefixes");
            }
        }
        for url in [&self.version_url, &self.smoke_url].into_iter().flatten() {
            let parsed = reqwest::Url::parse(url)?;
            let loopback = parsed.scheme() == "http"
                && parsed
                    .host_str()
                    .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback());
            if (parsed.scheme() != "https" && !loopback)
                || parsed.host_str().is_none()
                || parsed.username() != ""
                || parsed.password().is_some()
            {
                bail!(
                    "automatic delivery assertion URLs require HTTPS or literal loopback HTTP without credentials"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package() -> AutomaticDelivery {
        AutomaticDelivery {
            enabled: true,
            gh_program: "/usr/bin/gh".into(),
            repositories: vec!["gethamster/adam".into()],
            bases: vec!["main".into()],
            allowed_paths: vec!["src".into()],
            required_checks: vec!["test".into()],
            minimum_approvals: 1,
            require_independent_review: true,
            qualification_id: "reviewed-qualification".into(),
            minimum_routine_probability: 0.8,
            minimum_review_confidence: 0.8,
            package_release: Some(PackageRelease {
                repository: "gethamster/adam".into(),
                workflow: "release.yml".into(),
            }),
            ..Default::default()
        }
    }
    #[test]
    fn immutable_package_contract_is_default_off_and_exactly_adam_main() {
        assert!(
            serde_json::to_value(AutomaticDelivery::default())
                .unwrap()
                .get("package_release")
                .is_none()
        );
        package().validate().unwrap();
        let mut changed = package();
        changed.repositories.push("other/repository".into());
        assert!(changed.validate().is_err());
        let mut changed = package();
        changed.bases = vec!["release".into()];
        assert!(changed.validate().is_err());
        changed = package();
        changed.package_release.as_mut().unwrap().workflow = "deploy.yml".into();
        assert!(changed.validate().is_err());
        changed = package();
        changed.version_url = Some("https://example.com".into());
        assert!(changed.validate().is_err());
        changed = package();
        changed.minimum_approvals = 0;
        assert!(changed.validate().is_err());
    }
}
