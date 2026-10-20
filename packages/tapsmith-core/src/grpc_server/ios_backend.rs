//! iOS device work: `simctl` for simulators plus the XCUITest agent, which is
//! the only path on physical devices.

use super::device_backend::{ActionResult, DeviceBackend, PermissionChange};
use super::*;

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

    async fn app_is_running(&self, package: &str) -> Option<bool> {
        let command = AgentCommand::GetAppState {
            package: package.to_string(),
        };
        let result = self.svc.send_agent_command(&command).await.ok()?;
        let state = result.data.get("state").and_then(|v| v.as_str())?;
        Some(!matches!(
            state,
            "stopped" | "not_running" | "notRunning" | "unknown"
        ))
    }

    async fn relaunch_app(
        &self,
        serial: &str,
        package_name: &str,
        wait_for_idle: bool,
        idle_timeout_ms: u64,
    ) -> Result<(), String> {
        self.svc
            .reset_ios_app(serial, package_name, wait_for_idle, idle_timeout_ms)
            .await
    }

    async fn wipe_app_data(&self, serial: &str, package_name: &str) -> Result<(), String> {
        if self.svc.is_active_ios_physical().await {
            let app_path = self
                .svc
                .ios_agent_config
                .read()
                .await
                .as_ref()
                .and_then(|c| c.app_path.clone())
                .ok_or_else(|| {
                    "clearing app data on a physical iOS device requires the app \
                             bundle path passed at startAgent time"
                        .to_string()
                })?;
            if let Err(e) = ios::device::uninstall_app_on_device(serial, package_name).await {
                warn!(error = %e, "Uninstall step of physical-iOS clear failed, continuing to reinstall");
            }
            ios::device::install_app_on_device(serial, &app_path)
                .await
                .map_err(|e| format!("reinstall failed: {e}"))?;
            tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
            return Ok(());
        }
        // Terminate first (as launch_app's own clear path does): a
        // running process flushes UserDefaults / caches on exit, which
        // would resurrect state deleted from under it.
        let _ = ios::device::terminate_app(serial, package_name).await;
        match self
            .svc
            .get_app_container_cached(serial, package_name)
            .await
        {
            Ok(ref container) => {
                if let Err(e) = ios::device::clear_container(container).await {
                    warn!(error = %e, "Failed to clear app container, continuing anyway");
                }
                if !ios::device::keychain_state_disabled() {
                    if let Some(keychain_dir) = ios::device::simulator_keychain_dir(container) {
                        if let Err(e) = ios::device::clear_keychain(serial, &keychain_dir).await {
                            warn!(error = %e, "Failed to clear simulator keychain, continuing anyway");
                        }
                    }
                }
            }
            Err(e) => {
                debug!(error = %e, "Could not get app container (app may not be installed)");
            }
        }
        Ok(())
    }

    async fn deliver_deep_link(
        &self,
        request_id: String,
        uri: &str,
        delivery: DeepLinkDelivery,
        ack_epoch_gt: Option<u64>,
        ack_boot_before: Option<String>,
        ack_nav_gt: Option<u64>,
    ) -> ActionResult {
        let bundle_id = self
            .svc
            .ios_agent_config
            .read()
            .await
            .as_ref()
            .map(|c| c.target_package.clone())
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                Status::failed_precondition(
                    "device.openDeepLink requires an active agent session \
                             with a target package. Call startAgent first.",
                )
            })?;
        let serial = self.svc.active_serial().await?;

        if self.svc.is_active_ios_physical().await {
            // Physical: use the agent's XCUIApplication.open(url:).
            let command = AgentCommand::OpenDeepLink {
                url: uri.to_string(),
                package: bundle_id,
                deliver_in_process: true,
                require_ui_change: false,
                ack_epoch_gt,
                ack_boot_before: ack_boot_before.clone(),
                ack_nav_gt,
            };
            let result = TapsmithServiceImpl::require_epoch_ack_echo(
                ack_epoch_gt,
                self.svc.send_agent_command(&command).await,
            );
            return self.svc.make_action_response(request_id, result).await;
        }

        // Simulator: warm-first, cold fallback.
        //
        // Warm attempt: if the app is already running with rendered
        // content, deliver in-process (the physical-device path:
        // activate + XCUIApplication.open(url:)) and require the UI
        // hierarchy to change from its pre-open state within a bounded
        // window. The hierarchy-change requirement is what makes warm
        // delivery verifiable — a warm app trivially "has rendered
        // content", so the cold verify's readiness check would mask a
        // dropped Linking event. This skips the terminate -> cold
        // relaunch cycle (~14-22s on CI) for the common case: per-test
        // soft resets and in-test navigation against a running app.
        //
        // The historical reasons for avoiding this path no longer
        // hold as written: the "open(url:) hangs on quiescence" finding
        // predates the QuiescenceDisabler swizzle (which the agent now
        // applies before delivery), and the "warm delivery doesn't
        // trigger RN navigation" finding was observed with warm
        // `simctl openurl`, not XCUIApplication.open(url:) — which
        // physical devices use for every deep link today. The fallback
        // below keeps behavior identical when warm delivery doesn't
        // land; the worst case is the status quo plus one bounded
        // warm window (IOS_OPEN_DEEP_LINK_WARM_TIMEOUT_MS).
        //
        // `force_cold_launch` skips the warm attempt entirely. The SDK
        // sets it for the between-file soft reset: a July 2026 local
        // soak of the full E2E suite showed that sessions running
        // all-warm for ~60+ deliveries accumulate native navigation
        // state that observably diverges from a cold launch (screen
        // a11y trees stop being flattened — strict-mode selectors that
        // matched one element start matching two — and element ids go
        // stale), while every file-sized warm window (~20-30
        // deliveries from a cold start) stayed green. Cold-launching
        // at file boundaries pins each file to the exact starting
        // state it had before warm delivery existed and bounds the
        // warm window to one file.
        if delivery != DeepLinkDelivery::Cold {
            let warm_command = AgentCommand::OpenDeepLink {
                url: uri.to_string(),
                package: bundle_id.clone(),
                deliver_in_process: true,
                // With an epoch or nav ack the in-app hook is the
                // verifier; otherwise fall back to the hierarchy-change
                // heuristic (older app builds without the nav counter).
                require_ui_change: ack_epoch_gt.is_none() && ack_nav_gt.is_none(),
                ack_epoch_gt,
                ack_boot_before: ack_boot_before.clone(),
                ack_nav_gt,
            };
            let warm_result = TapsmithServiceImpl::require_epoch_ack_echo(
                ack_epoch_gt,
                self.svc
                    .send_agent_command_with_timeout(
                        &warm_command,
                        IOS_OPEN_DEEP_LINK_WARM_TIMEOUT_MS,
                    )
                    .await,
            );
            let warm_only = delivery == DeepLinkDelivery::WarmOnly;
            let next = if warm_only {
                "reporting the miss to the caller"
            } else {
                "falling back to cold relaunch"
            };
            match &warm_result {
                Ok(resp) if resp.success => {
                    return self.svc.make_action_response(request_id, warm_result).await;
                }
                Ok(resp) => {
                    info!(
                        %serial, uri = %uri,
                        error = %resp.error.as_deref().unwrap_or("unknown"),
                        "warm in-process deep link did not land; {next}"
                    );
                }
                Err(status) => {
                    info!(
                        %serial, uri = %uri, error = %status.message(),
                        "warm in-process deep link errored; {next}"
                    );
                }
            }
            if warm_only {
                return self.svc.make_action_response(request_id, warm_result).await;
            }
        }

        // Cold fallback: terminate -> simctl openurl -> agent verify,
        // retried as a unit. The first cold, trust-gated openurl on a
        // fresh sim intermittently fails to foreground the app (the
        // "Open in <app>?" prompt races the launch and the app lands
        // back on SpringBoard). Re-delivering self-heals: a second
        // openurl is warm and already trusted, so it lands. We only
        // succeed once the agent confirms the app actually rendered
        // content (the verify returns false on a never-foregrounded app
        // rather than masking it).
        //
        // simctl openurl to a running app doesn't trigger navigation
        // (the React Native scene handler misses it), so we terminate
        // first — making openurl cold-launch the app with the URL.
        // Fresh simulators can show "Open in <app>?" and block simctl
        // itself, so the daemon taps that dialog while openurl is
        // still pending.
        let max_attempts = IOS_OPEN_DEEP_LINK_MAX_ATTEMPTS;
        let mut last_error = "openDeepLink: app did not reach destination".to_string();
        let mut verify_result: Option<Result<AgentResponse, Status>> = None;
        let mut attempt: u32 = 0;
        // One-shot escalation for the compositor outage: when the
        // agent reports the display is not rendering (black screen
        // with a healthy a11y tree), app relaunches provably don't
        // recover it — only recreating the whole render surface does.
        // Reboot the simulator, bring the agent back, and re-deliver.
        let mut rebooted_for_black_display = false;
        loop {
            attempt += 1;
            if last_error.contains("display is not rendering")
                && !rebooted_for_black_display
                && !self.svc.is_active_ios_physical().await
            {
                rebooted_for_black_display = true;
                warn!(
                    %serial, uri = %uri,
                    "display is not rendering and an app relaunch cannot recover a dead \
                     compositor; rebooting the simulator and restarting the agent"
                );
                let _ = ios::device::shutdown_simulator(&serial).await;
                if let Err(e) = ios::device::boot_simulator(&serial).await {
                    warn!(%serial, error = %e, "simulator reboot failed; continuing with re-delivery");
                } else {
                    // Give SpringBoard a moment past `Booted` before
                    // asking xcodebuild to install/launch the runner.
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if let Err(e) = self
                        .svc
                        .restart_ios_agent_for_app(&serial, &bundle_id, false, 5_000)
                        .await
                    {
                        warn!(%serial, error = %e, "agent restart after simulator reboot failed");
                    }
                }
                // The reboot is recovery, not a delivery attempt —
                // don't let it consume one.
                attempt -= 1;
            }
            // The terminate MUST actually land: `simctl openurl` to a
            // still-running app foregrounds it without delivering any
            // navigation event to React Native, and the verify below
            // can't tell that apart from a legitimate delivery whose
            // destination renders like the current screen — the app
            // silently stays where it was while the reset "succeeds".
            // The agent now confirms the process died; when it can't
            // (observed under CoreSimulator pressure with agent
            // commands running 10-40s), force it host-side before
            // delivering.
            let terminate_confirmed = matches!(
                self.svc.send_agent_command_with_timeout(
                    &AgentCommand::TerminateApp {
                        package: bundle_id.clone(),
                    },
                    10_000,
                )
                .await,
                Ok(resp) if resp.success
            );
            if !terminate_confirmed {
                warn!(
                    %serial, uri = %uri,
                    "agent could not confirm app termination; forcing simctl \
                     terminate so openurl cold-launches the app"
                );
                let _ = ios::device::terminate_app(&serial, &bundle_id).await;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;

            if let Err(e) = self
                .svc
                .open_ios_simulator_url_with_prompt_handling(&serial, uri)
                .await
            {
                if ios::device::is_retryable_open_url_error(&e.to_string()) {
                    last_error = e.to_string();
                    if attempt >= max_attempts {
                        warn!(
                            %serial, uri = %uri, attempt, max_attempts,
                            error = %e,
                            "simctl openurl kept hitting a transient timeout; giving up"
                        );
                        break;
                    }
                    warn!(
                        %serial, uri = %uri, attempt,
                        error = %e,
                        "simctl openurl hit a transient timeout; re-delivering deep link"
                    );
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                return Ok(self
                    .svc
                    .action_error(request_id, "ACTION_FAILED", e.to_string())
                    .await);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;

            let command = AgentCommand::OpenDeepLink {
                url: uri.to_string(),
                package: bundle_id.clone(),
                deliver_in_process: false,
                require_ui_change: false,
                ack_epoch_gt,
                ack_boot_before: ack_boot_before.clone(),
                ack_nav_gt,
            };
            let result = TapsmithServiceImpl::require_epoch_ack_echo(
                ack_epoch_gt,
                self.svc
                    .send_agent_command_with_timeout(&command, IOS_OPEN_DEEP_LINK_VERIFY_TIMEOUT_MS)
                    .await,
            );
            if matches!(&result, Ok(resp) if resp.success) {
                verify_result = Some(result);
                break;
            }
            last_error = match &result {
                Ok(resp) => resp
                    .error
                    .clone()
                    .unwrap_or_else(|| "app did not reach deep-link destination".to_string()),
                Err(status) => status.message().to_string(),
            };
            verify_result = Some(result);
            // A display-not-rendering failure is exempt from the
            // attempt cap until the one-shot reboot escalation (top of
            // loop) has had its chance — re-delivery alone provably
            // can't fix it, so counting it against the cap just gives up.
            let reboot_pending =
                last_error.contains("display is not rendering") && !rebooted_for_black_display;
            if attempt >= max_attempts && !reboot_pending {
                warn!(
                    %serial, uri = %uri, attempt, max_attempts,
                    error = %last_error,
                    "deep link did not reach its destination after retries; giving up"
                );
                break;
            }
            warn!(
                %serial, uri = %uri, attempt,
                error = %last_error,
                "deep link did not reach its destination; re-delivering"
            );
        }

        match verify_result {
            Some(result) => self.svc.make_action_response(request_id, result).await,
            None => Ok(self
                .svc
                .action_error(request_id, "ACTION_FAILED", last_error)
                .await),
        }
    }

    async fn launch_app(
        &self,
        request_id: String,
        serial: String,
        req: &proto::LaunchAppRequest,
    ) -> ActionResult {
        let idle_timeout_ms = if req.idle_timeout_ms > 0 {
            req.idle_timeout_ms
        } else {
            10000
        };
        let is_physical = self.svc.is_active_ios_physical().await;

        if req.clear_data {
            if is_physical {
                // Physical devices have no host-accessible app container
                // filesystem — `get_app_container` / `clear_container`
                // are simulator-only hacks. Instead, we rely on the
                // agent-mediated relaunch below to provide a fresh
                // launch; to actually wipe persistent state on a
                // physical device the user must reinstall the app
                // (future work: wire reinstall via devicectl into
                // clear_data).
                warn!(
                    package = %req.package_name,
                    "clear_data on physical iOS device is best-effort: \
                     persistent AsyncStorage/caches are not wiped. \
                     Reinstall the app via `tapsmith test --force-install` \
                     for a clean slate."
                );
            } else {
                // Terminate the app first to avoid file access conflicts
                // when clearing the data container.
                let _ = ios::device::terminate_app(&serial, &req.package_name).await;
                // Clear the data container (AsyncStorage, caches, etc.)
                // without uninstalling the app.
                match self
                    .svc
                    .get_app_container_cached(&serial, &req.package_name)
                    .await
                {
                    Ok(ref container) => {
                        if let Err(e) = ios::device::clear_container(container).await {
                            warn!(error = %e, "Failed to clear app container");
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "Could not get app container");
                    }
                }
            }
        }

        if let Err(error_message) = self
            .svc
            .reset_ios_app(
                &serial,
                &req.package_name,
                req.wait_for_idle,
                idle_timeout_ms,
            )
            .await
        {
            return Ok(self
                .svc
                .action_error(request_id, "LAUNCH_FAILED", error_message)
                .await);
        }

        Ok(Response::new(proto::ActionResponse {
            request_id,
            success: true,
            error_type: String::new(),
            error_message: String::new(),
            screenshot: Vec::new(),
        }))
    }

    async fn restart_app(
        &self,
        request_id: String,
        serial: String,
        req: &proto::RestartAppRequest,
    ) -> ActionResult {
        let idle_timeout_ms = if req.idle_timeout_ms > 0 {
            req.idle_timeout_ms
        } else {
            10000
        };

        if let Err(error_message) = self
            .svc
            .reset_ios_app(
                &serial,
                &req.package_name,
                req.wait_for_idle,
                idle_timeout_ms,
            )
            .await
        {
            return Ok(self
                .svc
                .action_error(request_id, "LAUNCH_FAILED", error_message)
                .await);
        }

        Ok(TapsmithServiceImpl::success_action_response(request_id))
    }

    async fn clear_app_data(
        &self,
        request_id: String,
        req: &proto::ClearAppDataRequest,
    ) -> ActionResult {
        if self.svc.is_active_ios_physical().await {
            // Physical devices have no host-accessible app container
            // filesystem, so `get_app_container` / `clear_container`
            // are meaningless. Reinstall the app bundle as the only
            // reliable way to wipe persistent state on a real device.
            // Requires the app path to have been cached during
            // StartAgent (via the StartAgentRequest.ios_app_path
            // field). Without it we can't reinstall and must surface
            // an actionable error.
            let serial = self.svc.active_serial().await?;
            let app_path = self
                .svc
                .ios_agent_config
                .read()
                .await
                .as_ref()
                .and_then(|c| c.app_path.clone());
            let Some(app_path) = app_path else {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "UNSUPPORTED_ON_PHYSICAL_DEVICE",
                        "device.clearAppData on a physical iOS device requires the \
                                 app bundle path to have been passed at startAgent time. \
                                 Tapsmith's CLI does this automatically when `app` is set in \
                                 your config. If you're calling startAgent manually, pass \
                                 the device-signed .app path via StartAgentRequest.ios_app_path."
                            .to_string(),
                    )
                    .await);
            };
            // Uninstall + install is the only devicectl sequence
            // that actually wipes the app's data container on
            // physical iOS (plain `install` preserves the container
            // when replacing an existing bundle). Uninstall also
            // drops the bundle's LaunchServices URL-scheme entry,
            // so we wait briefly after install for LaunchServices
            // to re-index — without this, the first subsequent
            // `openDeepLink` call drops silently.
            if let Err(e) = ios::device::uninstall_app_on_device(&serial, &req.package_name).await {
                warn!(error = %e, "Uninstall step of physical-iOS clearAppData failed, continuing to reinstall");
            }
            if let Err(e) = ios::device::install_app_on_device(&serial, &app_path).await {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "ACTION_FAILED",
                        format!("clearAppData reinstall failed: {e}"),
                    )
                    .await);
            }
            // Give LaunchServices time to re-index the bundle's URL
            // schemes. Without this, the next `openDeepLink` often
            // fires before iOS knows what app handles the scheme.
            tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
            return Ok(Response::new(proto::ActionResponse {
                request_id,
                success: true,
                error_type: String::new(),
                error_message: String::new(),
                screenshot: Vec::new(),
            }));
        }
        let serial = self.svc.active_serial().await?;
        // Clear the data container (AsyncStorage, UserDefaults, caches)
        // but do NOT terminate the app — launchApp handles that and
        // properly re-establishes the XCUITest accessibility bridge.
        // Clearing data while the app is running is fine because the
        // app will be relaunched anyway.
        match self
            .svc
            .get_app_container_cached(&serial, &req.package_name)
            .await
        {
            Ok(ref container) => {
                if let Err(e) = ios::device::clear_container(container).await {
                    warn!(error = %e, "Failed to clear app container, continuing anyway");
                }
                // Also clear the simulator's device-level keychain so
                // keychain-backed auth state (e.g. Firebase Auth)
                // doesn't survive a "full" data clear, leaving a
                // half-signed-in app. The keychain is device-global,
                // so this clears it for every app on the simulator —
                // acceptable for test-owned simulators.
                if !ios::device::keychain_state_disabled() {
                    match ios::device::simulator_keychain_dir(container) {
                        Some(keychain_dir) => {
                            if let Err(e) =
                                ios::device::clear_keychain(&serial, &keychain_dir).await
                            {
                                warn!(error = %e, "Failed to clear simulator keychain, continuing anyway");
                            }
                        }
                        None => {
                            warn!(container = %container, "Could not derive simulator keychain dir; keychain not cleared");
                        }
                    }
                }
            }
            Err(e) => {
                debug!(error = %e, "Could not get app container (app may not be installed)");
            }
        }
        Ok(Response::new(proto::ActionResponse {
            request_id,
            success: true,
            error_type: String::new(),
            error_message: String::new(),
            screenshot: Vec::new(),
        }))
    }

    async fn supports_cold_deep_link(&self) -> bool {
        !self.svc.is_active_ios_physical().await
    }

    async fn save_app_state(
        &self,
        request_id: String,
        serial: String,
        req: &proto::SaveAppStateRequest,
    ) -> ActionResult {
        let pkg = &req.package_name;
        let local_path = &req.path;

        if self.svc.is_active_ios_physical().await {
            // Physical devices: use `xcrun devicectl device copy from
            // --domain-type appDataContainer` to pull the app's data
            // container to a scratch directory, then tar it for
            // parity with the simulator output shape.
            warn!(
                %pkg,
                "Keychain state is not captured on physical iOS devices — keychain-backed data (e.g. native auth SDK credentials) will not be saved"
            );
            let scratch =
                tempfile::tempdir().map_err(|e| Status::internal(format!("tempdir: {e}")))?;
            let scratch_path = scratch.path().to_string_lossy().to_string();
            if let Err(e) =
                ios::device::copy_app_container_from_device(&serial, pkg, &scratch_path).await
            {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_SAVE_FAILED",
                        format!("devicectl copy from failed: {e}"),
                    )
                    .await);
            }
            if let Some(parent) = std::path::Path::new(local_path).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let output = tokio::process::Command::new("tar")
                .args([
                    "czf",
                    local_path,
                    "--exclude=./Library/Caches",
                    "--exclude=./Library/WebKit",
                    "--exclude=./Library/SplashBoard",
                    "--exclude=./Library/Saved Application State",
                    "--exclude=./tmp",
                    "-C",
                    &scratch_path,
                    ".",
                ])
                .output()
                .await;
            return match output {
                Ok(out) if out.status.success() => {
                    info!(%pkg, %local_path, "iOS physical app state saved");
                    Ok(TapsmithServiceImpl::success_action_response(request_id))
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    Ok(self
                        .svc
                        .action_error(
                            request_id,
                            "APP_STATE_SAVE_FAILED",
                            format!("tar failed: {stderr}"),
                        )
                        .await)
                }
                Err(e) => Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_SAVE_FAILED",
                        format!("Failed to run tar: {e}"),
                    )
                    .await),
            };
        }
        // iOS simulator: app container is on the host filesystem.
        // Use simctl to find it, then tar it directly.
        let container = match self.svc.get_app_container_cached(&serial, pkg).await {
            Ok(path) => path,
            Err(e) => {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_SAVE_FAILED",
                        format!("Failed to locate app container: {e}"),
                    )
                    .await);
            }
        };

        // Terminate the app to flush data
        let _ = ios::device::terminate_app(&serial, pkg).await;

        // Stage the simulator's device-level keychain db so the
        // archive captures keychain-backed state (Firebase Auth and
        // other SDKs persist credentials there, outside the app
        // container). Staging failures are non-fatal: the archive is
        // still valid, just without keychain state.
        let keychain_staging =
            tempfile::tempdir().map_err(|e| Status::internal(format!("tempdir: {e}")))?;
        let mut keychain_staged = 0;
        if !ios::device::keychain_state_disabled() {
            match ios::device::simulator_keychain_dir(&container) {
                Some(keychain_dir) => {
                    let dest = keychain_staging
                        .path()
                        .join(ios::device::KEYCHAIN_ARCHIVE_MEMBER);
                    match ios::device::stage_keychain_files(&keychain_dir, &dest).await {
                        Ok(count) => keychain_staged = count,
                        Err(e) => {
                            warn!(%pkg, error = %e, "Failed to stage simulator keychain; archive will not include keychain state");
                        }
                    }
                }
                None => {
                    warn!(%pkg, container, "Could not derive simulator keychain dir; archive will not include keychain state");
                }
            }
        }

        // Create tar.gz archive of the data container, excluding
        // caches and ephemeral data that inflates the archive
        // without contributing to app state (auth tokens, prefs,
        // databases). A React Native app container can have tens
        // of MB in Library/Caches and Library/WebKit alone.
        if let Some(parent) = std::path::Path::new(local_path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let mut tar_args: Vec<String> = vec![
            "czf".into(),
            local_path.clone(),
            "--exclude=./Library/Caches".into(),
            "--exclude=./Library/WebKit".into(),
            "--exclude=./Library/SplashBoard".into(),
            "--exclude=./Library/Saved Application State".into(),
            "--exclude=./tmp".into(),
            "-C".into(),
            container.clone(),
            ".".into(),
        ];
        if keychain_staged > 0 {
            // Second -C pair appends the staged keychain as a
            // reserved member alongside the container contents.
            tar_args.extend([
                "-C".into(),
                keychain_staging.path().to_string_lossy().to_string(),
                format!("./{}", ios::device::KEYCHAIN_ARCHIVE_MEMBER),
            ]);
        }
        let output = tokio::process::Command::new("tar")
            .args(&tar_args)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                info!(%pkg, %local_path, "iOS app state saved");
                Ok(TapsmithServiceImpl::success_action_response(request_id))
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_SAVE_FAILED",
                        format!("tar failed: {stderr}"),
                    )
                    .await)
            }
            Err(e) => Ok(self
                .svc
                .action_error(
                    request_id,
                    "APP_STATE_SAVE_FAILED",
                    format!("Failed to run tar: {e}"),
                )
                .await),
        }
    }

    async fn restore_app_state(
        &self,
        request_id: String,
        serial: String,
        req: &proto::RestoreAppStateRequest,
    ) -> ActionResult {
        let pkg = &req.package_name;
        let local_path = &req.path;

        if self.svc.is_active_ios_physical().await {
            // Physical: extract the archive locally, then push each
            // top-level child (`Documents`, `Library`, `tmp`) back
            // into the app container via `devicectl device copy to
            // --remove-existing-content true`. We terminate the app
            // first so iOS isn't actively writing to the directories
            // we're overwriting.
            let scratch =
                tempfile::tempdir().map_err(|e| Status::internal(format!("tempdir: {e}")))?;
            let scratch_path = scratch.path().to_string_lossy().to_string();
            let extract = tokio::process::Command::new("tar")
                .args(["xzf", local_path, "-C", &scratch_path])
                .output()
                .await;
            match extract {
                Ok(out) if !out.status.success() => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Ok(self
                        .svc
                        .action_error(
                            request_id,
                            "APP_STATE_RESTORE_FAILED",
                            format!("tar extract failed: {stderr}"),
                        )
                        .await);
                }
                Err(e) => {
                    return Ok(self
                        .svc
                        .action_error(
                            request_id,
                            "APP_STATE_RESTORE_FAILED",
                            format!("Failed to run tar: {e}"),
                        )
                        .await);
                }
                _ => {}
            }
            // Collect top-level children that actually exist in the
            // archive — we only want to push directories the user
            // actually saved, so empty runs don't wipe the live data.
            let mut sources: Vec<String> = Vec::new();
            for name in ["Documents", "Library", "tmp"] {
                let candidate = std::path::Path::new(&scratch_path).join(name);
                if candidate.exists() {
                    sources.push(candidate.to_string_lossy().to_string());
                }
            }
            if sources.is_empty() {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_RESTORE_FAILED",
                        "Archive contained no Documents/Library/tmp directories".to_string(),
                    )
                    .await);
            }
            // Wipe the container by uninstalling + reinstalling the
            // app. `devicectl copy to --remove-existing-content`
            // alone would leave the app running and racing with
            // our writes; wiping via reinstall is the same pattern
            // we use for `clearAppData` and avoids every possible
            // "process is holding files" edge case. After the
            // reinstall the container is empty, so the subsequent
            // copy just lays the saved state on top.
            let app_path = self
                .svc
                .ios_agent_config
                .read()
                .await
                .as_ref()
                .and_then(|c| c.app_path.clone());
            let Some(app_path) = app_path else {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_RESTORE_FAILED",
                        "device.restoreAppState on a physical iOS device requires \
                                 the app bundle path cached at startAgent time. Tapsmith's \
                                 CLI passes this automatically when `app` is set."
                            .to_string(),
                    )
                    .await);
            };
            if let Err(e) = ios::device::uninstall_app_on_device(&serial, pkg).await {
                warn!(error = %e, "Pre-restore uninstall failed, continuing");
            }
            if let Err(e) = ios::device::install_app_on_device(&serial, &app_path).await {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_RESTORE_FAILED",
                        format!("Pre-restore reinstall failed: {e}"),
                    )
                    .await);
            }
            // Give LaunchServices a moment to re-register the
            // freshly installed bundle before devicectl re-enters
            // its sandbox to push the container contents.
            tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
            if let Err(e) = ios::device::copy_app_container_to_device(&serial, pkg, &sources).await
            {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_RESTORE_FAILED",
                        format!("devicectl copy to failed: {e}"),
                    )
                    .await);
            }
            info!(%pkg, %local_path, "iOS physical app state restored");
            // Reinstalling + pushing the container replaces the app
            // bundle under the running XCUITest runner. The agent's
            // cached XCUIApplication references become stale in a
            // way that causes snapshots to return partial trees
            // after the next launch — elements present in the
            // hierarchy dump are missing from findElement. Restart
            // the agent entirely so the new test session starts
            // against fresh XCUITest bindings.
            if let Err(e) = self
                .svc
                .restart_ios_agent_for_app(&serial, pkg, false, 10_000)
                .await
            {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_RESTORE_FAILED",
                        format!("Agent restart after restore failed: {e}"),
                    )
                    .await);
            }
            return Ok(TapsmithServiceImpl::success_action_response(request_id));
        }
        // iOS simulator: terminate the app, clear the data container,
        // then extract the saved archive.
        let _ = ios::device::terminate_app(&serial, pkg).await;

        let container = match self.svc.get_app_container_cached(&serial, pkg).await {
            Ok(path) => path,
            Err(e) => {
                return Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_RESTORE_FAILED",
                        format!("Failed to locate app container: {e}"),
                    )
                    .await);
            }
        };

        // Clear the container in-place rather than rm -rf. Deleting
        // the container directory entirely can cause iOS to lose
        // its internal bundle-id → container UUID mapping, making
        // the app launch into a fresh container instead of the one
        // we extracted into.
        if let Err(e) = ios::device::clear_container(&container).await {
            warn!(%pkg, error = %e, "clear_container failed, continuing");
        }

        // Archives saved by newer daemons carry the simulator
        // keychain under a reserved member; older archives don't,
        // and restore exactly as before.
        let has_keychain = match tokio::process::Command::new("tar")
            .args(["tzf", local_path])
            .output()
            .await
        {
            Ok(out) if out.status.success() => {
                ios::device::archive_has_keychain_member(&String::from_utf8_lossy(&out.stdout))
            }
            _ => false,
        };

        // Exclude both the `./`-prefixed form (how bsdtar stores it
        // when we add it on save) and the bare form, so the keychain
        // member can never be extracted into the app container
        // regardless of how the archive recorded it.
        let member = ios::device::KEYCHAIN_ARCHIVE_MEMBER;
        let member_pattern = format!("./{member}");
        // The archive carries the *source* container's
        // container-manager metadata; keep this container's own
        // (PILOT-462). bsdtar's --exclude is not anchored, so the
        // files are put back after the extract rather than excluded.
        // Spawned, so a cancelled request (its future dropped
        // mid-extract) still finishes the extract and the put-back
        // instead of leaving the archive's record in the container.
        let output = {
            let container = container.clone();
            let local_path = local_path.clone();
            let excludes = [
                member_pattern.clone(),
                format!("{member_pattern}/*"),
                member.to_string(),
                format!("{member}/*"),
            ];
            tokio::spawn(async move {
                ios::device::keeping_container_manager_files(&container, async {
                    let mut tar = tokio::process::Command::new("tar");
                    tar.args(["xzf", local_path.as_str(), "-C", container.as_str()]);
                    for pattern in &excludes {
                        tar.args(["--exclude", pattern.as_str()]);
                    }
                    tar.output().await
                })
                .await
            })
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e)))
        };

        match output {
            Ok(out) if out.status.success() => {
                if has_keychain && !ios::device::keychain_state_disabled() {
                    if let Err(e) =
                        restore_simulator_keychain(&serial, &container, local_path).await
                    {
                        // Failing silently here would reproduce the
                        // half-restored auth state this feature
                        // exists to fix — surface it instead.
                        return Ok(self
                            .svc
                            .action_error(
                                request_id,
                                "APP_STATE_RESTORE_FAILED",
                                format!("Keychain restore failed: {e}"),
                            )
                            .await);
                    }
                } else if has_keychain {
                    debug!(%pkg, "Archive contains keychain state but TAPSMITH_NO_KEYCHAIN_STATE is set; skipping keychain restore");
                } else {
                    debug!(%pkg, "Archive has no keychain member (saved by an older version); skipping keychain restore");
                }
                info!(%pkg, %local_path, container, "iOS simulator app state restored");
                Ok(TapsmithServiceImpl::success_action_response(request_id))
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Ok(self
                    .svc
                    .action_error(
                        request_id,
                        "APP_STATE_RESTORE_FAILED",
                        format!("tar extract failed: {stderr}"),
                    )
                    .await)
            }
            Err(e) => Ok(self
                .svc
                .action_error(
                    request_id,
                    "APP_STATE_RESTORE_FAILED",
                    format!("Failed to run tar: {e}"),
                )
                .await),
        }
    }

    async fn list_web_views(
        &self,
        request_id: String,
        serial: String,
    ) -> Result<Response<proto::ListWebViewsResponse>, Status> {
        #[cfg(target_os = "macos")]
        {
            use crate::ios::webkit_debug_proxy::WebkitDebugProxyHandle;

            let mut proxy_guard = self.svc.webkit_debug_proxy.write().await;
            if proxy_guard.is_none() {
                match WebkitDebugProxyHandle::start(serial.clone(), 9221).await {
                    Ok(handle) => {
                        *proxy_guard = Some(handle);
                    }
                    Err(e) => {
                        return Ok(Response::new(proto::ListWebViewsResponse {
                            request_id,
                            webviews: Vec::new(),
                            error_message: format!("Failed to start ios_webkit_debug_proxy: {e}"),
                        }));
                    }
                }
            }
            let port = proxy_guard.as_ref().unwrap().port();
            drop(proxy_guard);

            // Query the proxy's HTTP endpoint for available targets
            match query_cdp_json(port).await {
                Ok(targets) => {
                    let webviews = targets
                        .into_iter()
                        .filter_map(|t| {
                            let ws_url = t.get("webSocketDebuggerUrl")?.as_str()?;
                            Some(proto::WebViewInfo {
                                socket_name: ws_url.to_string(),
                                pid: 0,
                                package_name: String::new(),
                                url: t
                                    .get("url")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                title: t
                                    .get("title")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                        })
                        .collect();
                    Ok(Response::new(proto::ListWebViewsResponse {
                        request_id,
                        webviews,
                        error_message: String::new(),
                    }))
                }
                Err(e) => Ok(Response::new(proto::ListWebViewsResponse {
                    request_id,
                    webviews: Vec::new(),
                    error_message: format!(
                        "Failed to query ios_webkit_debug_proxy: {e}. \
                                 Ensure Safari Web Inspector is enabled on the device \
                                 and the WebView has isInspectable = true (iOS 16.4+)."
                    ),
                })),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(Response::new(proto::ListWebViewsResponse {
                request_id,
                webviews: Vec::new(),
                error_message: "iOS WebView testing requires macOS".to_string(),
            }))
        }
    }

    async fn forward_web_view_port(
        &self,
        request_id: String,
        _serial: String,
        req: proto::ForwardWebViewPortRequest,
    ) -> Result<Response<proto::ForwardWebViewPortResponse>, Status> {
        // For iOS, the socket_name IS the webSocketDebuggerUrl —
        // extract the port from it and return it directly since
        // ios_webkit_debug_proxy already exposes targets on localhost.
        let port = extract_port_from_ws_url(&req.socket_name).unwrap_or(0);
        if port == 0 {
            return Ok(Response::new(proto::ForwardWebViewPortResponse {
                request_id,
                success: false,
                local_port: 0,
                error_message: format!(
                    "Failed to extract port from WebSocket URL: {}",
                    req.socket_name
                ),
            }));
        }
        Ok(Response::new(proto::ForwardWebViewPortResponse {
            request_id,
            success: true,
            local_port: port as u32,
            error_message: String::new(),
        }))
    }
}
