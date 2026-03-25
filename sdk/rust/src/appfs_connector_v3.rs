use crate::appfs_connector_v2::{
    AppConnectorV2, ConnectorContextV2, ConnectorErrorV2, ConnectorInfoV2, FetchLivePageRequestV2,
    FetchLivePageResponseV2, FetchSnapshotChunkRequestV2, FetchSnapshotChunkResponseV2,
    HealthStatusV2, SnapshotMetaV2, SubmitActionRequestV2, SubmitActionResponseV2,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Frozen AppFS connector SDK surface version for v0.4.
pub const APPFS_CONNECTOR_SDK_V3_VERSION: &str = "0.4.0";

pub type ConnectorInfoV3 = ConnectorInfoV2;
pub type ConnectorContextV3 = ConnectorContextV2;
pub type ConnectorErrorV3 = ConnectorErrorV2;
pub type HealthStatusV3 = HealthStatusV2;
pub type SnapshotMetaV3 = SnapshotMetaV2;
pub type FetchSnapshotChunkRequestV3 = FetchSnapshotChunkRequestV2;
pub type FetchSnapshotChunkResponseV3 = FetchSnapshotChunkResponseV2;
pub type FetchLivePageRequestV3 = FetchLivePageRequestV2;
pub type FetchLivePageResponseV3 = FetchLivePageResponseV2;
pub type SubmitActionRequestV3 = SubmitActionRequestV2;
pub type SubmitActionResponseV3 = SubmitActionResponseV2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppStructureSyncReasonV3 {
    Initialize,
    EnterScope,
    Refresh,
    Recover,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAppStructureRequestV3 {
    pub app_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshAppStructureRequestV3 {
    pub app_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_revision: Option<String>,
    pub reason: AppStructureSyncReasonV3,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_action_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppStructureNodeKindV3 {
    Directory,
    ActionFile,
    SnapshotResource,
    LiveResource,
    StaticJsonResource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStructureNodeV3 {
    pub path: String,
    pub kind: AppStructureNodeKindV3,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_entry: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_content: Option<JsonValue>,
    #[serde(default)]
    pub mutable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStructureSnapshotV3 {
    pub app_id: String,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_scope: Option<String>,
    #[serde(default)]
    pub ownership_prefixes: Vec<String>,
    pub nodes: Vec<AppStructureNodeV3>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppStructureSyncResultV3 {
    Unchanged {
        app_id: String,
        revision: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_scope: Option<String>,
    },
    Snapshot {
        snapshot: AppStructureSnapshotV3,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetAppStructureResponseV3 {
    pub result: AppStructureSyncResultV3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshAppStructureResponseV3 {
    pub result: AppStructureSyncResultV3,
}

/// AppFS connector v0.4 structure-sync extension trait.
pub trait AppConnectorV3: AppConnectorV2 {
    fn get_app_structure(
        &mut self,
        request: GetAppStructureRequestV3,
        ctx: &ConnectorContextV3,
    ) -> std::result::Result<GetAppStructureResponseV3, ConnectorErrorV3>;

    fn refresh_app_structure(
        &mut self,
        request: RefreshAppStructureRequestV3,
        ctx: &ConnectorContextV3,
    ) -> std::result::Result<RefreshAppStructureResponseV3, ConnectorErrorV3>;
}
