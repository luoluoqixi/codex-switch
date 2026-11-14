//! Desktop application discovery and lifecycle. All subprocesses run off the UI thread.
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
#[path = "desktop/windows.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "desktop/macos.rs"]
mod platform;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesktopApp {
    ChatGpt,
}

impl DesktopApp {
    pub const ALL: [Self; 1] = [Self::ChatGpt];

    pub fn name(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::ChatGpt => 0,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum AppAction {
    Open,
    Close,
    Restart,
}

impl AppAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => crate::tr!("desktop.open"),
            Self::Close => crate::tr!("desktop.close"),
            Self::Restart => crate::tr!("desktop.reopen"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppStatus {
    app: DesktopApp,
    pids: Vec<u32>,
    path: Option<PathBuf>,
    #[cfg(target_os = "windows")]
    registered_id: Option<String>,
    problem: Option<String>,
    debug_endpoint: bool,
}

impl AppStatus {
    pub fn app(&self) -> DesktopApp {
        self.app
    }
    pub fn is_running(&self) -> bool {
        !self.pids.is_empty()
    }
    pub fn is_installed(&self) -> bool {
        #[cfg(target_os = "windows")]
        if self.registered_id.is_some() {
            return true;
        }
        self.path.is_some()
    }
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }
    pub fn has_installation_path(&self) -> bool {
        self.path.is_some() && self.problem.is_none()
    }
    pub fn has_debug_endpoint(&self) -> bool {
        self.debug_endpoint
    }
}

pub(crate) fn installation_path(app: DesktopApp, paths: &[String; 1]) -> Result<PathBuf> {
    let status = inspect(paths)?
        .into_iter()
        .find(|status| status.app == app)
        .context(crate::tr!("desktop.could_not_read_the_app_status"))?;
    if let Some(problem) = status.problem() {
        bail!("{problem}");
    }
    let path = status.path.as_deref().context(crate::tr!(
        "desktop.could_not_locate_the_app_set_the_chatgpt_app"
    ))?;
    anyhow::ensure!(
        path.exists(),
        crate::tr!("desktop.the_app_path_no_longer_exists_refresh_or_check")
    );
    // Reveal the application itself: the .exe on Windows or the .app bundle on macOS.
    Ok(path.to_path_buf())
}

pub(crate) fn validate_paths(paths: &[String]) -> Result<()> {
    for (app, value) in DesktopApp::ALL.into_iter().zip(paths) {
        if value.is_empty() {
            continue;
        }
        let path = Path::new(value);
        anyhow::ensure!(
            path.is_absolute(),
            crate::tr!("desktop.the_app_path_must_be_absolute"; arg0 = app.name())
        );
        #[cfg(target_os = "windows")]
        anyhow::ensure!(
            path.is_file()
                && path.file_name().is_some_and(|name| name
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&format!("{}.exe", app.name()))),
            crate::tr!("desktop.select_the_full_path_to_exe"; arg0 = app.name())
        );
        #[cfg(target_os = "macos")]
        anyhow::ensure!(
            path.is_dir() && path.extension().is_some_and(|ext| ext == "app"),
            crate::tr!("desktop.select_the_app_directory"; arg0 = app.name())
        );
    }
    Ok(())
}

pub(crate) fn inspect(paths: &[String; 1]) -> Result<Vec<AppStatus>> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let mut statuses = platform::inspect(paths)?;
        crate::themes::set_expected_executable(
            statuses.first().and_then(|status| status.path.clone()),
        );
        for status in &mut statuses {
            status.debug_endpoint = status.is_running()
                && status.problem.is_none()
                && crate::themes::has_debug_endpoint();
        }
        Ok(statuses)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = paths;
        bail!(crate::tr!(
            "desktop.app_launch_controls_are_supported_on_windows_and_macos"
        ))
    }
}

pub(crate) fn perform(app: DesktopApp, action: AppAction, paths: &[String; 1]) -> Result<String> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let before = inspect(paths)?
            .into_iter()
            .find(|status| status.app == app)
            .context(crate::tr!("desktop.could_not_read_the_app_status"))?;
        if let Some(problem) = &before.problem {
            bail!("{problem}");
        }
        if matches!(action, AppAction::Open) && before.is_running() {
            anyhow::ensure!(
                crate::themes::has_debug_endpoint(),
                crate::tr!("desktop.codex_is_running_without_a_theme_debugging_connection_click")
            );
            return Ok(
                crate::tr!("desktop.is_running_with_a_theme_debugging_connection"; arg0 = app.name()),
            );
        }
        if !matches!(action, AppAction::Close) {
            anyhow::ensure!(
                before.is_installed(),
                crate::tr!("desktop.was_not_found_set_its_app_path_in_settings"; arg0 = app.name())
            );
        }
        if !matches!(action, AppAction::Open) && before.is_running() {
            platform::close(&before, paths)?;
            wait_for(app, paths, false).with_context(|| {
                if cfg!(target_os = "windows") {
                    crate::tr!(
                        "desktop.some_app_processes_are_still_running_check_system_permissions"
                    )
                } else {
                    crate::tr!("desktop.the_app_has_not_quit_yet_handle_its_prompts")
                }
            })?;
        }
        if !matches!(action, AppAction::Close) {
            platform::launch(&before)?;
            wait_for(app, paths, true).context(crate::tr!(
                "desktop.the_open_request_was_sent_but_the_app_is"
            ))?;
            crate::themes::verify_launch_endpoint()?;
        }
        Ok(crate::tr!("desktop.completed"; arg0 = action.label(), arg1 = app.name()))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (app, action, paths);
        bail!(crate::tr!(
            "desktop.app_launch_controls_are_supported_on_windows_and_macos"
        ))
    }
}

fn wait_for(app: DesktopApp, paths: &[String; 1], running: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let statuses = inspect(paths)?;
        let status = statuses
            .iter()
            .find(|s| s.app == app)
            .context(crate::tr!("desktop.could_not_read_the_app_status"))?;
        if let Some(problem) = &status.problem {
            bail!("{problem}");
        }
        if status.is_running() == running {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                crate::tr!("desktop.timed_out_waiting_for_the_status_to_update"; arg0 = app.name())
            );
        }
        thread::sleep(Duration::from_millis(400));
    }
}

// A timeout also applies to discovery, so a stalled OS helper cannot wedge the UI.
fn output(mut command: Command) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW: hide helper consoles.
    }
    let mut child = command.spawn().context(crate::tr!(
        "desktop.could_not_start_the_system_app_management_tool"
    ))?;
    // Drain stdout concurrently so a full pipe cannot deadlock discovery.
    let mut pipe = child
        .stdout
        .take()
        .context(crate::tr!("desktop.could_not_read_the_system_tool_output"))?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut pipe, &mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .context(crate::tr!("desktop.could_not_read_the_system_tool_status"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(crate::tr!(
                "desktop.the_system_app_management_tool_timed_out_try_again"
            ));
        }
        thread::sleep(Duration::from_millis(40));
    };
    let bytes = reader.join().map_err(|_| {
        anyhow::anyhow!(crate::tr!("desktop.could_not_read_the_system_tool_output"))
    })??;
    anyhow::ensure!(
        status.success(),
        crate::tr!("desktop.the_system_app_operation_failed_check_the_installation_path"; status = status)
    );
    Ok(bytes)
}

fn optional_path(value: &serde_json::Value) -> Option<PathBuf> {
    value.as_str().filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn pids(value: &serde_json::Value) -> Vec<u32> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pid| pid.as_u64().and_then(|p| u32::try_from(p).ok()))
        .collect()
}
