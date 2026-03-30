use crate::appfs_connector_v2::{
    connector_error_codes_v2, ActionExecutionModeV2, ActionStreamingPlanV2, AuthStatusV2,
    ConnectorContextV2, ConnectorErrorV2, ConnectorInfoV2, ConnectorTransportV2,
    FetchLivePageRequestV2, FetchLivePageResponseV2, FetchSnapshotChunkRequestV2,
    FetchSnapshotChunkResponseV2, HealthStatusV2, LiveModeV2, LivePageInfoV2, SnapshotMetaV2,
    SnapshotRecordV2, SnapshotResumeV2, SubmitActionOutcomeV2, SubmitActionRequestV2,
    SubmitActionResponseV2,
};
use crate::appfs_connector_v3::{
    AppStructureNodeKindV3, AppStructureNodeV3, AppStructureSnapshotV3, AppStructureSyncReasonV3,
    AppStructureSyncResultV3, GetAppStructureRequestV3, GetAppStructureResponseV3,
    RefreshAppStructureRequestV3, RefreshAppStructureResponseV3,
};

/// Canonical AppFS connector SDK surface version after runtime closure cleanup.
pub const APPFS_CONNECTOR_SDK_VERSION: &str = "0.4.0";

pub mod connector_error_codes {
    pub use crate::appfs_connector_v2::connector_error_codes_v2::{
        AUTH_EXPIRED, CACHE_MISS_EXPAND_FAILED, CURSOR_EXPIRED, CURSOR_INVALID, INTERNAL,
        INVALID_ARGUMENT, INVALID_PAYLOAD, NOT_SUPPORTED, PERMISSION_DENIED, RATE_LIMITED,
        RESOURCE_EXHAUSTED, SNAPSHOT_TOO_LARGE, TIMEOUT, UPSTREAM_UNAVAILABLE,
    };
}

pub type ConnectorInfo = ConnectorInfoV2;
pub type ConnectorContext = ConnectorContextV2;
pub type ConnectorError = ConnectorErrorV2;
pub type ConnectorTransport = ConnectorTransportV2;
pub type AuthStatus = AuthStatusV2;
pub type HealthStatus = HealthStatusV2;
pub type SnapshotMeta = SnapshotMetaV2;
pub type SnapshotResume = SnapshotResumeV2;
pub type SnapshotRecord = SnapshotRecordV2;
pub type FetchSnapshotChunkRequest = FetchSnapshotChunkRequestV2;
pub type FetchSnapshotChunkResponse = FetchSnapshotChunkResponseV2;
pub type FetchLivePageRequest = FetchLivePageRequestV2;
pub type FetchLivePageResponse = FetchLivePageResponseV2;
pub type LiveMode = LiveModeV2;
pub type LivePageInfo = LivePageInfoV2;
pub type SubmitActionRequest = SubmitActionRequestV2;
pub type SubmitActionResponse = SubmitActionResponseV2;
pub type SubmitActionOutcome = SubmitActionOutcomeV2;
pub type ActionExecutionMode = ActionExecutionModeV2;
pub type ActionStreamingPlan = ActionStreamingPlanV2;
pub type GetAppStructureRequest = GetAppStructureRequestV3;
pub type GetAppStructureResponse = GetAppStructureResponseV3;
pub type RefreshAppStructureRequest = RefreshAppStructureRequestV3;
pub type RefreshAppStructureResponse = RefreshAppStructureResponseV3;
pub type AppStructureSyncReason = AppStructureSyncReasonV3;
pub type AppStructureSyncResult = AppStructureSyncResultV3;
pub type AppStructureSnapshot = AppStructureSnapshotV3;
pub type AppStructureNode = AppStructureNodeV3;
pub type AppStructureNodeKind = AppStructureNodeKindV3;

fn not_supported_structure_sync() -> ConnectorError {
    ConnectorError {
        code: connector_error_codes_v2::NOT_SUPPORTED.to_string(),
        message: "app structure sync is not supported by this connector".to_string(),
        retryable: false,
        details: None,
    }
}

/// Canonical AppFS connector trait used by the runtime and mount-side read-through.
///
/// The HTTP and gRPC bridge implementations may continue to speak their current V2/V3 wire
/// surfaces internally; adapters map those protocols into this unified SDK trait.
pub trait AppConnector: Send {
    fn connector_id(&self) -> std::result::Result<ConnectorInfo, ConnectorError>;

    fn health(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<HealthStatus, ConnectorError>;

    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: std::time::Duration,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SnapshotMeta, ConnectorError>;

    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<FetchSnapshotChunkResponse, ConnectorError>;

    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<FetchLivePageResponse, ConnectorError>;

    fn submit_action(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError>;

    fn get_app_structure(
        &mut self,
        _request: GetAppStructureRequest,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<GetAppStructureResponse, ConnectorError> {
        Err(not_supported_structure_sync())
    }

    fn refresh_app_structure(
        &mut self,
        _request: RefreshAppStructureRequest,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<RefreshAppStructureResponse, ConnectorError> {
        Err(not_supported_structure_sync())
    }
}
