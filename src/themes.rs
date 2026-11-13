//! Theme library and the lifetime of the optional, in-process CDP worker.
//! Import reads the manifest only to locate and name the extracted theme.
mod cdp;
mod identity;
mod image_import;
mod payload;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) const DEBUG_PORT: u16 = 9347;
static ENABLED: AtomicBool = AtomicBool::new(false);
static PROFILE: OnceLock<PathBuf> = OnceLock::new();
static EXECUTABLE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

pub(crate) fn set_expected_executable(path: Option<PathBuf>) {
    if let Some(path) = path
        && let Ok(mut expected) = EXECUTABLE.get_or_init(|| Mutex::new(None)).lock()
    {
        *expected = Some(path);
    }
}

fn verify_listener() -> Result<()> {
    let expected = EXECUTABLE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| anyhow::anyhow!("无法确认 Codex 进程身份"))?;
    #[cfg(target_os = "windows")]
    {
        identity::verify_listener(DEBUG_PORT, expected.as_deref())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = expected;
        Ok(())
    }
}

/// Only the user's existing Open/Restart command consumes these arguments.
pub(crate) fn launch_arguments() -> Result<Vec<OsString>> {
    if !ENABLED.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    let profile = PROFILE.get().context("主题运行目录尚未初始化")?;
    fs::create_dir_all(profile).context("无法创建主题所需的 Codex 浏览器数据目录")?;
    Ok(vec![
        "--remote-debugging-address=127.0.0.1".into(),
        format!("--remote-debugging-port={DEBUG_PORT}").into(),
        format!("--user-data-dir={}", profile.display()).into(),
    ])
}

pub(crate) fn verify_launch_endpoint() -> Result<()> {
    if !ENABLED.load(Ordering::Acquire) {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if cdp::is_available() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    anyhow::bail!(
        "应用已打开，但未能开启主题调试通道。请检查应用路径；当前 Codex 版本也可能不支持调试启动参数。主题尚未确认生效。"
    )
}

#[derive(Clone)]
pub(crate) struct ThemeEntry {
    pub id: String,
    pub name: String,
}

#[derive(Clone)]
pub(crate) struct ThemeSnapshot {
    pub themes: Vec<ThemeEntry>,
    pub selected: Option<String>,
    pub transparency: u8,
}
impl Default for ThemeSnapshot {
    fn default() -> Self {
        Self {
            themes: Vec::new(),
            selected: None,
            transparency: 30,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ThemeService {
    inner: Arc<Mutex<Library>>,
}
struct Library {
    state_path: PathBuf,
    restored: bool,
    selected: Option<String>,
    transparencies: BTreeMap<String, u8>,
    worker: Option<Worker>,
    errors: Arc<Mutex<Option<String>>>,
}
struct Worker {
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<Result<()>>>,
}
impl Worker {
    fn stop(mut self) -> Result<()> {
        let _ = self.stop.send(());
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("主题后台任务意外退出"))?
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.stop();
        }
        ENABLED.store(false, Ordering::Release);
    }
}

impl ThemeService {
    pub(crate) fn new(state_path: PathBuf) -> Self {
        let profile = browser_directory(&state_path);
        let _ = PROFILE.set(profile);
        Self {
            inner: Arc::new(Mutex::new(Library {
                state_path,
                restored: false,
                selected: None,
                transparencies: BTreeMap::new(),
                worker: None,
                errors: Arc::new(Mutex::new(None)),
            })),
        }
    }

    pub(crate) fn directory(&self, configured: &str) -> Result<PathBuf> {
        theme_directory(configured)
    }

    pub(crate) fn snapshot(&self, directory: &str) -> Result<ThemeSnapshot> {
        let root = self.directory(directory)?;
        let scan_result = scan(&root);
        let mut library = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("无法读取主题状态"))?;
        let themes = match scan_result {
            Ok(themes) => themes,
            Err(error) => {
                if library.selected.is_some() {
                    library.disable()?;
                    library.save()?;
                }
                return Err(error);
            }
        };
        if !library.restored {
            library.restored = true;
            if let Err(error) = library.restore(&themes) {
                library.report(format!("无法恢复主题：{error:#}"));
            }
        }
        // A changed theme directory must never silently keep using the old library.
        if library
            .selected
            .as_ref()
            .is_some_and(|id| !themes.iter().any(|theme| &theme.id == id))
        {
            library.disable()?;
            library.save()?;
        }
        Ok(ThemeSnapshot {
            transparency: library
                .selected
                .as_ref()
                .and_then(|id| library.transparencies.get(id))
                .copied()
                .unwrap_or_else(|| {
                    library
                        .selected
                        .as_ref()
                        .map(|id| payload::default_transparency(Path::new(id)))
                        .unwrap_or(30)
                }),
            selected: library.selected.clone(),
            themes,
        })
    }

    pub(crate) fn select(&self, directory: &str, id: Option<String>) -> Result<String> {
        if let Some(id) = &id {
            ensure!(
                scan(&self.directory(directory)?)?
                    .iter()
                    .any(|entry| &entry.id == id),
                "主题目录已不存在，请刷新后重试"
            );
        }
        let mut library = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("无法修改主题状态"))?;
        library.restored = true;
        if let Some(id) = id {
            let transparency = library.transparencies.get(&id).copied();
            let script = payload::build(Path::new(&id), transparency)?;
            library.activate(id, script)?;
            Ok("已选择主题；如未显示，请点击现有的重新打开按钮。".into())
        } else {
            library.disable()?;
            library.save()?;
            Ok("主题已关闭，主题后台任务已停止。".into())
        }
    }

    pub(crate) fn set_transparency(&self, value: u8) -> Result<String> {
        ensure!(value <= 100, "透明度须为 0–100");
        let mut library = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("无法修改主题状态"))?;
        let id = library.selected.clone().context("请先开启主题")?;
        let script = payload::build(Path::new(&id), Some(value))?;
        let previous = library.transparencies.insert(id.clone(), value);
        if let Err(error) = library.activate(id.clone(), script) {
            match previous {
                Some(previous) => {
                    library.transparencies.insert(id, previous);
                }
                None => {
                    library.transparencies.remove(&id);
                }
            }
            return Err(error);
        }
        Ok(format!("背景透明度已设为 {value}%"))
    }

    pub(crate) fn import_zip(&self, directory: &str, path: &Path) -> Result<String> {
        let root = self.directory(directory)?;
        extract_zip(path, &root)?;
        Ok("主题 ZIP 已解压到主题目录，请在下拉框中选择主题。".into())
    }

    pub(crate) fn import_files(&self, directory: &str, paths: &[PathBuf]) -> Result<String> {
        ensure!(!paths.is_empty(), "请选择主题 ZIP 或背景图片");
        let root = self.directory(directory)?;
        let mut imported = 0;
        let mut failures = Vec::new();
        for path in paths {
            let result = if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
            {
                self.import_zip(directory, path).map(|_| ())
            } else {
                image_import::import(path, &root)
            };
            match result {
                Ok(()) => imported += 1,
                Err(error) => failures.push(format!(
                    "{}：{error:#}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )),
            }
        }
        ensure!(
            failures.is_empty(),
            "已导入 {imported} 个，{} 个失败。\n\n{}",
            failures.len(),
            failures.join("\n\n")
        );
        Ok(format!("已导入 {imported} 个主题，请在下拉框中选择。"))
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.inner.try_lock().ok()?.errors.try_lock().ok()?.take()
    }

    /// Stop injection before process exit, while keeping the saved selection.
    pub(crate) fn shutdown(&self) -> Result<()> {
        let mut library = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("无法停止主题后台任务"))?;
        ENABLED.store(false, Ordering::Release);
        if let Err(error) = library.worker.take().map(Worker::stop).transpose() {
            library.selected = None;
            library.save()?;
            return Err(error);
        }
        Ok(())
    }
}

impl Library {
    fn report(&self, message: String) {
        if let Ok(mut error) = self.errors.lock() {
            *error = Some(message);
        }
    }
    fn restore(&mut self, themes: &[ThemeEntry]) -> Result<()> {
        if !self.state_path.exists() {
            return Ok(());
        }
        let saved: Value =
            serde_json::from_slice(&fs::read(&self.state_path)?).context("主题状态文件格式无效")?;
        if let Some(values) = saved["transparency"].as_object() {
            self.transparencies = values
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .as_u64()
                        .filter(|n| *n <= 100)
                        .map(|n| (key.clone(), n as u8))
                })
                .collect();
        }
        if let Some(id) = saved["selected"].as_str() {
            if !themes.iter().any(|theme| theme.id == id) {
                self.save()?;
                return Ok(());
            }
            let script = payload::build(Path::new(id), self.transparencies.get(id).copied())?;
            self.worker = Some(start_worker(script, self.errors.clone())?);
            self.selected = Some(id.into());
            ENABLED.store(true, Ordering::Release);
        }
        Ok(())
    }
    fn save(&self) -> Result<()> {
        let parent = self.state_path.parent().context("主题状态路径无效")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(
            &mut file,
            &json!({"selected": self.selected, "transparency": self.transparencies}),
        )?;
        file.flush()?;
        file.persist(&self.state_path)
            .map_err(|error| error.error)
            .context("无法保存主题状态")?;
        Ok(())
    }
    fn disable(&mut self) -> Result<()> {
        // Even when renderer cleanup reports an error, no worker may survive Off.
        ENABLED.store(false, Ordering::Release);
        self.selected = None;
        let result = self.worker.take().map(Worker::stop).transpose();
        if let Err(error) = result {
            self.save()?;
            return Err(error);
        }
        Ok(())
    }
    fn activate(&mut self, id: String, script: String) -> Result<()> {
        let previous = self.selected.clone();
        // Persist first so a disk failure cannot leave an unrecorded new theme.
        self.selected = Some(id);
        if let Err(error) = self.save() {
            self.selected = previous;
            return Err(error);
        }
        if let Some(worker) = self.worker.take()
            && let Err(error) = worker.stop()
        {
            self.report(format!("清理旧主题时出错：{error:#}"));
        }
        match start_worker(script, self.errors.clone()) {
            Ok(worker) => {
                self.worker = Some(worker);
                ENABLED.store(true, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                self.selected = None;
                ENABLED.store(false, Ordering::Release);
                let _ = self.save();
                Err(error)
            }
        }
    }
}

fn start_worker(script: String, errors: Arc<Mutex<Option<String>>>) -> Result<Worker> {
    let (stop, receiver) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("codex-theme".into())
        .spawn(move || {
            let mut connection = cdp::Runtime::new();
            let mut last_error = String::new();
            loop {
                if let Err(error) = connection.refresh(&script) {
                    let message = format!("主题应用失败：{error:#}");
                    if message != last_error {
                        if let Ok(mut slot) = errors.lock() {
                            *slot = Some(message.clone());
                        }
                        last_error = message;
                    }
                }
                match receiver.recv_timeout(Duration::from_millis(1500)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            connection.clear()
        })
        .context("无法启动主题后台任务")?;
    Ok(Worker {
        stop,
        thread: Some(thread),
    })
}

fn theme_directory(configured: &str) -> Result<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if configured.is_empty() {
        Ok(std::env::current_exe()?
            .parent()
            .context("无法确定程序目录")?
            .join("Themes"))
    } else {
        Ok(std::path::absolute(configured)?)
    }
}

fn browser_directory(state_path: &Path) -> PathBuf {
    // Store deliberately excludes dot directories from configuration profiles.
    // Browser storage must never appear as a deletable configuration backup.
    state_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".theme-codex-profile")
}

fn scan(root: &Path) -> Result<Vec<ThemeEntry>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.to_path_buf()];
    let mut seen = HashSet::new();
    let mut themes = Vec::new();
    while let Some(path) = pending.pop() {
        let canonical = fs::canonicalize(&path).context("无法读取主题目录")?;
        if !seen.insert(canonical) {
            continue;
        }
        if path.join("theme.json").is_file() {
            themes.push(ThemeEntry {
                id: std::path::absolute(&path)?.to_string_lossy().into_owned(),
                name: payload::theme_name(&path),
            });
        }
        for entry in fs::read_dir(&path).context("无法列出主题目录")? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    let mut counts = BTreeMap::<String, usize>::new();
    for entry in &themes {
        *counts.entry(entry.name.clone()).or_default() += 1;
    }
    for entry in &mut themes {
        if counts[&entry.name] > 1 {
            let relative = Path::new(&entry.id)
                .strip_prefix(root)
                .unwrap_or(Path::new(&entry.id));
            entry.name = format!("{} · {}", entry.name, relative.display());
        }
    }
    themes.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(themes)
}

fn extract_zip(path: &Path, root: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(path).context("无法打开 ZIP 文件")?)
        .context("无法解压 ZIP 文件")?;
    let mut names = Vec::with_capacity(archive.len());
    let mut first_directory = None;
    for ix in 0..archive.len() {
        let entry = archive.by_index(ix).context("无法读取 ZIP 条目")?;
        let name = entry.name().replace('\\', "/");
        if first_directory.is_none()
            && let Some((directory, _)) = name.split_once('/')
            && !directory.is_empty()
            && !directory.starts_with('.')
        {
            first_directory = Some(format!("{directory}/"));
        }
        names.push(name);
    }
    let (prefix, manifest_index) =
        if let Some(index) = names.iter().position(|n| n == "manifest.json") {
            (String::new(), index)
        } else {
            let prefix = first_directory
                .context("ZIP 根目录及第一个非点号开头的文件夹中均未找到 manifest.json")?;
            let manifest = format!("{prefix}manifest.json");
            let index = names
                .iter()
                .position(|name| name == &manifest)
                .context("ZIP 的第一个非点号开头的文件夹中未找到 manifest.json")?;
            (prefix, index)
        };
    let manifest: Value = {
        let mut entry = archive.by_index(manifest_index)?;
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut entry, &mut bytes).context("无法读取 manifest.json")?;
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .context("manifest.json 不是有效的 JSON")?
    };
    let name = manifest["themeId"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .context("manifest.json 中缺少非空的 themeId")?;
    let folder = theme_folder_name(name)?;
    fs::create_dir_all(root).context("无法创建主题目录")?;
    let root = fs::canonicalize(root)?;
    let target = root.join(folder);
    if target.exists() {
        ensure!(
            fs::canonicalize(&target)?.starts_with(&root),
            "主题文件夹不能通过链接写出主题目录"
        );
    }
    fs::create_dir_all(&target).context("无法创建主题文件夹")?;
    let root = fs::canonicalize(target)?;
    for (ix, name) in names.iter().enumerate() {
        let Some(relative) = name.strip_prefix(&prefix) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        let mut entry = archive.by_index(ix).context("无法读取 ZIP 条目")?;
        // These are ordinary extraction write boundaries, never theme validation.
        entry.enclosed_name().context("ZIP 条目不能写出主题目录")?;
        let relative = Path::new(relative);
        ensure!(
            relative.components().all(|part| matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )),
            "ZIP 条目不能写出主题文件夹"
        );
        let destination = root.join(relative);
        let mut ancestor = destination.as_path();
        loop {
            if ancestor.exists() {
                ensure!(
                    fs::canonicalize(ancestor)?.starts_with(&root),
                    "ZIP 条目不能通过链接写出主题目录"
                );
            }
            if ancestor == root {
                break;
            }
            ancestor = ancestor.parent().context("ZIP 条目路径无效")?;
        }
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            fs::create_dir_all(destination.parent().context("ZIP 条目路径无效")?)?;
            // Do not create archive-provided symlinks; extract their bytes like a file.
            let mut output = File::create(&destination).context("无法写入解压文件")?;
            io::copy(&mut entry, &mut output).context("ZIP 解压失败")?;
        }
    }
    Ok(())
}

fn theme_folder_name(name: &str) -> Result<String> {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_end_matches(['.', ' ']);
    ensure!(
        !name.is_empty(),
        "manifest.json 中的 themeId 无法作为文件夹名"
    );
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
        });
    Ok(if reserved {
        format!("_{name}")
    } else {
        name.into()
    })
}

#[cfg(test)]
mod tests;
