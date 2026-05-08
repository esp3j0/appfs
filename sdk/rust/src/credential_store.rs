use crate::error::{Error, Result};
use crate::KvStore;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Safe model-visible credential states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorCredentialStatus {
    Missing,
    Ready,
    Expired,
    Failed,
}

/// Private credential record stored outside the AppFS mount tree.
///
/// The `credentials` payload may contain tokens or other secrets. Never render
/// this whole record into AppFS resources, events, skills, or session files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorCredentialRecord {
    pub profile_id: String,
    pub credential_status: ConnectorCredentialStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ready_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<JsonValue>,
}

/// Safe summary that can be exposed in AppFS event content or resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorCredentialSummary {
    pub credential_status: ConnectorCredentialStatus,
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ready_at: Option<String>,
}

impl ConnectorCredentialRecord {
    #[must_use]
    pub fn safe_summary(&self) -> ConnectorCredentialSummary {
        ConnectorCredentialSummary {
            credential_status: self.credential_status,
            profile_id: self.profile_id.clone(),
            upstream_user_id: self.upstream_user_id.clone(),
            login: self.login.clone(),
            display_name: self.display_name.clone(),
            last_ready_at: self.last_ready_at.clone(),
        }
    }
}

impl ConnectorCredentialSummary {
    /// Build a safe summary from connector-returned content by whitelisting
    /// fields. Unknown fields, including tokens or credential blobs, are dropped.
    #[must_use]
    pub fn from_connector_content(content: &JsonValue, fallback_profile_id: &str) -> Self {
        Self {
            credential_status: content
                .get("credential_status")
                .and_then(JsonValue::as_str)
                .and_then(parse_credential_status)
                .unwrap_or(ConnectorCredentialStatus::Ready),
            profile_id: fallback_profile_id.to_string(),
            upstream_user_id: optional_summary_string(content, "upstream_user_id"),
            login: optional_summary_string(content, "login"),
            display_name: optional_summary_string(content, "display_name"),
            last_ready_at: optional_summary_string(content, "last_ready_at"),
        }
    }
}

/// KV-backed connector credential state.
#[derive(Clone)]
pub struct ConnectorCredentialStore {
    connector_name: String,
    kv: KvStore,
}

impl ConnectorCredentialStore {
    pub async fn new(db_path: &str, connector_name: impl Into<String>) -> Result<Self> {
        let connector_name = validate_key_component("connector_name", connector_name.into())?;
        let kv = KvStore::new(db_path).await?;
        Ok(Self { connector_name, kv })
    }

    pub fn from_kv(kv: KvStore, connector_name: impl Into<String>) -> Result<Self> {
        let connector_name = validate_key_component("connector_name", connector_name.into())?;
        Ok(Self { connector_name, kv })
    }

    #[must_use]
    pub fn connector_name(&self) -> &str {
        &self.connector_name
    }

    pub fn key_for_profile(&self, profile_id: &str) -> Result<String> {
        connector_credentials_key(&self.connector_name, profile_id)
    }

    pub async fn put(&self, record: &ConnectorCredentialRecord) -> Result<()> {
        let key = self.key_for_profile(&record.profile_id)?;
        self.kv.set(&key, record).await
    }

    pub async fn get(&self, profile_id: &str) -> Result<Option<ConnectorCredentialRecord>> {
        let key = self.key_for_profile(profile_id)?;
        self.kv.get(&key).await
    }

    pub async fn summary(&self, profile_id: &str) -> Result<Option<ConnectorCredentialSummary>> {
        Ok(self
            .get(profile_id)
            .await?
            .map(|record| record.safe_summary()))
    }

    pub async fn delete(&self, profile_id: &str) -> Result<()> {
        let key = self.key_for_profile(profile_id)?;
        self.kv.delete(&key).await
    }
}

pub fn connector_credentials_key(connector_name: &str, profile_id: &str) -> Result<String> {
    let connector_name = validate_key_component("connector_name", connector_name)?;
    let profile_id = validate_key_component("profile_id", profile_id)?;
    Ok(format!(
        "connector:{connector_name}:profile:{profile_id}:credentials"
    ))
}

fn optional_summary_string(content: &JsonValue, field: &str) -> Option<String> {
    content
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_credential_status(value: &str) -> Option<ConnectorCredentialStatus> {
    match value {
        "missing" => Some(ConnectorCredentialStatus::Missing),
        "ready" => Some(ConnectorCredentialStatus::Ready),
        "expired" => Some(ConnectorCredentialStatus::Expired),
        "failed" => Some(ConnectorCredentialStatus::Failed),
        _ => None,
    }
}

fn validate_key_component(field_name: &str, value: impl AsRef<str>) -> Result<String> {
    let value = value.as_ref().trim();
    if value.is_empty() || value.contains(['\0', '\r', '\n']) {
        return Err(Error::Internal(format!(
            "{field_name} must be non-empty and cannot contain control separators"
        )));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        connector_credentials_key, ConnectorCredentialRecord, ConnectorCredentialStatus,
        ConnectorCredentialStore,
    };
    use serde_json::json;

    #[test]
    fn credential_key_uses_connector_profile_shape() {
        let key =
            connector_credentials_key("tinode-http", "tinode:default").expect("credential key");

        assert_eq!(
            key,
            "connector:tinode-http:profile:tinode:default:credentials"
        );
        assert!(connector_credentials_key("", "tinode:default").is_err());
        assert!(connector_credentials_key("tinode-http", "").is_err());
        assert!(connector_credentials_key("tinode\nhttp", "tinode:default").is_err());
    }

    #[test]
    fn safe_summary_drops_secret_credential_payload() {
        let record = ConnectorCredentialRecord {
            profile_id: "tinode:default".to_string(),
            credential_status: ConnectorCredentialStatus::Ready,
            upstream_user_id: Some("usr123".to_string()),
            login: Some("appfs_default".to_string()),
            display_name: Some("Default agent".to_string()),
            last_ready_at: Some("2026-05-06T00:00:00Z".to_string()),
            expires_at: Some("2026-05-07T00:00:00Z".to_string()),
            credentials: Some(json!({
                "token": "secret-token",
                "refresh_token": "secret-refresh"
            })),
        };

        let summary = serde_json::to_value(record.safe_summary()).expect("summary json");
        assert_eq!(summary["credential_status"], "ready");
        assert_eq!(summary["profile_id"], "tinode:default");
        assert_eq!(summary["upstream_user_id"], "usr123");
        assert!(summary.get("credentials").is_none());
        assert!(!summary.to_string().contains("secret-token"));
    }

    #[test]
    fn summary_from_connector_content_whitelists_fields() {
        let content = json!({
            "credential_status": "ready",
            "profile_id": "connector-returned-value",
            "upstream_user_id": "usr123",
            "login": "appfs_default",
            "token": "do-not-leak"
        });

        let summary =
            super::ConnectorCredentialSummary::from_connector_content(&content, "tinode:default");
        let summary_json = serde_json::to_value(summary).expect("summary json");
        assert_eq!(summary_json["profile_id"], "tinode:default");
        assert!(summary_json.get("token").is_none());
        assert!(!summary_json.to_string().contains("do-not-leak"));
    }

    #[tokio::test]
    async fn credential_store_round_trips_private_record_by_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("credentials.db");
        let store =
            ConnectorCredentialStore::new(db_path.to_str().expect("utf8 db path"), "tinode-http")
                .await
                .expect("credential store");
        let record = ConnectorCredentialRecord {
            profile_id: "tinode:default".to_string(),
            credential_status: ConnectorCredentialStatus::Ready,
            upstream_user_id: Some("usr123".to_string()),
            login: Some("appfs_default".to_string()),
            display_name: None,
            last_ready_at: Some("2026-05-06T00:00:00Z".to_string()),
            expires_at: None,
            credentials: Some(json!({"token": "secret-token"})),
        };

        store.put(&record).await.expect("put record");
        assert_eq!(
            store
                .get("tinode:default")
                .await
                .expect("get record")
                .as_ref(),
            Some(&record)
        );
        let summary = store
            .summary("tinode:default")
            .await
            .expect("summary")
            .expect("summary exists");
        assert_eq!(summary.profile_id, "tinode:default");

        store.delete("tinode:default").await.expect("delete");
        assert!(store
            .get("tinode:default")
            .await
            .expect("get after delete")
            .is_none());
    }
}
