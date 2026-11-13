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
            Self::Open => "打开",
            Self::Close => "关闭",
            Self::Restart => "重新打开",
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
}

pub(crate) fn installation_path(app: DesktopApp, paths: &[String; 1]) -> Result<PathBuf> {
    let status = inspect(paths)?
        .into_iter()
        .find(|status| status.app == app)
        .context("无法读取应用状态")?;
    if let Some(problem) = status.problem() {
        bail!("{problem}");
    }
    let path = status
        .path
        .as_deref()
        .context("无法确定应用位置，请在设置中指定 ChatGPT 应用路径")?;
    anyhow::ensure!(path.exists(), "应用路径已不存在，请刷新或检查设置");
    // Reveal the application itself: the .exe on Windows or the .app bundle on macOS.
    Ok(path.to_path_buf())
}

pub(crate) fn validate_paths(paths: &[String; 1]) -> Result<()> {
    for (app, value) in DesktopApp::ALL.into_iter().zip(paths) {
        if value.is_empty() {
            continue;
        }
        let path = Path::new(value);
        anyhow::ensure!(path.is_absolute(), "{} 应用路径须为绝对路径", app.name());
        #[cfg(target_os = "windows")]
        anyhow::ensure!(
            path.is_file()
                && path.file_name().is_some_and(|name| name
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&format!("{}.exe", app.name()))),
            "请选择 {}.exe 的完整路径",
            app.name()
        );
        #[cfg(target_os = "macos")]
        anyhow::ensure!(
            path.is_dir() && path.extension().is_some_and(|ext| ext == "app"),
            "请选择 {}.app 应用目录",
            app.name()
        );
    }
    Ok(())
}

pub(crate) fn inspect(paths: &[String; 1]) -> Result<Vec<AppStatus>> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        platform::inspect(paths)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = paths;
        bail!("应用启动控制目前支持 Windows 和 macOS")
    }
}

pub(crate) fn perform(app: DesktopApp, action: AppAction, paths: &[String; 1]) -> Result<String> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let before = inspect(paths)?
            .into_iter()
            .find(|status| status.app == app)
            .context("无法读取应用状态")?;
        if let Some(problem) = &before.problem {
            bail!("{problem}");
        }
        if matches!(action, AppAction::Open) && before.is_running() {
            return Ok(format!("{} 已在运行。", app.name()));
        }
        if !matches!(action, AppAction::Close) {
            anyhow::ensure!(
                before.is_installed(),
                "未找到 {}，请在设置中填写应用路径。",
                app.name()
            );
        }
        if !matches!(action, AppAction::Open) && before.is_running() {
            platform::close(&before, paths)?;
            wait_for(app, paths, false).with_context(|| {
                if cfg!(target_os = "windows") {
                    "应用进程尚未全部退出，请检查系统权限或是否有其他程序自动重新启动该应用"
                } else {
                    "应用尚未退出，请处理应用内的提示或手动退出后重试"
                }
            })?;
        }
        if !matches!(action, AppAction::Close) {
            platform::launch(&before)?;
            wait_for(app, paths, true)
                .context("已发送打开请求，但尚未检测到应用运行；请检查应用或在设置中指定路径")?;
        }
        Ok(format!("已{} {}。", action.label(), app.name()))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (app, action, paths);
        bail!("应用启动控制目前支持 Windows 和 macOS")
    }
}

fn wait_for(app: DesktopApp, paths: &[String; 1], running: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let statuses = inspect(paths)?;
        let status = statuses
            .iter()
            .find(|s| s.app == app)
            .context("无法读取应用状态")?;
        if let Some(problem) = &status.problem {
            bail!("{problem}");
        }
        if status.is_running() == running {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("等待 {} 状态更新超时", app.name());
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
    let mut child = command.spawn().context("无法启动系统应用管理工具")?;
    // Drain stdout concurrently so a full pipe cannot deadlock discovery.
    let mut pipe = child.stdout.take().context("无法读取系统工具输出")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut pipe, &mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().context("无法读取系统工具状态")? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("系统应用管理工具超时，请稍后重试");
        }
        thread::sleep(Duration::from_millis(40));
    };
    let bytes = reader
        .join()
        .map_err(|_| anyhow::anyhow!("无法读取系统工具输出"))??;
    anyhow::ensure!(
        status.success(),
        "系统应用操作失败（{status}），请检查应用安装位置和系统权限"
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
