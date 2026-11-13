//! Named configuration folders with timestamped, immutable backup versions.
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, Utc};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::NamedTempFile;

type ActiveFiles = [Option<Vec<u8>>; 2];

#[derive(Clone, Default)]
pub(crate) struct Summary {
    pub model: String,
    pub provider: String,
}

#[derive(Clone)]
pub(crate) struct Profile {
    pub key: String,
    pub name: String,
    pub time: String,
    pub latest_stamp: Option<u128>,
    pub version_count: usize,
    pub summary: Summary,
    pub problem: Option<String>,
    pub is_current: bool,
    pub is_dirty: bool,
    pub versions: Vec<BackupVersion>,
}

#[derive(Clone)]
pub(crate) struct BackupVersion {
    pub id: String,
    pub time: String,
    pub summary: Summary,
    pub problem: Option<String>,
    pub is_current: bool,
}

#[derive(Clone)]
pub(crate) struct Settings {
    pub max_versions: usize,
    pub chatgpt_app_path: String,
    pub launch_at_login: bool,
    pub exit_on_close: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_versions: 10,
            chatgpt_app_path: String::new(),
            launch_at_login: false,
            exit_on_close: false,
        }
    }
}

impl Settings {
    pub fn app_paths(&self) -> [String; 1] {
        [self.chatgpt_app_path.clone()]
    }
}

#[derive(Default)]
pub(crate) struct Snapshot {
    pub profiles: Vec<Profile>,
    pub current_name: String,
    pub current: Summary,
    pub has_config: bool,
    pub has_auth: bool,
    pub warning: Option<String>,
    pub settings: Settings,
}

struct Version {
    id: String,
    folder: PathBuf,
    stamp: u128,
}

impl Version {
    fn files(&self) -> Result<ActiveFiles> {
        Ok([
            read_optional(&self.folder.join("config.toml"))?,
            read_optional(&self.folder.join("auth.json"))?,
        ])
    }
}

#[derive(Clone)]
pub(crate) struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub fn profile_directory(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        let path = self.backups().join(name);
        let metadata = fs::symlink_metadata(&path).context("配置目录已不存在，请刷新列表")?;
        if !metadata.is_dir() || metadata.is_symlink() {
            bail!("配置目录无效，请刷新列表。");
        }
        Ok(path)
    }

    pub fn version_directory(&self, name: &str, id: &str) -> Result<PathBuf> {
        anyhow::ensure!(
            !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
            "备份版本标识无效"
        );
        let folder = self.profile_directory(name)?;
        let path = folder.join(id);
        let metadata = fs::symlink_metadata(&path).context("备份版本已不存在，请刷新后重试")?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.is_symlink(),
            "备份版本目录无效"
        );
        let parent = fs::canonicalize(&folder).context("无法确认配置目录")?;
        let resolved = fs::canonicalize(&path).context("无法确认版本目录")?;
        anyhow::ensure!(
            resolved.parent() == Some(parent.as_path()),
            "备份版本目录超出预期范围"
        );
        Ok(path)
    }

    fn groups(&self) -> Result<BTreeMap<String, Vec<Version>>> {
        let mut groups = BTreeMap::new();
        let entries = match fs::read_dir(self.backups()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(groups),
            Err(error) => return Err(error).context("无法读取备份目录"),
        };
        for entry in entries {
            let entry = entry.context("无法读取配置目录")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_dir()
                || name.starts_with('.')
                || validate_name(&name).is_err()
            {
                continue;
            }
            let mut versions = Vec::new();
            for version in fs::read_dir(entry.path()).context("无法读取配置的备份版本")?
            {
                let version = version?;
                let id = version.file_name().to_string_lossy().into_owned();
                if !version.file_type()?.is_dir()
                    || id.is_empty()
                    || !id.bytes().all(|b| b.is_ascii_digit())
                {
                    continue;
                }
                if let Ok(stamp) = id.parse::<u128>() {
                    versions.push(Version {
                        id,
                        folder: version.path(),
                        stamp,
                    });
                }
            }
            versions.sort_by(|a, b| b.stamp.cmp(&a.stamp).then_with(|| b.id.cmp(&a.id)));
            groups.insert(name, versions);
        }
        Ok(groups)
    }

    // Only one folder owns the active configuration. Identical copies elsewhere
    // never acquire a second active marker; content matching is only a fallback.
    fn current_group(
        &self,
        groups: &BTreeMap<String, Vec<Version>>,
        files: &ActiveFiles,
    ) -> Result<Option<String>> {
        if files.iter().all(Option::is_none) {
            return Ok(None);
        }
        if self
            .read_state()?
            .as_ref()
            .is_some_and(|s| s["profile"].as_str() == Some(""))
        {
            return Ok(None);
        }
        if let Some(name) = self
            .read_state()?
            .as_ref()
            .and_then(|value| value["profile"].as_str())
            && groups.contains_key(name)
        {
            return Ok(Some(name.into()));
        }
        let mut matches = Vec::new();
        if files.iter().all(Option::is_some) {
            for (name, versions) in groups {
                if let Some(version) = versions
                    .iter()
                    .find(|version| version.files().is_ok_and(|saved| &saved == files))
                {
                    matches.push((version.stamp, name));
                }
            }
        }
        matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        Ok(matches.first().map(|(_, name)| (*name).clone()))
    }

    pub fn load(&self) -> Result<Snapshot> {
        let files = self.active_files()?;
        let groups = self.groups()?;
        let active_group = self.current_group(&groups, &files)?;
        let mut snapshot = Snapshot {
            settings: self.settings()?,
            has_config: files[0].is_some(),
            has_auth: files[1].is_some(),
            current_name: active_group
                .clone()
                .unwrap_or_else(|| "尚未归入配置目录".into()),
            ..Default::default()
        };
        if let Some(bytes) = &files[0] {
            match summarize(bytes) {
                Ok(summary) => snapshot.current = summary,
                Err(_) => {
                    snapshot.warning = Some("当前 config.toml 格式无效，仍可备份原文件。".into())
                }
            }
        }
        if files[1].as_ref().is_some_and(|bytes| !valid_auth(bytes)) {
            snapshot.warning = Some("当前 auth.json 格式无效，仍可备份原文件。".into());
        }
        for (name, versions) in &groups {
            let latest = versions.first();
            let saved = latest
                .map(Version::files)
                .unwrap_or_else(|| Ok([None, None]));
            let summary = saved
                .as_ref()
                .ok()
                .and_then(|pair| pair[0].as_deref())
                .and_then(|bytes| summarize(bytes).ok());
            let problem = match &saved {
                Err(_) => Some("无法读取备份文件".into()),
                Ok(_) if latest.is_none() => Some("暂无备份".into()),
                Ok(pair) if pair[0].is_none() => Some("缺少 config.toml".into()),
                Ok(_) if summary.is_none() => Some("config.toml 无效".into()),
                Ok(pair) if pair[1].is_none() => Some("缺少 auth.json".into()),
                Ok(pair) if !valid_auth(pair[1].as_ref().unwrap()) => Some("auth.json 无效".into()),
                _ => None,
            };
            let is_current = active_group.as_ref() == Some(name);
            let recorded = self.read_state()?;
            let current_version = if is_current {
                let recorded_id = recorded.as_ref().and_then(|s| s["version"].as_str());
                versions
                    .iter()
                    .find(|v| {
                        Some(v.id.as_str()) == recorded_id && v.files().is_ok_and(|f| f == files)
                    })
                    .or_else(|| {
                        versions
                            .iter()
                            .find(|v| v.files().is_ok_and(|f| f == files))
                    })
            } else {
                None
            };
            let history = versions
                .iter()
                .map(|version| {
                    let saved = version.files();
                    let summary = saved
                        .as_ref()
                        .ok()
                        .and_then(|f| f[0].as_deref())
                        .and_then(|bytes| summarize(bytes).ok());
                    let problem = match &saved {
                        Err(_) => Some("无法读取备份文件".into()),
                        Ok(f) if f[0].is_none() || f[1].is_none() => Some("备份文件不完整".into()),
                        Ok(_) if summary.is_none() => Some("config.toml 无效".into()),
                        Ok(f) if !valid_auth(f[1].as_ref().unwrap()) => {
                            Some("auth.json 无效".into())
                        }
                        _ => None,
                    };
                    BackupVersion {
                        id: version.id.clone(),
                        time: display_time(version.stamp),
                        summary: summary.unwrap_or_default(),
                        problem,
                        is_current: current_version.is_some_and(|v| v.id == version.id),
                    }
                })
                .collect();
            snapshot.profiles.push(Profile {
                key: name.clone(),
                name: name.clone(),
                time: latest
                    .map(|v| display_time(v.stamp))
                    .unwrap_or_else(|| "尚无版本".into()),
                latest_stamp: latest.map(|v| v.stamp),
                version_count: versions.len(),
                summary: summary.unwrap_or_default(),
                problem,
                is_current,
                is_dirty: is_current && saved.as_ref().ok() != Some(&files),
                versions: history,
            });
        }
        Ok(snapshot)
    }

    pub fn read_config(&self) -> Result<String> {
        let bytes = read_optional(&self.root.join("config.toml"))?
            .context("config.toml 已不存在，请刷新后重试")?;
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("config.toml 不是有效的 UTF-8 文件"))
    }

    pub fn save_config(&self, original: &str, edited: &str) -> Result<String> {
        if let Err(error) = toml::from_str::<toml::Table>(edited) {
            // Report the location without echoing source lines that may contain credentials.
            let offset = error.span().map_or(0, |span| span.start).min(edited.len());
            let prefix = &edited.as_bytes()[..offset];
            let line = prefix.iter().filter(|&&byte| byte == b'\n').count() + 1;
            let column = String::from_utf8_lossy(
                prefix
                    .rsplit(|&byte| byte == b'\n')
                    .next()
                    .unwrap_or_default(),
            )
            .chars()
            .count()
                + 1;
            bail!("TOML 格式无效，请检查第 {line} 行、第 {column} 列附近的内容。");
        }
        let _lock = self.lock()?;
        let path = self.root.join("config.toml");
        anyhow::ensure!(
            read_optional(&path)?.as_deref() == Some(original.as_bytes()),
            "config.toml 在编辑期间已被其他程序修改或删除，请重新打开后再编辑；本次修改尚未写入。"
        );
        replace_all(vec![(path.clone(), edited.as_bytes().to_vec())], |_| {
            if fs::read(&path)? != original.as_bytes() {
                return Err(io::Error::other(
                    "config.toml 在保存前发生了变化，请重新打开后重试",
                ));
            }
            Ok(())
        })?;
        Ok("已保存 config.toml。可重新打开 ChatGPT 或新建会话加载配置。".into())
    }

    pub fn create_profile(&self, name: &str) -> Result<String> {
        let name = name.trim();
        validate_name(name)?;
        let _lock = self.lock()?;
        let files = self.active_files()?;
        ensure_backup_files(&files)?;
        let folder = self.backups().join(name);
        fs::create_dir(&folder).context("无法新建配置目录，名称可能已存在")?;
        self.save_current(name, &files)
    }

    pub fn backup(&self, name: &str) -> Result<String> {
        let _lock = self.lock()?;
        self.profile_directory(name)?;
        let files = self.active_files()?;
        ensure_backup_files(&files)?;
        self.save_current(name, &files)
    }

    fn save_current(&self, name: &str, files: &ActiveFiles) -> Result<String> {
        let version = self.save_version(name, files)?;
        if self.active_files()? != *files {
            bail!("备份已保存，但当前文件同时发生了变化；请刷新后重试。");
        }
        self.write_state(name, &version)
            .context("备份已保存，但无法更新当前配置记录")?;
        Ok(format!(
            "已保存到 {name}{}",
            if files[0].is_none() {
                "（仅认证文件）"
            } else if files[1].is_none() {
                "（缺少认证文件）"
            } else {
                ""
            }
        ))
    }

    pub fn switch(&self, name: &str) -> Result<String> {
        self.switch_to(name, None)
    }

    pub fn switch_version(&self, name: &str, version: &str) -> Result<String> {
        self.switch_to(name, Some(version))
    }

    fn switch_to(&self, name: &str, version: Option<&str>) -> Result<String> {
        let _lock = self.lock()?;
        self.profile_directory(name)?;
        let groups = self.groups()?;
        let selected = groups
            .get(name)
            .and_then(|versions| match version {
                Some(id) => versions.iter().find(|v| v.id == id),
                None => versions.first(),
            })
            .context("备份版本已不存在，请刷新后重试")?;
        let target = selected.files()?;
        let config = target[0]
            .as_ref()
            .context("所选备份缺少 config.toml，未切换配置")?;
        let auth = target[1]
            .as_ref()
            .context("所选备份缺少 auth.json，未切换配置")?;
        summarize(config)
            .map_err(|_| anyhow::anyhow!("所选备份的 config.toml 格式无效，未切换配置。"))?;
        if !valid_auth(auth) {
            bail!("所选备份的 auth.json 格式无效，未切换配置。");
        }
        let old = self.active_files()?;
        // Switching between identical copies still changes the owning folder.
        if old == target {
            self.write_state(name, &selected.id)?;
            return Ok(format!("当前配置已关联到 {name}。"));
        }
        let current_group = self.current_group(&groups, &old)?;
        let auto_backup = if old.iter().any(Option::is_some) {
            let mut previous = current_group.unwrap_or_else(|| "切换前自动备份".into());
            if previous == name {
                previous = if name == "切换前自动备份" {
                    "切换前恢复备份"
                } else {
                    "切换前自动备份"
                }
                .into();
            }
            let folder = self.backups().join(&previous);
            if !folder.try_exists()? {
                fs::create_dir(&folder).context("无法创建自动备份目录")?;
            }
            let version = self.save_version(&previous, &old)?;
            Some(format!("{previous}/{version}"))
        } else {
            None
        };
        if self.active_files()? != old {
            bail!("当前配置在切换前发生了变化，请刷新后重试；自动备份已保留。");
        }
        let changes = vec![
            (self.root.join("config.toml"), config.clone()),
            (self.root.join("auth.json"), auth.clone()),
            (
                self.backups().join("current.json"),
                self.state_bytes(name, &selected.id)?,
            ),
        ];
        replace_all(changes, |_| Ok(())).with_context(|| match &auto_backup {
            Some(path) => format!("切换失败，切换前的文件保留在 backups/{path}"),
            None => "切换失败".into(),
        })?;
        Ok(format!(
            "已切换到 {name}（{}）。重新打开 ChatGPT 或新建会话以加载配置。",
            display_time(selected.stamp)
        ))
    }

    fn settings(&self) -> Result<Settings> {
        let Some(bytes) = read_optional(&self.backups().join("settings.json"))? else {
            return Ok(Settings::default());
        };
        let value: Value = serde_json::from_slice(&bytes).context("全局设置格式无效")?;
        let count = value["maxVersions"].as_u64().context("备份保留数量无效")?;
        anyhow::ensure!((1..=1000).contains(&count), "备份保留数量须为 1–1000");
        Ok(Settings {
            max_versions: count as usize,
            chatgpt_app_path: value["chatgptAppPath"].as_str().unwrap_or_default().into(),
            launch_at_login: value["launchAtLogin"].as_bool().unwrap_or(false),
            exit_on_close: value["exitOnClose"].as_bool().unwrap_or(false),
        })
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<String> {
        anyhow::ensure!(
            (1..=1000).contains(&settings.max_versions),
            "备份保留数量须为 1–1000"
        );
        crate::desktop::validate_paths(&settings.app_paths())?;
        let _lock = self.lock()?;
        // Keep unknown keys so future global settings can evolve independently.
        let path = self.backups().join("settings.json");
        let mut value = read_optional(&path)?
            .map(|bytes| serde_json::from_slice::<Value>(&bytes))
            .transpose()
            .context("全局设置格式无效")?
            .unwrap_or_else(|| json!({}));
        anyhow::ensure!(value.is_object(), "全局设置格式无效");
        value["maxVersions"] = json!(settings.max_versions);
        value
            .as_object_mut()
            .context("全局设置格式无效")?
            .remove("codexAppPath");
        value["chatgptAppPath"] = json!(settings.chatgpt_app_path);
        value["launchAtLogin"] = json!(settings.launch_at_login);
        value["exitOnClose"] = json!(settings.exit_on_close);
        let bytes = serde_json::to_vec_pretty(&value)?;
        let startup = crate::startup::apply(settings.launch_at_login, self.root())?;
        if let Err(error) = replace_all(vec![(path, bytes)], |_| Ok(())) {
            if let Err(rollback) = startup.rollback() {
                bail!("设置保存失败：{error:#}；无法恢复开机启动设置：{rollback:#}");
            }
            return Err(error).context("设置未保存，开机启动设置已恢复");
        }
        for name in self.groups()?.keys() {
            self.prune_versions(name, settings.max_versions)
                .context("设置已保存，但清理旧版本失败；请检查目录权限后重试")?;
        }
        Ok("设置已保存".to_string())
    }

    pub fn delete_version(&self, name: &str, id: &str) -> Result<String> {
        let _lock = self.lock()?;
        let parent = self.profile_directory(name)?;
        let groups = self.groups()?;
        let version = groups
            .get(name)
            .and_then(|versions| versions.iter().find(|v| v.id == id))
            .context("备份版本已不存在，请刷新后重试")?;
        remove_managed_directory(&version.folder, &parent)?;
        // The active files remain in place, even when their saved copy is deleted.
        Ok(format!(
            "已删除 {name} 的 {} 版本，当前配置文件未改动。",
            display_time(version.stamp)
        ))
    }

    pub fn delete_profiles(&self, names: &[String]) -> Result<String> {
        let _lock = self.lock()?;
        anyhow::ensure!(!names.is_empty(), "请选择要删除的备份目录");
        let names: std::collections::BTreeSet<_> = names.iter().collect();
        // Resolve all targets before beginning a batch; each removal also checks
        // the managed root and rejects links in remove_managed_directory.
        for name in &names {
            self.profile_directory(name)?;
        }
        let mut deleted = 0;
        let mut failures = Vec::new();
        for name in names {
            match self.delete_profile_locked(name) {
                Ok(()) => deleted += 1,
                Err(error) => failures.push(format!("{name}：{error:#}")),
            }
        }
        if !failures.is_empty() {
            bail!(
                "已完成删除 {deleted} 个目录；以下目录删除未完成：{}。请刷新列表后重试，当前配置文件未改动。",
                failures.join("；")
            );
        }
        Ok(format!(
            "已删除 {deleted} 个备份目录及其所有版本，当前配置文件未改动。"
        ))
    }

    fn delete_profile_locked(&self, name: &str) -> Result<()> {
        let folder = self.profile_directory(name)?;
        remove_managed_directory(&folder, &self.backups())?;
        if self
            .read_state()?
            .as_ref()
            .and_then(|s| s["profile"].as_str())
            == Some(name)
        {
            // An explicit unassigned state prevents identical copies from taking ownership.
            self.write_state("", "")
                .context("备份目录已删除，但无法清除当前归属记录")?;
        }
        Ok(())
    }

    fn prune_versions(&self, name: &str, limit: usize) -> Result<()> {
        let folder = self.profile_directory(name)?;
        if let Some(versions) = self.groups()?.get(name) {
            for version in versions.iter().skip(limit) {
                remove_managed_directory(&version.folder, &folder)?;
            }
        }
        Ok(())
    }

    fn state_bytes(&self, name: &str, version: &str) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(&json!({
            "schemaVersion": 2, "profile": name, "version": version,
            "updatedAt": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }))?)
    }

    fn write_state(&self, name: &str, version: &str) -> Result<()> {
        replace_all(
            vec![(
                self.backups().join("current.json"),
                self.state_bytes(name, version)?,
            )],
            |_| Ok(()),
        )
    }

    fn read_state(&self) -> Result<Option<Value>> {
        Ok(read_optional(&self.backups().join("current.json"))?
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
    }

    fn active_files(&self) -> Result<ActiveFiles> {
        Ok([
            read_optional(&self.root.join("config.toml"))?,
            read_optional(&self.root.join("auth.json"))?,
        ])
    }

    fn lock(&self) -> Result<File> {
        fs::create_dir_all(self.backups()).context("无法创建备份目录")?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.backups().join(".codex-switch.lock"))
            .context("无法创建操作锁")?;
        file.try_lock()
            .context("另一项配置操作正在进行，请稍后重试")?;
        Ok(file)
    }

    fn save_version(&self, name: &str, files: &ActiveFiles) -> Result<String> {
        ensure_backup_files(files)?;
        let limit = self.settings()?.max_versions;
        let folder = self.profile_directory(name)?;
        let pending = tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(&folder)
            .context("无法准备备份目录")?;
        for (filename, bytes) in [("config.toml", &files[0]), ("auth.json", &files[1])] {
            if let Some(bytes) = bytes {
                let path = pending.path().join(filename);
                stage(&path, bytes)?
                    .persist_noclobber(&path)
                    .map_err(|error| error.error)
                    .context("无法保存备份文件")?;
            }
        }
        let newest = self
            .groups()?
            .get(name)
            .and_then(|v| v.first())
            .map_or(0, |v| v.stamp);
        let mut stamp = millis(SystemTime::now()).max(newest.saturating_add(1));
        loop {
            let version = stamp.to_string();
            let path = folder.join(&version);
            if path.try_exists()? {
                stamp += 1;
                continue;
            }
            fs::rename(pending.path(), &path).context("无法完成备份版本")?;
            self.prune_versions(name, limit)
                .context("备份已保存，但清理旧版本失败")?;
            return Ok(version);
        }
    }
}

// Only remove a real, direct child of the expected managed directory. In
// particular, never recurse into a junction or symbolic link outside backups.
fn remove_managed_directory(path: &Path, parent: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).context("备份目录已不存在")?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.is_symlink(),
        "拒绝删除链接或无效目录"
    );
    let resolved_parent = fs::canonicalize(parent).context("无法确认备份根目录")?;
    let resolved = fs::canonicalize(path).context("无法确认待删除的备份目录")?;
    anyhow::ensure!(
        resolved.parent() == Some(resolved_parent.as_path()),
        "备份目录超出预期范围，已取消删除"
    );
    fs::remove_dir_all(path).context("无法删除备份，请检查目录权限")
}

fn ensure_backup_files(files: &ActiveFiles) -> Result<()> {
    if files.iter().all(Option::is_none) {
        bail!("未找到可备份的 config.toml 或 auth.json。");
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('.') || name != sanitize_name(name) {
        bail!("名称不能为空，不能包含路径符号、保留名称、首位句点或末尾空格，最多 80 个字符。");
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("无法读取 {}", path.display())),
    }
}

fn summarize(bytes: &[u8]) -> Result<Summary> {
    // Discard parser diagnostics: they may contain source text or credentials.
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("配置编码无效"))?;
    let value: toml::Value = toml::from_str(text).map_err(|_| anyhow::anyhow!("配置格式无效"))?;
    Ok(Summary {
        model: value
            .get("model")
            .and_then(toml::Value::as_str)
            .unwrap_or("默认模型")
            .into(),
        provider: value
            .get("model_provider")
            .and_then(toml::Value::as_str)
            .unwrap_or("openai")
            .into(),
    })
}

fn valid_auth(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes).is_ok_and(|value| value.is_object())
}

fn sanitize_name(input: &str) -> String {
    let replaced: String = input
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut name = replaced
        .trim_end_matches(['.', ' '])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        name = "default".into();
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        name.insert(stem.len(), '_');
    }
    name.chars().take(80).collect()
}

fn millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn display_time(ms: u128) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .map(|date| {
            date.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "时间未知".into())
}

fn stage(path: &Path, bytes: &[u8]) -> Result<NamedTempFile> {
    let parent = path.parent().context("文件路径无效")?;
    let mut file = NamedTempFile::new_in(parent).context("无法创建临时文件")?;
    file.write_all(bytes).context("无法写入临时文件")?;
    file.as_file().sync_all().context("无法同步临时文件")?;
    Ok(file)
}

// Each replacement is atomic; an ordinary commit error restores earlier files.
// This is not crash-atomic across three files: the automatic backup is recovery.
fn replace_all(
    changes: Vec<(PathBuf, Vec<u8>)>,
    mut before_commit: impl FnMut(usize) -> io::Result<()>,
) -> Result<()> {
    let mut originals = Vec::new();
    let mut staged = Vec::new();
    for (path, bytes) in &changes {
        originals.push(read_optional(path)?);
        staged.push(stage(path, bytes)?);
    }
    for (ix, temp) in staged.into_iter().enumerate() {
        let result = before_commit(ix).and_then(|_| {
            temp.persist(&changes[ix].0)
                .map(|_| ())
                .map_err(|error| error.error)
        });
        if let Err(error) = result {
            let mut rollback_errors = Vec::new();
            for previous in (0..ix).rev() {
                let path = &changes[previous].0;
                let restored = match &originals[previous] {
                    Some(bytes) => stage(path, bytes).and_then(|temp| {
                        temp.persist(path).map(|_| ()).map_err(|e| e.error.into())
                    }),
                    None => fs::remove_file(path).map_err(Into::into),
                };
                if restored.is_err() {
                    rollback_errors.push(path.display().to_string());
                }
            }
            if !rollback_errors.is_empty() {
                bail!(
                    "写入失败，且无法回滚 {}，请从自动备份恢复。",
                    rollback_errors.join("、")
                );
            }
            return Err(error).context("写入失败，已回滚本次更改");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(directory.path().into());
        fs::write(store.root.join("config.toml"), "model = 'original'\n").unwrap();
        fs::write(
            store.root.join("auth.json"),
            r#"{"OPENAI_API_KEY":"fixture-original"}"#,
        )
        .unwrap();
        (directory, store)
    }

    #[test]
    fn repeated_backups_produce_versions_in_one_named_folder() {
        let (_directory, store) = fixture();
        store.create_profile("工作").unwrap();
        store.backup("工作").unwrap();
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.profiles.len(), 1);
        assert_eq!(snapshot.profiles[0].version_count, 2);
        assert_eq!(snapshot.profiles[0].key, "工作");
        let versions = store.groups().unwrap();
        for version in &versions["工作"] {
            assert!(version.folder.join("config.toml").is_file());
            assert!(version.folder.join("auth.json").is_file());
        }
    }

    #[test]
    fn identical_copies_have_one_owner_and_switching_changes_it() {
        let (_directory, store) = fixture();
        store.create_profile("官方").unwrap();
        store.create_profile("备用").unwrap();
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.profiles.iter().filter(|p| p.is_current).count(), 1);
        assert_eq!(snapshot.current_name, "备用");
        store.switch("官方").unwrap();
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.current_name, "官方");
        assert_eq!(snapshot.profiles.iter().filter(|p| p.is_current).count(), 1);
        fs::remove_file(store.backups().join("current.json")).unwrap();
        assert_eq!(
            store
                .load()
                .unwrap()
                .profiles
                .iter()
                .filter(|p| p.is_current)
                .count(),
            1
        );
    }

    #[test]
    fn switching_preserves_previous_files_and_restores_the_selected_latest_version() {
        let (_directory, store) = fixture();
        store.create_profile("官方").unwrap();
        fs::write(store.root.join("config.toml"), "model = 'work'\n").unwrap();
        fs::write(
            store.root.join("auth.json"),
            r#"{"OPENAI_API_KEY":"fixture-work"}"#,
        )
        .unwrap();
        store.create_profile("工作").unwrap();
        let before = store.active_files().unwrap();
        store.switch("官方").unwrap();
        assert_eq!(store.load().unwrap().current.model, "original");
        let groups = store.groups().unwrap();
        assert_eq!(groups["工作"].len(), 2);
        assert_eq!(groups["工作"][0].files().unwrap(), before);
        fs::write(store.root.join("config.toml"), "model = 'modified'\n").unwrap();
        assert!(
            store
                .load()
                .unwrap()
                .profiles
                .iter()
                .find(|p| p.name == "官方")
                .unwrap()
                .is_dirty
        );
        store.switch("官方").unwrap();
        assert_eq!(store.load().unwrap().current.model, "original");
        assert!(store.groups().unwrap().contains_key("切换前自动备份"));
    }

    #[test]
    fn invalid_latest_version_does_not_change_current_files_or_expose_content() {
        let (_directory, store) = fixture();
        store.create_profile("损坏").unwrap();
        let original = store.active_files().unwrap();
        let groups = store.groups().unwrap();
        let folder = &groups["损坏"][0].folder;
        fs::remove_file(folder.join("auth.json")).unwrap();
        assert!(store.switch("损坏").is_err());
        assert_eq!(store.active_files().unwrap(), original);
        fs::write(folder.join("auth.json"), "private invalid json").unwrap();
        assert!(!format!("{:#}", store.switch("损坏").unwrap_err()).contains("private"));
        assert_eq!(store.active_files().unwrap(), original);
        assert!(store.load().unwrap().profiles[0].problem.is_some());
    }

    #[test]
    fn new_folder_names_are_validated_and_never_overwrite_existing_folders() {
        let (_directory, store) = fixture();
        assert!(store.create_profile("../escape").is_err());
        assert!(store.create_profile("CON").is_err());
        assert!(store.create_profile(" ").is_err());
        store.create_profile("官方").unwrap();
        assert!(store.create_profile("官方").is_err());
        assert_eq!(store.groups().unwrap()["官方"].len(), 1);
        assert!(store.backup("不存在").is_err());
    }

    #[test]
    fn refresh_is_read_only_and_partial_current_files_can_be_saved() {
        let directory = tempfile::tempdir().unwrap();
        let missing = Store::new(directory.path().join("missing"));
        assert!(missing.load().unwrap().profiles.is_empty());
        assert!(!missing.root.exists());
        let (_directory, store) = fixture();
        fs::remove_file(store.root.join("config.toml")).unwrap();
        assert!(
            store
                .create_profile("仅认证")
                .unwrap()
                .contains("仅认证文件")
        );
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.profiles[0].version_count, 1);
        assert!(snapshot.profiles[0].problem.is_some());
    }

    #[test]
    fn failed_second_or_state_write_rolls_back_previous_replacements() {
        for failure_ix in [1, 2] {
            let (_directory, store) = fixture();
            let before = store.active_files().unwrap();
            let state = store.root.join("current.json");
            let result = replace_all(
                vec![
                    (
                        store.root.join("config.toml"),
                        b"model = 'replacement'".to_vec(),
                    ),
                    (store.root.join("auth.json"), b"{}".to_vec()),
                    (state.clone(), b"{}".to_vec()),
                ],
                |ix| {
                    if ix == failure_ix {
                        Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "injected failure",
                        ))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err());
            assert_eq!(store.active_files().unwrap(), before);
            assert!(!state.exists());
        }
    }

    #[test]
    fn operation_lock_prevents_concurrent_writes() {
        let (_directory, store) = fixture();
        store.create_profile("工作").unwrap();
        let lock = store.lock().unwrap();
        assert!(store.backup("工作").is_err());
        drop(lock);
        assert!(store.backup("工作").is_ok());
    }
}
