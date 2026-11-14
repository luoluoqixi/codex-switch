//! Per-user login registration. Saving preferences is the only entry point that
//! changes the OS; loading or refreshing the UI never installs a startup item.
use anyhow::{Context, Result};
use std::path::Path;

#[cfg(target_os = "windows")]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const RUN_NAME: &str = "CodexSwitch";

pub(crate) struct StartupChange {
    #[cfg(target_os = "windows")]
    previous: Option<windows_registry::Value>,
    #[cfg(target_os = "macos")]
    path: std::path::PathBuf,
    #[cfg(target_os = "macos")]
    previous: Option<Vec<u8>>,
}

pub(crate) fn apply(enabled: bool, config_root: &Path) -> Result<StartupChange> {
    #[cfg(target_os = "windows")]
    {
        let key = windows_registry::CURRENT_USER
            .create(RUN_KEY)
            .context(crate::tr!(
                "startup.could_not_open_the_current_user_s_startup_settings"
            ))?;
        let previous = match key.get_value(RUN_NAME) {
            Ok(value) => Some(value),
            Err(error) if error.code().0 as u32 == 0x80070002 => None,
            Err(error) => {
                return Err(error).context(crate::tr!("startup.could_not_read_startup_settings"));
            }
        };
        if enabled {
            let executable = std::env::current_exe().context(crate::tr!(
                "startup.could_not_locate_the_application_executable"
            ))?;
            // CommandLineToArgvW quoting, including a directory ending in '\'.
            let command = format!(
                "{} --codex-dir {}",
                quote_windows(&executable),
                quote_windows(config_root)
            );
            key.set_string(RUN_NAME, command)
                .context(crate::tr!("startup.could_not_enable_launch_at_startup"))?;
        } else if previous.is_some() {
            key.remove_value(RUN_NAME)
                .context(crate::tr!("startup.could_not_disable_launch_at_startup"))?;
        }
        Ok(StartupChange { previous })
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").context(crate::tr!(
            "single_instance.could_not_find_your_home_directory"
        ))?;
        let path = std::path::PathBuf::from(home)
            .join("Library/LaunchAgents/com.luoluoqixi.codex-switch.plist");
        let previous = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .context(crate::tr!("startup.could_not_read_login_startup_settings"));
            }
        };
        if enabled {
            let executable = std::env::current_exe().context(crate::tr!(
                "startup.could_not_locate_the_application_executable"
            ))?;
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.luoluoqixi.codex-switch</string>
<key>ProgramArguments</key><array><string>{}</string><string>--codex-dir</string><string>{}</string></array>
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#,
                xml(&executable.to_string_lossy()),
                xml(&config_root.to_string_lossy())
            );
            write_plist(&path, plist.as_bytes())
                .context(crate::tr!("startup.could_not_enable_launch_at_login"))?;
        } else if previous.is_some() {
            std::fs::remove_file(&path)
                .context(crate::tr!("startup.could_not_disable_launch_at_login"))?;
        }
        Ok(StartupChange { path, previous })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = config_root;
        anyhow::ensure!(
            !enabled,
            crate::tr!("startup.launch_at_login_is_supported_on_windows_and_macos")
        );
        Ok(StartupChange {})
    }
}

impl StartupChange {
    pub(crate) fn rollback(self) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            let key = windows_registry::CURRENT_USER.create(RUN_KEY)?;
            match self.previous {
                Some(value) => key.set_value(RUN_NAME, &value)?,
                None => match key.remove_value(RUN_NAME) {
                    Ok(()) => {}
                    Err(error) if error.code().0 as u32 == 0x80070002 => {}
                    Err(error) => return Err(error.into()),
                },
            }
        }
        #[cfg(target_os = "macos")]
        match self.previous {
            Some(bytes) => write_plist(&self.path, &bytes)?,
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            },
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn quote_windows(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for ch in text.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        quoted.extend(std::iter::repeat_n(
            '\\',
            if ch == '"' { slashes * 2 + 1 } else { slashes },
        ));
        quoted.push(ch);
        slashes = 0;
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(target_os = "macos")]
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
fn write_plist(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .context(crate::tr!("startup.invalid_login_startup_directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    Ok(())
}
