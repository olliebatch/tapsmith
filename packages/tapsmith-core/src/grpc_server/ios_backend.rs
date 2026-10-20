//! iOS device work: `simctl` for simulators plus the XCUITest agent, which is
//! the only path on physical devices.

use std::time::Duration;

use tonic::Status;
use tracing::{info, warn};

use super::device_backend::{ActionResult, DeviceBackend, PermissionChange};
use super::TapsmithServiceImpl;
use crate::agent_comms::AgentCommand;
use crate::ios;

pub(super) struct IosBackend<'a> {
    pub(super) svc: &'a TapsmithServiceImpl,
}

#[async_trait::async_trait]
impl DeviceBackend for IosBackend<'_> {
    async fn terminate_app(&self, request_id: String, package: &str) -> ActionResult {
        // Terminate through the XCUITest agent so the runner stays in
        // sync with app state. This prevents the cascading fallback
        // chain in reset_ios_app when the next test calls restartApp.
        // Fall back to simctl terminate if the agent is unreachable.
        let agent_result = self
            .svc
            .send_agent_command_with_timeout(
                &AgentCommand::TerminateApp {
                    package: package.to_string(),
                },
                4_000,
            )
            .await;

        // Fall back to simctl on transport error AND on agent-reported
        // failure (Ok(resp) where !resp.success). The agent can return
        // a structured failure (e.g. app already gone, XCUI error) that
        // would otherwise be silently swallowed. Physical devices have
        // no simctl fallback — if the agent fails, the app state is
        // indeterminate and we surface the error rather than pretending
        // it worked.
        let needs_fallback = match &agent_result {
            Err(_) => true,
            Ok(resp) => !resp.success,
        };
        if needs_fallback {
            if self.svc.is_active_ios_physical().await {
                let msg = match &agent_result {
                    Err(status) => status.message().to_string(),
                    Ok(resp) => resp
                        .error
                        .clone()
                        .unwrap_or_else(|| "terminate_app agent path failed".to_string()),
                };
                return Ok(self
                    .svc
                    .action_error(request_id, "TERMINATE_FAILED", msg)
                    .await);
            }
            let serial = self.svc.active_serial().await?;
            let _ = ios::device::terminate_app(&serial, package).await;
        }

        Ok(TapsmithServiceImpl::success_action_response(request_id))
    }

    async fn app_state(&self, package: &str) -> Result<String, Status> {
        // Route through the XCUITest agent which can query app state
        let command = AgentCommand::GetAppState {
            package: package.to_string(),
        };
        let result = self.svc.send_agent_command(&command).await?;
        Ok(result
            .data
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("stopped")
            .to_string())
    }

    async fn current_package(&self) -> Result<String, Status> {
        // On iOS, return the target package from agent config
        Ok(self
            .svc
            .ios_agent_config
            .read()
            .await
            .as_ref()
            .map(|c| c.target_package.clone())
            .unwrap_or_default())
    }

    async fn current_activity(&self) -> Result<String, Status> {
        // Android-only concept — iOS doesn't have activities
        Ok(String::new())
    }

    async fn change_permission(
        &self,
        request_id: String,
        package: &str,
        permission: &str,
        change: PermissionChange,
    ) -> ActionResult {
        if self.svc.is_active_ios_physical().await {
            // `xcrun simctl privacy` only targets simulators. Physical
            // devices require user interaction (or MDM) to change
            // permission state. Surface a clear error with a workaround
            // pointing to the in-app permission dialog + UIInterruptionMonitor.
            let action = change.as_str();
            return Ok(self
                .svc
                .action_error(
                    request_id,
                    "UNSUPPORTED_ON_PHYSICAL_DEVICE",
                    format!(
                        "device.{action}Permission is not supported on physical iOS devices. \
                         Workaround: trigger the in-app permission dialog during the test and \
                         let the XCUITest UIInterruptionMonitor tap through it automatically."
                    ),
                )
                .await);
        }
        let serial = self.svc.active_serial().await?;
        let result = match change {
            PermissionChange::Grant => {
                ios::device::grant_permission(&serial, package, permission).await
            }
            PermissionChange::Revoke => {
                ios::device::revoke_permission(&serial, package, permission).await
            }
        };
        match result {
            Ok(()) => Ok(TapsmithServiceImpl::success_action_response(request_id)),
            Err(e) => Ok(self
                .svc
                .action_error(request_id, "ACTION_FAILED", e.to_string())
                .await),
        }
    }

    async fn set_clipboard(&self, request_id: String, text: String) -> ActionResult {
        if self.svc.is_active_ios_physical().await {
            // simctl pbcopy is simulator-only. Physical devices go
            // through the XCUITest agent, which calls
            // `UIPasteboard.general.string = ...`. Writes don't
            // trigger the iOS 16+ paste prompt — only reads do, and
            // even then the runner bundle bypasses the dialog since
            // it isn't a foreground app.
            let command = AgentCommand::SetClipboard { text };
            let result = self.svc.send_agent_command(&command).await;
            return self.svc.make_action_response(request_id, result).await;
        }
        // Use simctl pbcopy to avoid the iOS 16+ paste permission dialog
        // that would crash the XCUITest agent if it accessed UIPasteboard.
        //
        // pbcopy rides the sim's pasteboard-sync service, which wedges
        // intermittently on CI — retry briefly, then fall back to the
        // agent's `UIPasteboard.general.string = ...` (the physical-
        // device path above): clipboard WRITES don't trigger the iOS
        // 16+ paste prompt, so the fallback is prompt-safe on
        // simulators too.
        let serial = self.svc.active_serial().await?;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            match ios::device::set_clipboard(&serial, &text).await {
                Ok(()) => return Ok(TapsmithServiceImpl::success_action_response(request_id)),
                Err(e) => {
                    warn!(%serial, attempt, error = %e, "simctl pbcopy failed");
                    last_err = Some(e);
                }
            }
        }
        let pbcopy_err = last_err.expect("loop ran at least once");
        info!(%serial, "Falling back to agent UIPasteboard write after pbcopy failures");
        let command = AgentCommand::SetClipboard { text };
        let result = self.svc.send_agent_command(&command).await.map_err(|agent_err| {
            Status::internal(format!(
                "Failed to set clipboard on {serial}: {pbcopy_err} (agent fallback also failed: {agent_err})"
            ))
        })?;
        self.svc.make_action_response(request_id, Ok(result)).await
    }

    async fn get_clipboard(&self) -> Result<String, Status> {
        if self.svc.is_active_ios_physical().await {
            // Route through the XCUITest agent — the runner bundle
            // can call `UIPasteboard.general.string` without hitting
            // the iOS 16+ paste prompt since it isn't a foreground
            // app. On simulators we keep the simctl pbpaste path to
            // match the existing behaviour (and avoid touching the
            // agent at all when we don't need to).
            let command = AgentCommand::GetClipboard {};
            let result = self
                .svc
                .send_agent_command(&command)
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
            return Ok(result
                .data
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string());
        }
        // Use simctl pbpaste to avoid the iOS 16+ paste permission dialog
        // that would crash the XCUITest agent if it accessed UIPasteboard.
        // Same bounded retry as set_clipboard — the sim's
        // pasteboard-sync service wedges intermittently on CI.
        let serial = self.svc.active_serial().await?;
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            match ios::device::get_clipboard(&serial).await {
                Ok(t) => return Ok(t),
                Err(e) => {
                    warn!(%serial, attempt, error = %e, "simctl pbpaste failed");
                    last_err = Some(e);
                }
            }
        }
        Err(Status::internal(
            last_err.expect("loop ran at least once").to_string(),
        ))
    }

    async fn set_orientation(&self, request_id: String, orientation: &str) -> ActionResult {
        let command = AgentCommand::SetOrientation {
            orientation: orientation.to_string(),
        };
        let result = self.svc.send_agent_command(&command).await;
        self.svc.make_action_response(request_id, result).await
    }

    async fn get_orientation(&self) -> Result<String, Status> {
        let command = AgentCommand::GetOrientation {};
        let result = self.svc.send_agent_command(&command).await?;
        Ok(result
            .data
            .get("orientation")
            .and_then(|v| v.as_str())
            .unwrap_or("portrait")
            .to_string())
    }

    async fn set_color_scheme(&self, request_id: String, scheme: &str) -> ActionResult {
        match scheme {
            "dark" | "light" => {}
            other => {
                return Err(Status::invalid_argument(format!(
                    "scheme must be 'dark' or 'light', got '{other}'"
                )));
            }
        }
        if self.svc.is_active_ios_physical().await {
            // `xcrun simctl ui appearance` is simulator-only. Physical
            // devices require the user to toggle light/dark mode in
            // Settings; there is no programmatic path.
            return Ok(self
                .svc
                .action_error(
                    request_id,
                    "UNSUPPORTED_ON_PHYSICAL_DEVICE",
                    "device.setColorScheme is not supported on physical iOS devices. \
                     Workaround: set the system appearance on the device manually \
                     (Settings → Display & Brightness) before running the test."
                        .to_string(),
                )
                .await);
        }
        let serial = self.svc.active_serial().await?;
        match ios::device::set_appearance(&serial, scheme).await {
            Ok(()) => Ok(TapsmithServiceImpl::success_action_response(request_id)),
            Err(e) => Ok(self
                .svc
                .action_error(request_id, "ACTION_FAILED", e.to_string())
                .await),
        }
    }

    async fn get_color_scheme(&self) -> Result<String, Status> {
        // Route through agent — it can read UITraitCollection.current
        let command = AgentCommand::GetColorScheme {};
        let result = self.svc.send_agent_command(&command).await?;
        Ok(result
            .data
            .get("scheme")
            .and_then(|v| v.as_str())
            .unwrap_or("light")
            .to_string())
    }

    async fn is_keyboard_shown(&self) -> Result<bool, Status> {
        let command = AgentCommand::IsKeyboardShown {};
        let result = self.svc.send_agent_command(&command).await?;
        Ok(result
            .data
            .get("shown")
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
    }

    async fn hide_keyboard(&self, request_id: String) -> ActionResult {
        let result = self
            .svc
            .send_agent_command(&AgentCommand::HideKeyboard {})
            .await;
        self.svc.make_action_response(request_id, result).await
    }

    async fn open_notifications(&self, request_id: String) -> ActionResult {
        // iOS has no notification shade — success no-op
        Ok(TapsmithServiceImpl::success_action_response(request_id))
    }

    async fn open_quick_settings(&self, request_id: String) -> ActionResult {
        // iOS has no quick settings — success no-op
        Ok(TapsmithServiceImpl::success_action_response(request_id))
    }

    async fn wake(&self, request_id: String) -> ActionResult {
        // iOS simulator is always awake
        Ok(TapsmithServiceImpl::success_action_response(request_id))
    }

    async fn unlock(&self, request_id: String) -> ActionResult {
        // iOS simulator has no lock screen
        Ok(TapsmithServiceImpl::success_action_response(request_id))
    }
}
