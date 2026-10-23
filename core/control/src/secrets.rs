//! Secret environment variables. Values are stored encrypted (see
//! `aegis-secrets`) and injected into an app's processes when they start;
//! they are never written into release directories or events. A change
//! applies the next time the process starts or is restarted.

use crate::{ControlError, ControlPlane, ControlResult};
use aegis_secrets::SecretInfo;
use aegis_types::ProjectId;
use serde_json::json;

impl ControlPlane {
    fn require_project(&self, project_id: &ProjectId) -> ControlResult<()> {
        match self.inner.projection.get_project(project_id) {
            Some(_) => Ok(()),
            None => Err(ControlError::NotFound(format!(
                "Unknown project {}",
                project_id
            ))),
        }
    }

    /// A project's secret names (values are never returned).
    pub fn list_secrets(&self, project_id: &ProjectId) -> Vec<SecretInfo> {
        self.inner.secrets.list(&project_id.to_string())
    }

    /// Creates or replaces a secret. Returns true if it was new.
    pub async fn set_secret(
        &self,
        project_id: ProjectId,
        name: &str,
        value: &str,
    ) -> ControlResult<bool> {
        self.require_project(&project_id)?;
        let name = name.trim();
        aegis_secrets::validate_name(name)
            .and_then(|_| aegis_secrets::validate_value(value))
            .map_err(ControlError::InvalidArgument)?;
        let key = project_id.to_string();
        let created = !self.inner.secrets.list(&key).iter().any(|s| s.name == name);
        self.inner.secrets.set(&key, name, value).await?;
        // The value is never part of the event.
        self.record(
            "SecretUpdated",
            json!({"project_id": key, "name": name, "created": created}),
        )
        .await?;
        Ok(created)
    }

    pub async fn delete_secret(&self, project_id: ProjectId, name: &str) -> ControlResult<()> {
        self.require_project(&project_id)?;
        let key = project_id.to_string();
        if !self.inner.secrets.remove(&key, name).await? {
            return Err(ControlError::NotFound(format!(
                "No secret named '{}'",
                name
            )));
        }
        self.record("SecretDeleted", json!({"project_id": key, "name": name}))
            .await?;
        Ok(())
    }
}
