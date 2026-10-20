//! Android device work: ADB on the host plus the UIAutomator2 agent.

use super::device_backend::{ActionResult, DeviceBackend, PermissionChange};
use super::*;

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

    async fn app_is_running(&self, package: &str) -> Option<bool> {
        // `pidof` exits non-zero when nothing matches, which adb::shell
        // surfaces as an error — `|| true` makes "no process" read as
        // not running rather than unknown.
        let serial = self.svc.active_serial().await.ok()?;
        let out = adb::shell(&serial, &format!("pidof {package} 2>/dev/null || true"))
            .await
            .ok()?;
        Some(!out.trim().is_empty())
    }

    async fn relaunch_app(
        &self,
        serial: &str,
        package_name: &str,
        wait_for_idle: bool,
        idle_timeout_ms: u64,
    ) -> Result<(), String> {
        self.svc
            .android_clean_relaunch(serial, package_name, wait_for_idle, idle_timeout_ms)
            .await
    }

    async fn wipe_app_data(&self, serial: &str, package_name: &str) -> Result<(), String> {
        let output = adb::shell(serial, &format!("pm clear {}", package_name))
            .await
            .map_err(|e| format!("pm clear failed: {e}"))?;
        if output.trim().starts_with("Success") {
            Ok(())
        } else {
            Err(format!(
                "pm clear did not report success: {}",
                output.trim()
            ))
        }
    }

    async fn deliver_deep_link(
        &self,
        request_id: String,
        uri: &str,
        _delivery: DeepLinkDelivery,
        ack_epoch_gt: Option<u64>,
        ack_boot_before: Option<String>,
        ack_nav_gt: Option<u64>,
    ) -> ActionResult {
        if uri.contains('\'') {
            return Err(Status::invalid_argument(
                "uri contains an invalid character: single quote (') is not allowed",
            ));
        }
        let serial = self.svc.active_serial().await?;
        let target_package = self
            .svc
            .started_agent_config
            .read()
            .await
            .as_ref()
            .map(|c| c.target_package.clone())
            .filter(|p| !p.is_empty());

        let cmd = format!("am start -a android.intent.action.VIEW -d '{}'", uri);
        if let Err(e) = adb::shell(&serial, &cmd).await {
            let screenshot = self.svc.error_screenshot().await;
            return Ok(Response::new(proto::ActionResponse {
                request_id,
                success: false,
                error_type: "ADB_COMMAND_FAILED".to_string(),
                error_message: e.to_string(),
                screenshot,
            }));
        }

        // Settle: with an epoch ack, wait for the in-app hook to
        // confirm the reset ran; otherwise wait for the target app to
        // be the resumed activity. Neither compares whole hierarchies —
        // a link that lands on the screen already showing used to stall
        // here for the full 10s window and then re-fire the intent.
        let started = tokio::time::Instant::now();
        let mut refired = false;
        loop {
            tokio::time::sleep(ANDROID_DEEP_LINK_POLL).await;
            let elapsed = started.elapsed();

            let satisfied = match (ack_epoch_gt, ack_nav_gt) {
                (Some(before), _) => match self.svc.current_hooks_marker(5_000).await {
                    Some(marker)
                        if app_reset::hooks_acknowledged(
                            before,
                            ack_boot_before.as_deref(),
                            &marker,
                        ) =>
                    {
                        if let Some(err) = marker.err {
                            return Ok(self
                                .svc
                                .action_error(
                                    request_id,
                                    "ACTION_FAILED",
                                    format!("in-app reset reported an error: {err}"),
                                )
                                .await);
                        }
                        true
                    }
                    _ => false,
                },
                // Navigation link into an app with the nav counter:
                // the app bumping it is positive proof of delivery —
                // "resumed activity" can't tell a delivered same-screen
                // link from a dropped one.
                (None, Some(nav_before)) => match self.svc.current_hooks_marker(5_000).await {
                    Some(marker) => {
                        app_reset::nav_acknowledged(nav_before, ack_boot_before.as_deref(), &marker)
                    }
                    None => false,
                },
                (None, None) => match (&target_package, self.svc.get_current_component().await) {
                    (Some(target), Ok(Some((pkg, _)))) => &pkg == target,
                    // No target to judge against: the intent was
                    // accepted, that is all we can know.
                    (None, _) => true,
                    _ => false,
                },
            };
            if satisfied {
                break;
            }
            let deadline = if ack_epoch_gt.is_some() {
                ANDROID_HOOK_ACK_TIMEOUT
            } else {
                ANDROID_DEEP_LINK_SETTLE_TIMEOUT
            };
            if elapsed >= deadline {
                if ack_epoch_gt.is_some() {
                    return Ok(self
                        .svc
                        .action_error(
                            request_id,
                            "ACTION_FAILED",
                            format!(
                                "in-app reset did not acknowledge within {}ms",
                                deadline.as_millis()
                            ),
                        )
                        .await);
                }
                // Navigation links are best-effort here, as before: the
                // caller's own assertions decide whether it landed.
                break;
            }
            if !refired && elapsed >= ANDROID_DEEP_LINK_REFIRE_AFTER {
                refired = true;
                debug!("Deep link: target not resumed yet, re-firing intent once");
                let _ = adb::shell(&serial, &cmd).await;
            }
        }

        // Final idle wait so animations finish before the caller
        // interacts with the new screen.
        let idle_cmd = AgentCommand::WaitForIdle {
            timeout_ms: Some(ANDROID_DEEP_LINK_IDLE_TIMEOUT_MS),
        };
        let _ = self
            .svc
            .send_agent_command_with_timeout(&idle_cmd, ANDROID_DEEP_LINK_IDLE_TIMEOUT_MS)
            .await;

        Ok(TapsmithServiceImpl::success_action_response(request_id))
    }

    async fn launch_app(
        &self,
        request_id: String,
        serial: String,
        req: &proto::LaunchAppRequest,
    ) -> ActionResult {
        if !req.activity.is_empty() {
            TapsmithServiceImpl::validate_activity(&req.activity)?;
        }

        // Clear data first if requested
        if req.clear_data {
            match adb::shell(&serial, &format!("pm clear {}", req.package_name)).await {
                Ok(output) if !output.trim().starts_with("Success") => {
                    error!(output = %output.trim(), "pm clear did not report success");
                    let screenshot = self.svc.error_screenshot().await;
                    return Ok(Response::new(proto::ActionResponse {
                        request_id,
                        success: false,
                        error_type: "CLEAR_DATA_FAILED".to_string(),
                        error_message: format!("Failed to clear app data: {}", output.trim()),
                        screenshot,
                    }));
                }
                Err(e) => {
                    error!(error = %e, "Failed to clear app data before launch");
                    let screenshot = self.svc.error_screenshot().await;
                    return Ok(Response::new(proto::ActionResponse {
                        request_id,
                        success: false,
                        error_type: "CLEAR_DATA_FAILED".to_string(),
                        error_message: format!("Failed to clear app data: {e}"),
                        screenshot,
                    }));
                }
                Ok(_) => {} // Success
            }
        }

        if req.activity.is_empty() {
            self.svc
                .launch_package(
                    &serial,
                    request_id,
                    &req.package_name,
                    req.wait_for_idle,
                    req.idle_timeout_ms,
                )
                .await
        } else {
            // -S force-stops the app before launching, ensuring no
            // residual savedInstanceState Bundle survives from a
            // previous Activity. Without it, React Navigation /
            // Expo Router can restore stale navigation state from
            // the Bundle even after pm clear wiped AsyncStorage.
            // --activity-clear-task ensures a fresh task stack.
            let cmd = format!(
                "am start -S --activity-clear-task -n {}/{}",
                req.package_name, req.activity,
            );
            self.svc
                .launch_and_idle(
                    &serial,
                    request_id,
                    &cmd,
                    req.wait_for_idle,
                    req.idle_timeout_ms,
                )
                .await
        }
    }

    async fn restart_app(
        &self,
        request_id: String,
        serial: String,
        req: &proto::RestartAppRequest,
    ) -> ActionResult {
        // Force-stop the app to kill the process and reset all in-memory state.
        if let Err(e) = adb::shell(&serial, &format!("am force-stop {}", req.package_name)).await {
            error!(error = %e, "Failed to force-stop app");
            let screenshot = self.svc.error_screenshot().await;
            return Ok(Response::new(proto::ActionResponse {
                request_id,
                success: false,
                error_type: "FORCE_STOP_FAILED".to_string(),
                error_message: format!("Failed to stop app: {e}"),
                screenshot,
            }));
        }

        self.svc
            .launch_package(
                &serial,
                request_id,
                &req.package_name,
                req.wait_for_idle,
                req.idle_timeout_ms,
            )
            .await
    }

    async fn clear_app_data(
        &self,
        request_id: String,
        req: &proto::ClearAppDataRequest,
    ) -> ActionResult {
        let cmd = format!("pm clear {}", req.package_name);
        self.svc.adb_action(request_id, &cmd).await
    }

    async fn supports_cold_deep_link(&self) -> bool {
        false
    }
}
