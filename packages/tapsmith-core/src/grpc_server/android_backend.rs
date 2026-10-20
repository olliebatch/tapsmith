//! Android device work: ADB on the host plus the UIAutomator2 agent.

use std::time::Duration;

use tonic::Status;
use tracing::error;

use super::device_backend::{ActionResult, DeviceBackend, PermissionChange};
use super::TapsmithServiceImpl;
use crate::adb;
use crate::agent_comms::AgentCommand;

pub(super) struct AndroidBackend<'a> {
    pub(super) svc: &'a TapsmithServiceImpl,
}

#[async_trait::async_trait]
impl DeviceBackend for AndroidBackend<'_> {
    async fn terminate_app(&self, request_id: String, package: &str) -> ActionResult {
        let cmd = format!("am force-stop {package}");
        self.svc.adb_action(request_id, &cmd).await
    }

    async fn app_state(&self, package: &str) -> Result<String, Status> {
        let serial = self.svc.active_serial().await?;

        // Check if app is installed
        let installed = adb::shell_lenient(&serial, &format!("pm list packages {package}"))
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        if !installed.contains(package) {
            return Ok("not_installed".to_string());
        }

        // Check if app process is running and in foreground
        let resumed = adb::shell_lenient(
            &serial,
            "dumpsys activity activities | grep -E 'mResumedActivity|ResumedActivity|topResumedActivity'",
        )
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

        if resumed.contains(package) {
            return Ok("foreground".to_string());
        }

        // Check if process exists at all
        let procs = adb::shell_lenient(&serial, &format!("pidof {package}"))
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let state = if procs.trim().is_empty() {
            "stopped"
        } else {
            "background"
        };
        Ok(state.to_string())
    }

    async fn current_package(&self) -> Result<String, Status> {
        Ok(self
            .svc
            .get_current_component()
            .await?
            .map(|(pkg, _)| pkg)
            .unwrap_or_default())
    }

    async fn current_activity(&self) -> Result<String, Status> {
        Ok(self
            .svc
            .get_current_component()
            .await?
            .map(|(_, act)| act)
            .unwrap_or_default())
    }

    async fn change_permission(
        &self,
        request_id: String,
        package: &str,
        permission: &str,
        change: PermissionChange,
    ) -> ActionResult {
        TapsmithServiceImpl::validate_permission(permission)?;
        let cmd = format!("pm {} {package} {permission}", change.as_str());
        self.svc.adb_action(request_id, &cmd).await
    }

    async fn set_clipboard(&self, request_id: String, text: String) -> ActionResult {
        // Use the on-device agent for clipboard operations since it has
        // access to Android's ClipboardManager via the instrumentation context.
        let command = AgentCommand::SetClipboard { text };
        let result = self.svc.send_agent_command(&command).await;
        self.svc.make_action_response(request_id, result).await
    }

    async fn get_clipboard(&self) -> Result<String, Status> {
        let command = AgentCommand::GetClipboard {};
        let result = self
            .svc
            .send_agent_command(&command)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(result
            .data
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    async fn set_orientation(&self, request_id: String, orientation: &str) -> ActionResult {
        let serial = self.svc.active_serial().await?;
        let rotation = match orientation {
            "portrait" => "0",
            "landscape" => "1",
            other => {
                return Err(Status::invalid_argument(format!(
                    "orientation must be 'portrait' or 'landscape', got '{other}'"
                )));
            }
        };
        if let Err(e) = adb::shell(&serial, "settings put system accelerometer_rotation 0").await {
            error!(error = %e, "Failed to disable auto-rotate");
        }
        let cmd = format!("settings put system user_rotation {rotation}");
        self.svc.adb_action(request_id, &cmd).await
    }

    async fn get_orientation(&self) -> Result<String, Status> {
        let serial = self.svc.active_serial().await?;
        let output = adb::shell(&serial, "settings get system user_rotation")
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let orientation = match output.trim() {
            "1" | "3" => "landscape",
            _ => "portrait",
        };
        Ok(orientation.to_string())
    }

    async fn set_color_scheme(&self, request_id: String, scheme: &str) -> ActionResult {
        let mode = match scheme {
            "dark" => "yes",
            "light" => "no",
            other => {
                return Err(Status::invalid_argument(format!(
                    "scheme must be 'dark' or 'light', got '{other}'"
                )));
            }
        };
        let cmd = format!("cmd uimode night {mode}");
        self.svc.adb_action(request_id, &cmd).await
    }

    async fn get_color_scheme(&self) -> Result<String, Status> {
        let serial = self.svc.active_serial().await?;
        let output = adb::shell_lenient(&serial, "cmd uimode night")
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let scheme = if output.contains("Night mode: yes") {
            "dark"
        } else {
            "light"
        };
        Ok(scheme.to_string())
    }

    async fn is_keyboard_shown(&self) -> Result<bool, Status> {
        let serial = self.svc.active_serial().await?;
        self.svc.android_is_keyboard_shown(&serial).await
    }

    async fn hide_keyboard(&self, request_id: String) -> ActionResult {
        // The Android soft keyboard is dismissed by BACK, not ESCAPE —
        // KEYCODE_ESCAPE is a no-op for the IME on most apps (incl.
        // Flutter). Only send BACK when the keyboard is actually shown,
        // otherwise BACK would pop the current screen / background the app.
        let serial = self.svc.active_serial().await?;
        let shown = self.svc.android_is_keyboard_shown(&serial).await?;
        if shown {
            self.svc
                .adb_action(request_id, "input keyevent KEYCODE_BACK")
                .await
        } else {
            Ok(TapsmithServiceImpl::success_action_response(request_id))
        }
    }

    async fn open_notifications(&self, request_id: String) -> ActionResult {
        self.svc
            .adb_action(request_id, "cmd statusbar expand-notifications")
            .await
    }

    async fn open_quick_settings(&self, request_id: String) -> ActionResult {
        self.svc
            .adb_action(request_id, "cmd statusbar expand-settings")
            .await
    }

    async fn wake(&self, request_id: String) -> ActionResult {
        self.svc
            .adb_action(request_id, "input keyevent KEYCODE_WAKEUP")
            .await
    }

    async fn unlock(&self, request_id: String) -> ActionResult {
        let serial = self.svc.active_serial().await?;
        // Wake the screen first
        let _ = adb::shell_lenient(&serial, "input keyevent KEYCODE_WAKEUP").await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Only dismiss a keyguard that is actually showing. The
        // legacy sequence below is a MENU key plus a swipe up, and
        // on an already-unlocked device both land on the foreground
        // app — the swipe scrolls its root list to the bottom. UI
        // mode unlocks right before running a file whose reset the
        // background preparation already satisfied, so nothing
        // undoes that scroll and the first assertions fail on a
        // "prepared" device (headless survived only because its
        // inline reset runs after the unlock).
        if adb::keyguard_showing(&serial).await == Some(false) {
            return Ok(TapsmithServiceImpl::success_action_response(request_id));
        }
        // `wm dismiss-keyguard` is the input-free path (API 26+). It
        // does nothing for secure keyguards, so re-check and fall
        // back to the gesture only while the lock screen is still up
        // (or when the dump cannot tell).
        let _ = adb::shell_lenient(&serial, "wm dismiss-keyguard").await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        if adb::keyguard_showing(&serial).await == Some(false) {
            return Ok(TapsmithServiceImpl::success_action_response(request_id));
        }
        // Dismiss non-secure lock screen (KEYCODE_MENU)
        let _ = adb::shell_lenient(&serial, "input keyevent 82").await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Swipe up as fallback for swipe-to-unlock screens
        let _ = adb::shell_lenient(&serial, "input swipe 540 1800 540 800 300").await;
        Ok(TapsmithServiceImpl::success_action_response(request_id))
    }
}
