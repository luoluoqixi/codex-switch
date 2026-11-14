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
    pub language: crate::i18n::Language,
    pub max_versions: usize,
    pub chatgpt_app_path: String,
    pub theme_directory: String,
    pub launch_at_login: bool,
    pub exit_on_close: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: crate::i18n::Language::detect(),
            max_versions: 10,
            chatgpt_app_path: String::new(),
            theme_directory: String::new(),
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
        let metadata = fs::symlink_metadata(&path).context(crate::tr!(
            "store.the_profile_directory_no_longer_exists_refresh_the_list"
        ))?;
        if !metadata.is_dir() || metadata.is_symlink() {
            bail!(crate::tr!(
                "store.invalid_profile_directory_refresh_the_list"
            ));
        }
        Ok(path)
    }

    pub fn version_directory(&self, name: &str, id: &str) -> Result<PathBuf> {
        anyhow::ensure!(
            !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
            crate::tr!("store.invalid_backup_version_id")
        );
        let folder = self.profile_directory(name)?;
        let path = folder.join(id);
        let metadata = fs::symlink_metadata(&path).context(crate::tr!(
            "store.the_backup_version_no_longer_exists_refresh_and_try"
        ))?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.is_symlink(),
            crate::tr!("store.invalid_backup_version_directory")
        );
        let parent = fs::canonicalize(&folder)
            .context(crate::tr!("store.could_not_verify_the_profile_directory"))?;
        let resolved = fs::canonicalize(&path)
            .context(crate::tr!("store.could_not_verify_the_version_directory"))?;
        anyhow::ensure!(
            resolved.parent() == Some(parent.as_path()),
            crate::tr!("store.the_backup_version_directory_is_outside_the_expected_location")
        );
        Ok(path)
    }

    fn groups(&self) -> Result<BTreeMap<String, Vec<Version>>> {
        let mut groups = BTreeMap::new();
        let entries = match fs::read_dir(self.backups()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(groups),
            Err(error) => {
                return Err(error).context(crate::tr!("store.could_not_read_the_backup_directory"));
            }
        };
        for entry in entries {
            let entry = entry.context(crate::tr!("store.could_not_read_the_profile_directory"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_dir()
                || name.starts_with('.')
                || validate_name(&name).is_err()
            {
                continue;
            }
            let mut versions = Vec::new();
            for version in fs::read_dir(entry.path()).context(crate::tr!(
                "store.could_not_read_the_profile_s_backup_versions"
            ))? {
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
                .unwrap_or_else(|| crate::tr!("store.not_assigned_to_a_profile").into()),
            ..Default::default()
        };
        if let Some(bytes) = &files[0] {
            match summarize(bytes) {
                Ok(summary) => snapshot.current = summary,
                Err(_) => {
                    snapshot.warning = Some(
                        crate::tr!(
                            "store.the_current_config_toml_is_invalid_its_original_contents"
                        )
                        .into(),
                    )
                }
            }
        }
        if files[1].as_ref().is_some_and(|bytes| !valid_auth(bytes)) {
            snapshot.warning = Some(
                crate::tr!("store.the_current_auth_json_is_invalid_its_original_contents").into(),
            );
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
                Err(_) => Some(crate::tr!("store.could_not_read_backup_files").into()),
                Ok(_) if latest.is_none() => Some(crate::tr!("store.no_backups").into()),
                Ok(pair) if pair[0].is_none() => {
                    Some(crate::tr!("store.missing_config_toml").into())
                }
                Ok(_) if summary.is_none() => Some(crate::tr!("store.invalid_config_toml").into()),
                Ok(pair) if pair[1].is_none() => Some(crate::tr!("store.missing_auth_json").into()),
                Ok(pair) if !valid_auth(pair[1].as_ref().unwrap()) => {
                    Some(crate::tr!("store.invalid_auth_json").into())
                }
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
                        Err(_) => Some(crate::tr!("store.could_not_read_backup_files").into()),
                        Ok(f) if f[0].is_none() || f[1].is_none() => {
                            Some(crate::tr!("store.incomplete_backup_files").into())
                        }
                        Ok(_) if summary.is_none() => {
                            Some(crate::tr!("store.invalid_config_toml").into())
                        }
                        Ok(f) if !valid_auth(f[1].as_ref().unwrap()) => {
                            Some(crate::tr!("store.invalid_auth_json").into())
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
                    .unwrap_or_else(|| crate::tr!("store.no_versions").into()),
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
        let bytes = read_optional(&self.root.join("config.toml"))?.context(crate::tr!(
            "store.config_toml_no_longer_exists_refresh_and_try_again"
        ))?;
        String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!(crate::tr!("store.config_toml_is_not_a_valid_utf_8_file")))
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
            bail!(
                crate::tr!("store.invalid_toml_check_the_content_near_line_column"; line = line, column = column)
            );
        }
        let _lock = self.lock()?;
        let path = self.root.join("config.toml");
        anyhow::ensure!(
            read_optional(&path)?.as_deref() == Some(original.as_bytes()),
            crate::tr!("store.config_toml_was_changed_or_deleted_by_another_program")
        );
        replace_all(vec![(path.clone(), edited.as_bytes().to_vec())], |_| {
            if fs::read(&path)? != original.as_bytes() {
                return Err(io::Error::other(crate::tr!(
                    "store.config_toml_changed_before_saving_reopen_it_and_try"
                )));
            }
            Ok(())
        })?;
        Ok(crate::tr!("store.config_toml_saved_reopen_chatgpt_or_start_a_new").into())
    }

    pub fn create_profile(&self, name: &str) -> Result<String> {
        let name = name.trim();
        validate_name(name)?;
        let _lock = self.lock()?;
        let files = self.active_files()?;
        ensure_backup_files(&files)?;
        let folder = self.backups().join(name);
        fs::create_dir(&folder).context(crate::tr!(
            "store.could_not_create_the_profile_directory_the_name_may"
        ))?;
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
            bail!(crate::tr!(
                "store.the_backup_was_saved_but_the_current_files_also"
            ));
        }
        self.write_state(name, &version).context(crate::tr!(
            "store.the_backup_was_saved_but_the_current_profile_record"
        ))?;
        Ok(
            crate::tr!("store.saved_to"; name = name, arg0 = if files[0].is_none() {
                crate::tr!("store.authentication_file_only")
            } else if files[1].is_none() {
                crate::tr!("store.authentication_file_missing")
            } else {
                ""
            }),
        )
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
            .context(crate::tr!(
                "store.the_backup_version_no_longer_exists_refresh_and_try"
            ))?;
        let target = selected.files()?;
        let config = target[0].as_ref().context(crate::tr!(
            "store.the_selected_backup_has_no_config_toml_the_profile"
        ))?;
        let auth = target[1].as_ref().context(crate::tr!(
            "store.the_selected_backup_has_no_auth_json_the_profile"
        ))?;
        summarize(config).map_err(|_| {
            anyhow::anyhow!(crate::tr!(
                "store.the_selected_backup_s_config_toml_is_invalid_the"
            ))
        })?;
        if !valid_auth(auth) {
            bail!(crate::tr!(
                "store.the_selected_backup_s_auth_json_is_invalid_the"
            ));
        }
        let old = self.active_files()?;
        // Switching between identical copies still changes the owning folder.
        if old == target {
            self.write_state(name, &selected.id)?;
            return Ok(
                crate::tr!("store.the_current_configuration_is_now_assigned_to"; name = name),
            );
        }
        let current_group = self.current_group(&groups, &old)?;
        let auto_backup = if old.iter().any(Option::is_some) {
            let mut previous = current_group
                .unwrap_or_else(|| crate::tr!("store.automatic_backup_before_switching").into());
            if previous == name {
                previous = if name == crate::tr!("store.automatic_backup_before_switching") {
                    crate::tr!("store.recovery_backup_before_switching")
                } else {
                    crate::tr!("store.automatic_backup_before_switching")
                }
                .into();
            }
            let folder = self.backups().join(&previous);
            if !folder.try_exists()? {
                fs::create_dir(&folder).context(crate::tr!(
                    "store.could_not_create_the_automatic_backup_directory"
                ))?;
            }
            let version = self.save_version(&previous, &old)?;
            Some(format!("{previous}/{version}"))
        } else {
            None
        };
        if self.active_files()? != old {
            bail!(crate::tr!(
                "store.the_current_configuration_changed_before_switching_refresh_and_try"
            ));
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
            Some(path) => crate::tr!("store.switch_failed_the_previous_files_are_kept_in_backups"; path = path),
            None => crate::tr!("store.switch_failed").into(),
        })?;
        Ok(
            crate::tr!("store.switched_to_reopen_chatgpt_or_start_a_new_conversation"; name = name, arg0 = display_time(selected.stamp)),
        )
    }

    pub(crate) fn initialize_language(&self) -> Result<crate::i18n::Language> {
        self.initialize_language_with(crate::i18n::Language::detect())
    }

    fn initialize_language_with(
        &self,
        detected: crate::i18n::Language,
    ) -> Result<crate::i18n::Language> {
        let _lock = self.lock()?;
        let path = self.backups().join("settings.json");
        let mut value = read_optional(&path)?
            .map(|bytes| serde_json::from_slice::<Value>(&bytes))
            .transpose()
            .context(crate::tr!("store.invalid_global_settings_format"))?
            .unwrap_or_else(|| json!({"maxVersions": 10}));
        anyhow::ensure!(
            value.is_object(),
            crate::tr!("store.invalid_global_settings_format")
        );
        if let Some(language) = value["language"]
            .as_str()
            .and_then(crate::i18n::Language::from_code)
        {
            return Ok(language);
        }
        // Migrate only the language preference. Startup must not register login
        // items, prune backups, or replace any other existing setting.
        value["language"] = json!(detected.code());
        replace_all(vec![(path, serde_json::to_vec_pretty(&value)?)], |_| Ok(()))?;
        Ok(detected)
    }

    fn settings(&self) -> Result<Settings> {
        let Some(bytes) = read_optional(&self.backups().join("settings.json"))? else {
            return Ok(Settings::default());
        };
        let value: Value = serde_json::from_slice(&bytes)
            .context(crate::tr!("store.invalid_global_settings_format"))?;
        let count = value["maxVersions"]
            .as_u64()
            .context(crate::tr!("store.invalid_backup_retention_limit"))?;
        anyhow::ensure!(
            (1..=1000).contains(&count),
            crate::tr!("store.the_backup_retention_limit_must_be_between_1_and")
        );
        Ok(Settings {
            language: value["language"]
                .as_str()
                .and_then(crate::i18n::Language::from_code)
                .unwrap_or_else(crate::i18n::Language::detect),
            max_versions: count as usize,
            chatgpt_app_path: value["chatgptAppPath"].as_str().unwrap_or_default().into(),
            theme_directory: value["themeDirectory"].as_str().unwrap_or_default().into(),
            launch_at_login: value["launchAtLogin"].as_bool().unwrap_or(false),
            exit_on_close: value["exitOnClose"].as_bool().unwrap_or(false),
        })
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<String> {
        anyhow::ensure!(
            (1..=1000).contains(&settings.max_versions),
            crate::tr!("store.the_backup_retention_limit_must_be_between_1_and")
        );
        crate::desktop::validate_paths(&settings.app_paths())?;
        let _lock = self.lock()?;
        // Keep unknown keys so future global settings can evolve independently.
        let path = self.backups().join("settings.json");
        let mut value = read_optional(&path)?
            .map(|bytes| serde_json::from_slice::<Value>(&bytes))
            .transpose()
            .context(crate::tr!("store.invalid_global_settings_format"))?
            .unwrap_or_else(|| json!({}));
        anyhow::ensure!(
            value.is_object(),
            crate::tr!("store.invalid_global_settings_format")
        );
        value["maxVersions"] = json!(settings.max_versions);
        value["language"] = json!(settings.language.code());
        value
            .as_object_mut()
            .context(crate::tr!("store.invalid_global_settings_format"))?
            .remove("codexAppPath");
        value["chatgptAppPath"] = json!(settings.chatgpt_app_path);
        value["themeDirectory"] = json!(settings.theme_directory);
        value["launchAtLogin"] = json!(settings.launch_at_login);
        value["exitOnClose"] = json!(settings.exit_on_close);
        let bytes = serde_json::to_vec_pretty(&value)?;
        let startup = crate::startup::apply(settings.launch_at_login, self.root())?;
        if let Err(error) = replace_all(vec![(path, bytes)], |_| Ok(())) {
            if let Err(rollback) = startup.rollback() {
                bail!(
                    crate::tr!("store.could_not_save_settings_could_not_restore_startup_settings"; error = format!("{:#}", error), rollback = format!("{:#}", rollback))
                );
            }
            return Err(error).context(crate::tr!(
                "store.settings_were_not_saved_startup_settings_were_restored"
            ));
        }
        // Apply only after persistence succeeds, including when backup pruning
        // subsequently fails. Later errors and the refreshed snapshot use it.
        crate::i18n::set_language(settings.language);
        for name in self.groups()?.keys() {
            self.prune_versions(name, settings.max_versions)
                .context(crate::tr!(
                    "store.settings_were_saved_but_old_versions_could_not_be"
                ))?;
        }
        Ok(crate::tr!("store.settings_saved").to_string())
    }

    pub fn delete_version(&self, name: &str, id: &str) -> Result<String> {
        let _lock = self.lock()?;
        let parent = self.profile_directory(name)?;
        let groups = self.groups()?;
        let version = groups
            .get(name)
            .and_then(|versions| versions.iter().find(|v| v.id == id))
            .context(crate::tr!(
                "store.the_backup_version_no_longer_exists_refresh_and_try"
            ))?;
        remove_managed_directory(&version.folder, &parent)?;
        // The active files remain in place, even when their saved copy is deleted.
        Ok(
            crate::tr!("store.deleted_the_version_of_the_current_configuration_files_were"; name = name, arg0 = display_time(version.stamp)),
        )
    }

    pub fn delete_profiles(&self, names: &[String]) -> Result<String> {
        let _lock = self.lock()?;
        anyhow::ensure!(
            !names.is_empty(),
            crate::tr!("store.select_the_backup_directories_to_delete")
        );
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
                crate::tr!("store.deleted_directories_these_could_not_be_deleted_refresh_and"; deleted = deleted, arg0 = failures.join("；"))
            );
        }
        Ok(
            crate::tr!("store.deleted_backup_directories_and_all_their_versions_the_current"; deleted = deleted),
        )
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
            self.write_state("", "").context(crate::tr!(
                "store.the_backup_directory_was_deleted_but_its_current_profile"
            ))?;
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
        fs::create_dir_all(self.backups())
            .context(crate::tr!("store.could_not_create_the_backup_directory"))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.backups().join(".codex-switch.lock"))
            .context(crate::tr!("store.could_not_create_the_operation_lock"))?;
        file.try_lock().context(crate::tr!(
            "store.another_configuration_operation_is_in_progress_try_again_later"
        ))?;
        Ok(file)
    }

    fn save_version(&self, name: &str, files: &ActiveFiles) -> Result<String> {
        ensure_backup_files(files)?;
        let limit = self.settings()?.max_versions;
        let folder = self.profile_directory(name)?;
        let pending = tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(&folder)
            .context(crate::tr!("store.could_not_prepare_the_backup_directory"))?;
        for (filename, bytes) in [("config.toml", &files[0]), ("auth.json", &files[1])] {
            if let Some(bytes) = bytes {
                let path = pending.path().join(filename);
                stage(&path, bytes)?
                    .persist_noclobber(&path)
                    .map_err(|error| error.error)
                    .context(crate::tr!("store.could_not_save_backup_files"))?;
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
            fs::rename(pending.path(), &path)
                .context(crate::tr!("store.could_not_finalize_the_backup_version"))?;
            self.prune_versions(name, limit).context(crate::tr!(
                "store.the_backup_was_saved_but_old_versions_could_not"
            ))?;
            return Ok(version);
        }
    }
}

// Only remove a real, direct child of the expected managed directory. In
// particular, never recurse into a junction or symbolic link outside backups.
fn remove_managed_directory(path: &Path, parent: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .context(crate::tr!("store.the_backup_directory_no_longer_exists"))?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.is_symlink(),
        crate::tr!("store.refusing_to_delete_a_link_or_invalid_directory")
    );
    let resolved_parent = fs::canonicalize(parent).context(crate::tr!(
        "store.could_not_verify_the_backup_root_directory"
    ))?;
    let resolved = fs::canonicalize(path).context(crate::tr!(
        "store.could_not_verify_the_backup_directory_to_delete"
    ))?;
    anyhow::ensure!(
        resolved.parent() == Some(resolved_parent.as_path()),
        crate::tr!("store.the_backup_directory_is_outside_the_expected_location_deletion")
    );
    fs::remove_dir_all(path).context(crate::tr!(
        "store.could_not_delete_the_backup_check_directory_permissions"
    ))
}

fn ensure_backup_files(files: &ActiveFiles) -> Result<()> {
    if files.iter().all(Option::is_none) {
        bail!(crate::tr!("store.no_config_toml_or_auth_json_was_found_to"));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('.') || name != sanitize_name(name) {
        bail!(crate::tr!(
            "store.enter_a_name_of_up_to_80_characters_paths"
        ));
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| crate::tr!("store.could_not_read"; arg0 = path.display()))
        }
    }
}

fn summarize(bytes: &[u8]) -> Result<Summary> {
    // Discard parser diagnostics: they may contain source text or credentials.
    let text = std::str::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!(crate::tr!("store.invalid_configuration_encoding")))?;
    let value: toml::Value = toml::from_str(text)
        .map_err(|_| anyhow::anyhow!(crate::tr!("store.invalid_configuration_format")))?;
    Ok(Summary {
        model: value
            .get("model")
            .and_then(toml::Value::as_str)
            .unwrap_or(crate::tr!("store.default_model"))
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
        .unwrap_or_else(|| crate::tr!("store.unknown_time").into())
}

fn stage(path: &Path, bytes: &[u8]) -> Result<NamedTempFile> {
    let parent = path
        .parent()
        .context(crate::tr!("store.invalid_file_path"))?;
    let mut file = NamedTempFile::new_in(parent)
        .context(crate::tr!("store.could_not_create_a_temporary_file"))?;
    file.write_all(bytes)
        .context(crate::tr!("store.could_not_write_the_temporary_file"))?;
    file.as_file()
        .sync_all()
        .context(crate::tr!("store.could_not_sync_the_temporary_file"))?;
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
                    crate::tr!("store.the_write_failed_and_could_not_be_restored_recover"; arg0 = rollback_errors.join("、"))
                );
            }
            return Err(error).context(crate::tr!(
                "store.the_write_failed_the_changes_were_rolled_back"
            ));
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
        assert!(
            store
                .groups()
                .unwrap()
                .contains_key(crate::tr!("store.automatic_backup_before_switching"))
        );
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
                .contains(crate::tr!("store.authentication_file_only"))
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

    #[test]
    fn first_launch_persists_detected_language_across_system_language_changes() {
        use crate::i18n::Language::{English, SimplifiedChinese};
        for detected in [English, SimplifiedChinese] {
            let (_directory, store) = fixture();
            assert_eq!(store.initialize_language_with(detected).unwrap(), detected);
            assert_eq!(store.load().unwrap().settings.language, detected);
            let path = store.backups().join("settings.json");
            let saved = fs::read(&path).unwrap();
            let opposite = if detected == English {
                SimplifiedChinese
            } else {
                English
            };
            assert_eq!(store.initialize_language_with(opposite).unwrap(), detected);
            assert_eq!(fs::read(path).unwrap(), saved);
        }
    }

    #[test]
    fn adding_language_to_existing_settings_preserves_every_other_setting() {
        use crate::i18n::Language::SimplifiedChinese;
        let (_directory, store) = fixture();
        fs::create_dir_all(store.backups()).unwrap();
        let path = store.backups().join("settings.json");
        let mut expected = json!({
            "maxVersions": 25,
            "chatgptAppPath": "C:/Apps/ChatGPT.exe",
            "themeDirectory": "D:/Themes",
            "launchAtLogin": true,
            "exitOnClose": true,
            "futureSetting": {"preserve": [1, 2, 3]}
        });
        fs::write(&path, serde_json::to_vec_pretty(&expected).unwrap()).unwrap();
        store.initialize_language_with(SimplifiedChinese).unwrap();
        expected["language"] = json!("zh-CN");
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn saved_language_overrides_detection_and_unsupported_codes_use_detection() {
        use crate::i18n::Language::{English, SimplifiedChinese};
        let (_directory, store) = fixture();
        fs::create_dir_all(store.backups()).unwrap();
        let path = store.backups().join("settings.json");
        fs::write(&path, br#"{"maxVersions":10,"language":"en"}"#).unwrap();
        assert_eq!(
            store.initialize_language_with(SimplifiedChinese).unwrap(),
            English
        );
        fs::write(&path, br#"{"maxVersions":10,"language":"zh-CN"}"#).unwrap();
        assert_eq!(
            store.initialize_language_with(English).unwrap(),
            SimplifiedChinese
        );
        fs::write(
            &path,
            br#"{"maxVersions":10,"language":"unsupported","custom":true}"#,
        )
        .unwrap();
        assert_eq!(store.initialize_language_with(English).unwrap(), English);
        let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["language"], "en");
        assert_eq!(saved["custom"], true);
    }

    #[test]
    fn invalid_settings_are_not_overwritten_during_language_initialization() {
        let (_directory, store) = fixture();
        fs::create_dir_all(store.backups()).unwrap();
        let path = store.backups().join("settings.json");
        for original in ["{invalid", "[]", "null"] {
            fs::write(&path, original).unwrap();
            assert!(
                store
                    .initialize_language_with(crate::i18n::Language::English)
                    .is_err()
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
    }
}
