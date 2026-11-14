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
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) const DEBUG_PORT: u16 = 9347;
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
        .map_err(|_| {
            anyhow::anyhow!(crate::tr!(
                "themes.could_not_verify_the_codex_process_identity"
            ))
        })?;
    #[cfg(target_os = "windows")]
    {
        identity::verify_listener(DEBUG_PORT, expected.as_deref())
    }
    #[cfg(not(target_os = "windows"))]
    {
        drop(expected);
        Ok(())
    }
}

/// Only the user's existing Open/Restart command consumes these arguments.
pub(crate) fn launch_arguments() -> Result<Vec<OsString>> {
    let profile = PROFILE.get().context(crate::tr!(
        "themes.the_theme_runtime_directory_is_not_initialized"
    ))?;
    fs::create_dir_all(profile).context(crate::tr!(
        "themes.could_not_create_the_codex_browser_data_directory_for"
    ))?;
    Ok(debug_arguments(profile))
}

fn debug_arguments(profile: &Path) -> Vec<OsString> {
    vec![
        "--remote-debugging-address=127.0.0.1".into(),
        format!("--remote-debugging-port={DEBUG_PORT}").into(),
        format!("--user-data-dir={}", profile.display()).into(),
    ]
}

pub(crate) fn verify_launch_endpoint() -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if cdp::is_available() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    anyhow::bail!(crate::tr!(
        "themes.the_app_opened_but_its_theme_debugging_connection_could"
    ))
}

pub(crate) fn has_debug_endpoint() -> bool {
    cdp::is_available()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ThemeStatus {
    #[default]
    Off,
    WaitingForConnection,
    WaitingForWindow,
    Applied(usize),
    Failed(String),
}

impl ThemeStatus {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Off => crate::tr!("themes.theme_off_the_theme_worker_has_stopped").into(),
            Self::WaitingForConnection => {
                crate::tr!("themes.theme_selected_waiting_for_the_codex_debugging_connection_open")
                    .into()
            }
            Self::WaitingForWindow => {
                crate::tr!("themes.debugging_connected_waiting_for_the_main_codex_window").into()
            }
            Self::Applied(windows) => {
                crate::tr!("themes.theme_applied_to_codex_windows_without_restarting"; windows = windows)
            }
            Self::Failed(error) => crate::tr!("themes.could_not_apply_theme"; error = error),
        }
    }
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
    pub status: ThemeStatus,
}
impl Default for ThemeSnapshot {
    fn default() -> Self {
        Self {
            themes: Vec::new(),
            selected: None,
            transparency: 30,
            status: ThemeStatus::Off,
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
    status: Arc<Mutex<ThemeStatus>>,
}
enum WorkerCommand {
    Update(String, mpsc::Sender<Result<ThemeStatus>>),
    Stop,
    Detach,
}
struct Worker {
    commands: mpsc::Sender<WorkerCommand>,
    thread: Option<JoinHandle<Result<()>>>,
}
impl Worker {
    fn update(&self, script: String) -> Result<ThemeStatus> {
        let (reply, receiver) = mpsc::channel();
        self.commands
            .send(WorkerCommand::Update(script, reply))
            .context(crate::tr!("themes.the_theme_worker_exited"))?;
        receiver.recv().context(crate::tr!(
            "themes.the_theme_worker_returned_no_application_result"
        ))?
    }
    fn stop(mut self) -> Result<()> {
        self.finish(WorkerCommand::Stop)
    }
    fn detach(mut self) -> Result<()> {
        self.finish(WorkerCommand::Detach)
    }
    fn finish(&mut self, command: WorkerCommand) -> Result<()> {
        let _ = self.commands.send(command);
        self.thread.take().unwrap().join().map_err(|_| {
            anyhow::anyhow!(crate::tr!("themes.the_theme_worker_exited_unexpectedly"))
        })?
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.detach();
        }
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
                status: Arc::new(Mutex::new(ThemeStatus::Off)),
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
            .map_err(|_| anyhow::anyhow!(crate::tr!("themes.could_not_read_theme_state")))?;
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
                library.report(crate::tr!("themes.could_not_restore_the_theme"; error = format!("{:#}", error)));
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
            status: library
                .status
                .lock()
                .map_err(|_| {
                    anyhow::anyhow!(crate::tr!(
                        "themes.could_not_read_the_theme_application_status"
                    ))
                })?
                .clone(),
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
                crate::tr!("themes.the_theme_directory_no_longer_exists_refresh_and_try")
            );
        }
        let mut library = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!(crate::tr!("themes.could_not_update_theme_state")))?;
        library.restored = true;
        if let Some(id) = id {
            let transparency = library.transparencies.get(&id).copied();
            let script = payload::build(Path::new(&id), transparency)?;
            let status = library.activate(id, script)?;
            Ok(status.message())
        } else {
            library.disable()?;
            library.save()?;
            Ok(crate::tr!("themes.theme_off_the_theme_worker_has_stopped").into())
        }
    }

    pub(crate) fn set_transparency(&self, value: u8) -> Result<String> {
        ensure!(
            value <= 100,
            crate::tr!("themes.transparency_must_be_between_0_and_100")
        );
        let mut library = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!(crate::tr!("themes.could_not_update_theme_state")))?;
        let id = library
            .selected
            .clone()
            .context(crate::tr!("themes.enable_a_theme_first"))?;
        let script = payload::build(Path::new(&id), Some(value))?;
        let previous = library.transparencies.insert(id.clone(), value);
        let status = match library.activate(id.clone(), script) {
            Ok(status) => status,
            Err(error) => {
                match previous {
                    Some(previous) => {
                        library.transparencies.insert(id, previous);
                    }
                    None => {
                        library.transparencies.remove(&id);
                    }
                }
                library.save()?;
                return Err(error);
            }
        };
        Ok(if matches!(status, ThemeStatus::Applied(_)) {
            crate::tr!("themes.background_transparency_set_to"; value = value)
        } else {
            crate::tr!("themes.background_transparency_saved_as"; value = value, arg0 = status.message())
        })
    }

    pub(crate) fn import_zip(&self, directory: &str, path: &Path) -> Result<String> {
        let root = self.directory(directory)?;
        extract_zip(path, &root)?;
        Ok(crate::tr!("themes.the_theme_zip_was_extracted_to_the_theme_directory").into())
    }

    pub(crate) fn import_files(&self, directory: &str, paths: &[PathBuf]) -> Result<String> {
        ensure!(
            !paths.is_empty(),
            crate::tr!("themes.select_theme_zips_or_background_images")
        );
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
            crate::tr!("themes.imported_failed_n_n"; imported = imported, arg0 = failures.len(), arg1 = failures.join("\n\n"))
        );
        Ok(crate::tr!("themes.imported_themes_select_one_from_the_dropdown"; imported = imported))
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.inner.try_lock().ok()?.errors.try_lock().ok()?.take()
    }

    /// Disconnect before process exit, preserving the current page and saved settings.
    pub(crate) fn shutdown(&self) -> Result<()> {
        let mut library = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!(crate::tr!("themes.could_not_stop_the_theme_worker")))?;
        library.worker.take().map(Worker::detach).transpose()?;
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
        let saved: Value = serde_json::from_slice(&fs::read(&self.state_path)?)
            .context(crate::tr!("themes.invalid_theme_state_file_format"))?;
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
            self.selected = Some(id.into());
            self.worker = Some(start_worker(self.errors.clone(), self.status.clone())?);
            self.worker.as_ref().unwrap().update(script)?;
        }
        Ok(())
    }
    fn save(&self) -> Result<()> {
        let parent = self
            .state_path
            .parent()
            .context(crate::tr!("themes.invalid_theme_state_path"))?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(
            &mut file,
            &json!({"selected": self.selected, "transparency": self.transparencies}),
        )?;
        file.flush()?;
        file.persist(&self.state_path)
            .map_err(|error| error.error)
            .context(crate::tr!("themes.could_not_save_theme_state"))?;
        Ok(())
    }
    fn disable(&mut self) -> Result<()> {
        // Even when renderer cleanup reports an error, no worker may survive Off.
        self.selected = None;
        let result = self.worker.take().map(Worker::stop).transpose();
        *self.status.lock().map_err(|_| {
            anyhow::anyhow!(crate::tr!(
                "themes.could_not_update_theme_application_status"
            ))
        })? = ThemeStatus::Off;
        *self.errors.lock().map_err(|_| {
            anyhow::anyhow!(crate::tr!("themes.could_not_clear_the_theme_error_state"))
        })? = None;
        if let Err(error) = result {
            self.save()?;
            return Err(error);
        }
        Ok(())
    }
    fn activate(&mut self, id: String, script: String) -> Result<ThemeStatus> {
        let previous = self.selected.clone();
        // Persist first so a disk failure cannot leave an unrecorded new theme.
        self.selected = Some(id);
        if let Err(error) = self.save() {
            self.selected = previous;
            return Err(error);
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.thread.as_ref().is_some_and(JoinHandle::is_finished))
        {
            let _ = self.worker.take().unwrap().stop();
        }
        if self.worker.is_none() {
            match start_worker(self.errors.clone(), self.status.clone()) {
                Ok(worker) => {
                    self.worker = Some(worker);
                }
                Err(error) => {
                    self.selected = None;
                    let _ = self.save();
                    return Err(error);
                }
            }
        }
        self.worker.as_ref().unwrap().update(script)
    }
}

fn start_worker(
    errors: Arc<Mutex<Option<String>>>,
    status: Arc<Mutex<ThemeStatus>>,
) -> Result<Worker> {
    let (commands, receiver) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("codex-theme".into())
        .spawn(move || run_worker(receiver, errors, status, cdp::Runtime::new()))
        .context(crate::tr!("themes.could_not_start_the_theme_worker"))?;
    Ok(Worker {
        commands,
        thread: Some(thread),
    })
}

fn run_worker(
    receiver: mpsc::Receiver<WorkerCommand>,
    errors: Arc<Mutex<Option<String>>>,
    status: Arc<Mutex<ThemeStatus>>,
    mut connection: cdp::Runtime,
) -> Result<()> {
    let mut last_error = None;
    let mut script = None;
    loop {
        let reply = match receiver.recv_timeout(Duration::from_millis(1500)) {
            Ok(WorkerCommand::Update(source, reply)) => {
                script = Some(source);
                Some(reply)
            }
            Ok(WorkerCommand::Stop) => break,
            // Closing a CDP socket does not undo styles already in the page.
            // Only an explicit Off command should execute renderer cleanup.
            Ok(WorkerCommand::Detach) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
        };
        let Some(script) = script.as_deref() else {
            continue;
        };
        let result = if reply.is_some() {
            connection.replace(script)
        } else {
            connection.refresh(script)
        };
        let next_status = match &result {
            Ok(status) => status.clone(),
            Err(error) => ThemeStatus::Failed(format!("{error:#}")),
        };
        let next_error = match &next_status {
            ThemeStatus::Failed(error) => Some(error.clone()),
            _ => None,
        };
        if let Ok(mut slot) = errors.lock() {
            if next_error.is_none() || reply.is_some() {
                *slot = None;
            } else if next_error != last_error {
                *slot = next_error
                    .as_ref()
                    .map(|error| crate::tr!("themes.could_not_apply_theme"; error = error));
            }
        }
        last_error = next_error;
        if let Ok(mut status) = status.lock() {
            *status = next_status;
        }
        if let Some(reply) = reply {
            let _ = reply.send(result);
        }
    }
    let result = connection.clear();
    if let Ok(mut status) = status.lock() {
        *status = ThemeStatus::Off;
    }
    result
}

fn theme_directory(configured: &str) -> Result<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if configured.is_empty() {
        default_theme_directory(&std::env::current_exe()?)
    } else {
        Ok(std::path::absolute(configured)?)
    }
}

fn default_theme_directory(executable: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    if executable
        .parent()
        .is_some_and(|p| p.ends_with("Contents/MacOS"))
        && executable
            .ancestors()
            .nth(3)
            .is_some_and(|p| p.extension().is_some_and(|ext| ext == "app"))
    {
        // Keep imported themes outside the signed application bundle.
        let home = std::env::var_os("HOME").context(crate::tr!(
            "single_instance.could_not_find_your_home_directory"
        ))?;
        return Ok(PathBuf::from(home).join("Library/Application Support/Codex Switch/Themes"));
    }
    Ok(executable
        .parent()
        .context(crate::tr!("themes.could_not_locate_the_program_directory"))?
        .join("Themes"))
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
        let canonical = fs::canonicalize(&path)
            .context(crate::tr!("themes.could_not_read_the_theme_directory"))?;
        if !seen.insert(canonical) {
            continue;
        }
        if path.join("theme.json").is_file() {
            themes.push(ThemeEntry {
                id: std::path::absolute(&path)?.to_string_lossy().into_owned(),
                name: payload::theme_name(&path),
            });
        }
        for entry in
            fs::read_dir(&path).context(crate::tr!("themes.could_not_list_the_theme_directory"))?
        {
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
    let mut archive = zip::ZipArchive::new(
        File::open(path).context(crate::tr!("themes.could_not_open_the_zip_file"))?,
    )
    .context(crate::tr!("themes.could_not_extract_the_zip_file"))?;
    let mut names = Vec::with_capacity(archive.len());
    let mut first_directory = None;
    for ix in 0..archive.len() {
        let entry = archive
            .by_index(ix)
            .context(crate::tr!("themes.could_not_read_a_zip_entry"))?;
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
            let prefix = first_directory.context(crate::tr!(
                "themes.no_manifest_json_was_found_at_the_zip_root"
            ))?;
            let manifest = format!("{prefix}manifest.json");
            let index = names
                .iter()
                .position(|name| name == &manifest)
                .context(crate::tr!("themes.no_manifest_json_was_found_in_the_zip_s"))?;
            (prefix, index)
        };
    let manifest: Value = {
        let mut entry = archive.by_index(manifest_index)?;
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut entry, &mut bytes)
            .context(crate::tr!("themes.could_not_read_manifest_json"))?;
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .context(crate::tr!("themes.manifest_json_is_not_valid_json"))?
    };
    let name = manifest["themeId"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .context(crate::tr!(
            "themes.manifest_json_must_contain_a_nonempty_themeid"
        ))?;
    let folder = theme_folder_name(name)?;
    fs::create_dir_all(root).context(crate::tr!(
        "image_import.could_not_create_the_theme_directory"
    ))?;
    let root = fs::canonicalize(root)?;
    let target = root.join(folder);
    if target.exists() {
        ensure!(
            fs::canonicalize(&target)?.starts_with(&root),
            crate::tr!("themes.the_theme_folder_cannot_write_outside_the_theme_directory")
        );
    }
    fs::create_dir_all(&target).context(crate::tr!("themes.could_not_create_the_theme_folder"))?;
    let root = fs::canonicalize(target)?;
    for (ix, name) in names.iter().enumerate() {
        let Some(relative) = name.strip_prefix(&prefix) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        let mut entry = archive
            .by_index(ix)
            .context(crate::tr!("themes.could_not_read_a_zip_entry"))?;
        // These are ordinary extraction write boundaries, never theme validation.
        entry.enclosed_name().context(crate::tr!(
            "themes.a_zip_entry_cannot_write_outside_the_theme_directory"
        ))?;
        let relative = Path::new(relative);
        ensure!(
            relative.components().all(|part| matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )),
            crate::tr!("themes.a_zip_entry_cannot_write_outside_its_theme_folder")
        );
        let destination = root.join(relative);
        let mut ancestor = destination.as_path();
        loop {
            if ancestor.exists() {
                ensure!(
                    fs::canonicalize(ancestor)?.starts_with(&root),
                    crate::tr!("themes.a_zip_entry_cannot_write_outside_the_theme_directory_2")
                );
            }
            if ancestor == root {
                break;
            }
            ancestor = ancestor
                .parent()
                .context(crate::tr!("themes.invalid_zip_entry_path"))?;
        }
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            fs::create_dir_all(
                destination
                    .parent()
                    .context(crate::tr!("themes.invalid_zip_entry_path"))?,
            )?;
            // Do not create archive-provided symlinks; extract their bytes like a file.
            let mut output = File::create(&destination)
                .context(crate::tr!("themes.could_not_write_an_extracted_file"))?;
            io::copy(&mut entry, &mut output)
                .context(crate::tr!("themes.zip_extraction_failed"))?;
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
        crate::tr!("themes.the_themeid_in_manifest_json_cannot_be_used_as")
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
