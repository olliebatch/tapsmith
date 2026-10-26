//! macOS device work: the host Mac, driven through the XCUITest agent built
//! as a macOS UI-test bundle.
//!
//! Not wired up yet: every operation fails loudly with "not supported on
//! macOS yet" (never a success for something that did not happen) until it
//! is implemented. Methods that only read or clean up answer "unknown" or do
//! nothing, since there is nothing to report or undo.

use super::device_backend::{
    ActionResult, CaptureRoute, CaptureStop, DeviceBackend, PermissionChange,
};
use super::*;

pub(super) struct MacOsBackend<'a> {
    #[allow(dead_code)] // Used once the agent launch lands.
    pub(super) svc: &'a TapsmithServiceImpl,
}

fn unsupported_message(operation: &str) -> String {
    format!("{operation} is not supported on macOS yet")
}

fn unsupported(operation: &str) -> Status {
    Status::unimplemented(unsupported_message(operation))
}

#[async_trait::async_trait]
impl DeviceBackend for MacOsBackend<'_> {
    // ─── App ───

    async fn terminate_app(&self, _request_id: String, _package: &str) -> ActionResult {
        Err(unsupported("Terminating the app"))
    }

    async fn app_state(&self, _package: &str) -> Result<String, Status> {
        Err(unsupported("Reading the app state"))
    }

    async fn current_package(&self) -> Result<String, Status> {
        Err(unsupported("Reading the foreground app"))
    }

    async fn current_activity(&self) -> Result<String, Status> {
        Err(unsupported("Reading the foreground activity"))
    }

    async fn change_permission(
        &self,
        _request_id: String,
        _package: &str,
        _permission: &str,
        _change: PermissionChange,
    ) -> ActionResult {
        Err(unsupported("Changing permissions"))
    }

    // ─── Device state ───

    async fn set_clipboard(&self, _request_id: String, _text: String) -> ActionResult {
        Err(unsupported("Setting the clipboard"))
    }

    async fn get_clipboard(&self) -> Result<String, Status> {
        Err(unsupported("Reading the clipboard"))
    }

    async fn set_orientation(&self, _request_id: String, _orientation: &str) -> ActionResult {
        Err(unsupported("Setting the orientation"))
    }

    async fn get_orientation(&self) -> Result<String, Status> {
        Err(unsupported("Reading the orientation"))
    }

    async fn set_color_scheme(&self, _request_id: String, _scheme: &str) -> ActionResult {
        Err(unsupported("Setting the color scheme"))
    }

    async fn get_color_scheme(&self) -> Result<String, Status> {
        Err(unsupported("Reading the color scheme"))
    }

    async fn is_keyboard_shown(&self) -> Result<bool, Status> {
        Err(unsupported("Reading the keyboard state"))
    }

    async fn hide_keyboard(&self, _request_id: String) -> ActionResult {
        Err(unsupported("Hiding the keyboard"))
    }

    // ─── System UI ───

    async fn open_notifications(&self, _request_id: String) -> ActionResult {
        Err(unsupported("Opening notifications"))
    }

    async fn open_quick_settings(&self, _request_id: String) -> ActionResult {
        Err(unsupported("Opening quick settings"))
    }

    async fn wake(&self, _request_id: String) -> ActionResult {
        Err(unsupported("Waking the device"))
    }

    async fn unlock(&self, _request_id: String) -> ActionResult {
        Err(unsupported("Unlocking the device"))
    }

    // ─── App lifecycle ───

    async fn app_is_running(&self, _package: &str) -> Option<bool> {
        None // Unknown.
    }

    async fn relaunch_app(
        &self,
        _serial: &str,
        _package_name: &str,
        _wait_for_idle: bool,
        _idle_timeout_ms: u64,
    ) -> Result<(), String> {
        Err(unsupported_message("Relaunching the app"))
    }

    async fn wipe_app_data(&self, _serial: &str, _package_name: &str) -> Result<(), String> {
        Err(unsupported_message("Clearing app data"))
    }

    async fn deliver_deep_link(
        &self,
        _request_id: String,
        _uri: &str,
        _delivery: DeepLinkDelivery,
        _ack_epoch_gt: Option<u64>,
        _ack_boot_before: Option<String>,
        _ack_nav_gt: Option<u64>,
    ) -> ActionResult {
        Err(unsupported("Opening deep links"))
    }

    async fn launch_app(
        &self,
        _request_id: String,
        _serial: String,
        _req: &proto::LaunchAppRequest,
    ) -> ActionResult {
        Err(unsupported("Launching the app"))
    }

    async fn restart_app(
        &self,
        _request_id: String,
        _serial: String,
        _req: &proto::RestartAppRequest,
    ) -> ActionResult {
        Err(unsupported("Restarting the app"))
    }

    async fn clear_app_data(
        &self,
        _request_id: String,
        _req: &proto::ClearAppDataRequest,
    ) -> ActionResult {
        Err(unsupported("Clearing app data"))
    }

    async fn supports_cold_deep_link(&self) -> bool {
        false
    }

    // ─── App state and web views ───

    async fn save_app_state(
        &self,
        _request_id: String,
        _serial: String,
        _req: &proto::SaveAppStateRequest,
    ) -> ActionResult {
        Err(unsupported("Saving app state"))
    }

    async fn restore_app_state(
        &self,
        _request_id: String,
        _serial: String,
        _req: &proto::RestoreAppStateRequest,
    ) -> ActionResult {
        Err(unsupported("Restoring app state"))
    }

    async fn list_web_views(
        &self,
        _request_id: String,
        _serial: String,
    ) -> Result<Response<proto::ListWebViewsResponse>, Status> {
        Err(unsupported("Web view testing"))
    }

    async fn forward_web_view_port(
        &self,
        _request_id: String,
        _serial: String,
        _req: proto::ForwardWebViewPortRequest,
    ) -> Result<Response<proto::ForwardWebViewPortResponse>, Status> {
        Err(unsupported("Web view testing"))
    }

    // ─── Device setup and capture ───

    async fn take_screenshot(
        &self,
        _request_id: String,
    ) -> Result<Response<proto::ScreenshotResponse>, Status> {
        Err(unsupported("Taking a screenshot"))
    }

    async fn prearm_network_tracing(&self, _req: &proto::SetDeviceRequest) {}

    // ─── Agent ───

    fn check_start_agent(
        &self,
        request_id: &str,
        _req: &proto::StartAgentRequest,
    ) -> Result<(), proto::ActionResponse> {
        Err(proto::ActionResponse {
            request_id: request_id.to_string(),
            success: false,
            error_type: "AGENT_START_FAILED".to_string(),
            error_message: unsupported_message("Starting the agent"),
            screenshot: Vec::new(),
        })
    }

    async fn reuse_agent(&self, _serial: &str, _req: &proto::StartAgentRequest) {}

    async fn launch_agent(
        &self,
        request_id: String,
        _serial: String,
        _req: &proto::StartAgentRequest,
    ) -> Result<(), proto::ActionResponse> {
        Err(proto::ActionResponse {
            request_id,
            success: false,
            error_type: "AGENT_START_FAILED".to_string(),
            error_message: unsupported_message("Starting the agent"),
            screenshot: Vec::new(),
        })
    }

    async fn connect_agent(
        &self,
        _agent: &mut AgentConnection,
        _serial: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!(unsupported_message("Connecting to the agent"))
    }

    async fn after_agent_connected(&self, _serial: &str, _target_package: &str) {}

    async fn agent_start_diagnostics(&self, _serial: &str) -> String {
        String::new()
    }

    // ─── Network capture ───

    async fn trust_capture_ca(
        &self,
        _request_id: String,
        _serial: String,
        _mitm_ca: &Arc<MitmAuthority>,
        _ca_pem_path: &str,
        _warnings: &mut Vec<String>,
        _tunnel_all_tls: &mut Option<String>,
    ) -> Result<(), CaptureStop> {
        Err(unsupported("Network capture").into())
    }

    async fn start_capture_proxy(
        &self,
        _request_id: String,
        _serial: String,
        _mitm_ca: &Arc<MitmAuthority>,
    ) -> Result<NetworkProxy, CaptureStop> {
        Err(unsupported("Network capture").into())
    }

    async fn check_capture_reachable(
        &self,
        _request_id: String,
        _host_port: u16,
    ) -> Result<(), CaptureStop> {
        Err(unsupported("Network capture").into())
    }

    async fn route_capture_traffic(
        &self,
        _request_id: String,
        _serial: String,
        _req: &proto::StartNetworkCaptureRequest,
        _proxy: &NetworkProxy,
        _mitm_ca: &Arc<MitmAuthority>,
        _host_port: u16,
        _http_ports: &[u16],
        _warnings: &mut Vec<String>,
    ) -> Result<CaptureRoute, CaptureStop> {
        Err(unsupported("Network capture").into())
    }

    // Capture never starts on macOS, so there is no route to release.
    async fn release_capture_route(
        &self,
        _serial: &str,
        _reverse_port: Option<u16>,
        _ca_cert_path: Option<String>,
        _used_iptables: bool,
    ) {
    }

    async fn release_capture_route_on_shutdown(
        &self,
        _serial: &str,
        _reverse_port: Option<u16>,
        _ca_cert_path: Option<String>,
        _used_iptables: bool,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_unimplemented_and_names_the_operation() {
        let status = unsupported("Clearing app data");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert_eq!(
            status.message(),
            "Clearing app data is not supported on macOS yet"
        );
    }
}
