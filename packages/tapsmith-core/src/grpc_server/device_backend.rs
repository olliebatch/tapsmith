//! Platform-specific device work behind one interface.
//!
//! RPC handlers resolve the active device's backend with
//! [`TapsmithServiceImpl::backend`] and call it, instead of matching on
//! [`Platform`] themselves. Adding a platform means adding a backend here,
//! not a branch in every handler.
//!
//! Backends borrow the service (like `ServiceResetOps`) so they can reuse its
//! agent connection, device lookup and error-screenshot helpers.

use tonic::{Response, Status};

use super::android_backend::AndroidBackend;
use super::ios_backend::IosBackend;
use super::TapsmithServiceImpl;
use crate::platform::Platform;
use crate::proto;

pub(super) type ActionResult = Result<Response<proto::ActionResponse>, Status>;

/// Whether a permission call grants or revokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PermissionChange {
    Grant,
    Revoke,
}

impl PermissionChange {
    /// The verb used in `pm grant|revoke` and in user-facing messages.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            PermissionChange::Grant => "grant",
            PermissionChange::Revoke => "revoke",
        }
    }
}

/// Device operations whose implementation depends on the platform.
///
/// Methods that act return an `ActionResponse` (failures carry an error
/// screenshot); methods that read return the value, and the handler wraps it.
#[async_trait::async_trait]
pub(super) trait DeviceBackend: Send + Sync {
    // ─── App ───
    async fn terminate_app(&self, request_id: String, package: &str) -> ActionResult;
    /// One of `not_installed`, `foreground`, `background` or `stopped`.
    async fn app_state(&self, package: &str) -> Result<String, Status>;
    /// The foreground app's package, or empty when unknown.
    async fn current_package(&self) -> Result<String, Status>;
    /// The foreground activity, or empty where the platform has none.
    async fn current_activity(&self) -> Result<String, Status>;
    async fn change_permission(
        &self,
        request_id: String,
        package: &str,
        permission: &str,
        change: PermissionChange,
    ) -> ActionResult;

    // ─── Device state ───
    async fn set_clipboard(&self, request_id: String, text: String) -> ActionResult;
    async fn get_clipboard(&self) -> Result<String, Status>;
    async fn set_orientation(&self, request_id: String, orientation: &str) -> ActionResult;
    /// `portrait` or `landscape`.
    async fn get_orientation(&self) -> Result<String, Status>;
    async fn set_color_scheme(&self, request_id: String, scheme: &str) -> ActionResult;
    /// `dark` or `light`.
    async fn get_color_scheme(&self) -> Result<String, Status>;
    async fn is_keyboard_shown(&self) -> Result<bool, Status>;
    async fn hide_keyboard(&self, request_id: String) -> ActionResult;

    // ─── System UI ───
    async fn open_notifications(&self, request_id: String) -> ActionResult;
    async fn open_quick_settings(&self, request_id: String) -> ActionResult;
    async fn wake(&self, request_id: String) -> ActionResult;
    async fn unlock(&self, request_id: String) -> ActionResult;
}

/// The backend for `platform`, borrowing `svc` for the call.
pub(super) fn for_platform(
    svc: &TapsmithServiceImpl,
    platform: Platform,
) -> Box<dyn DeviceBackend + '_> {
    match platform {
        Platform::Android => Box::new(AndroidBackend { svc }),
        Platform::Ios => Box::new(IosBackend { svc }),
    }
}
