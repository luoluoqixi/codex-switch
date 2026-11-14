use super::*;
use serde_json::{Value, json};

const DISCOVERY_SCRIPT: &str = r###"$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$specs = ConvertFrom-Json $env:CODEX_SWITCH_APPS
function Test-GuiExecutable([string]$path) {
    $reader = $null
    try {
        $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
        $reader = New-Object IO.BinaryReader($stream)
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) { return $false }
        $stream.Position = 0x3C
        $offset = $reader.ReadUInt32()
        if ($offset -gt $stream.Length - 94) { return $false }
        $stream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x4550) { return $false }
        $stream.Position = $offset + 24
        $magic = $reader.ReadUInt16()
        if ($magic -ne 0x10B -and $magic -ne 0x20B) { return $false }
        $stream.Position = $offset + 24 + 68
        return $reader.ReadUInt16() -eq 2 # IMAGE_SUBSYSTEM_WINDOWS_GUI
    } catch { return $false }
    finally { if ($reader) { $reader.Dispose() } }
}
$startApps = @(Get-StartApps -ErrorAction SilentlyContinue)
$packages = @(Get-AppxPackage -Name '*OpenAI*' -ErrorAction SilentlyContinue)
$session = (Get-Process -Id $PID).SessionId
$results = foreach ($spec in $specs) {
    $name = [string]$spec.name
    $custom = [string]$spec.path
    # A desktop shortcut such as com.openai.codex is not an AppUserModelID.
    $registered = $startApps | Where-Object { $_.Name -eq $name -and $_.AppID -match '^[^!]+![^!]+$' } | Select-Object -First 1
    $appId = if ($registered) { [string]$registered.AppID } else { '' }
    $expected = @()
    $problem = ''
    if ($custom) {
        if ((Test-Path -LiteralPath $custom -PathType Leaf) -and ([IO.Path]::GetFileName($custom) -ieq "$name.exe") -and (Test-GuiExecutable $custom)) {
            $expected += [IO.Path]::GetFullPath($custom)
        } else { $problem = 'path' }
        $appId = ''
    } else {
        # Resolve the package independently of StartApps: duplicate display names
        # and desktop shortcuts must not hide an installed, stopped application.
        foreach ($package in @($packages | Sort-Object @{ Expression = { if ($_.Name -eq 'OpenAI.Codex') { 0 } else { 1 } } })) {
            if ($package.InstallLocation -and $package.PackageFamilyName) {
                try {
                    $manifestPath = Join-Path $package.InstallLocation 'AppxManifest.xml'
                    $manifest = [xml](Get-Content -LiteralPath $manifestPath -Raw -ErrorAction Stop)
                    foreach ($application in $manifest.Package.Applications.Application) {
                        if ([IO.Path]::GetFileName([string]$application.Executable) -ieq "$name.exe") {
                            $candidate = Join-Path $package.InstallLocation ([string]$application.Executable)
                            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                                $expected += $candidate
                                if ($expected.Count -eq 1) {
                                    $appId = [string]$package.PackageFamilyName + '!' + [string]$application.Id
                                }
                            }
                        }
                    }
                } catch { }
            }
        }
        foreach ($base in @($env:LOCALAPPDATA, $env:ProgramFiles)) {
            if ($base) {
                foreach ($relative in @("Programs\$name\$name.exe", "$name\$name.exe")) {
                    $candidate = Join-Path $base $relative
                    if (Test-Path -LiteralPath $candidate -PathType Leaf) { $expected += $candidate }
                }
            }
        }
    }
    $matched = @()
    foreach ($process in @(Get-Process -Name $name -ErrorAction SilentlyContinue)) {
        if ($process.SessionId -ne $session) { continue }
        try { $processPath = [string]$process.Path } catch { $processPath = '' }
        if (-not $processPath) { $problem = 'process'; continue }
        # Exact registered/custom executable paths, or the Electron desktop app
        # layout, rather than relying on an executable's name alone.
        $isDesktop = $expected -icontains $processPath
        if (-not $custom -and -not $isDesktop) {
            $parent = Split-Path -Parent $processPath
            $isDesktop = Test-Path -LiteralPath (Join-Path $parent 'resources\app.asar') -PathType Leaf
        }
        if ($isDesktop) {
            $matched += $process
            if (-not ($expected -icontains $processPath)) { $expected += $processPath }
        }
    }
    $path = if ($expected.Count) { [string]$expected[0] } else { '' }
    [pscustomobject]@{ name = $name; path = $path; appId = $appId; pids = @($matched | ForEach-Object { $_.Id }); problem = $problem }
}
ConvertTo-Json -InputObject @($results) -Depth 4 -Compress"###;

fn powershell(script: &str) -> Command {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let mut command = Command::new(root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    command
}

pub(super) fn inspect(paths: &[String; 1]) -> Result<Vec<AppStatus>> {
    let specs: Vec<_> = DesktopApp::ALL
        .iter()
        .map(|app| json!({"name": app.name(), "path": paths[app.index()]}))
        .collect();
    let mut command = powershell(DISCOVERY_SCRIPT);
    // Pass user paths as data, never as PowerShell source or a command line fragment.
    command.env("CODEX_SWITCH_APPS", serde_json::to_string(&specs)?);
    let bytes = output(command)?;
    let text = String::from_utf8(bytes).context(crate::tr!(
        "windows.invalid_system_application_information_encoding"
    ))?;
    let values: Vec<Value> = serde_json::from_str(text.trim_start_matches('\u{feff}').trim())
        .context(crate::tr!(
            "windows.could_not_parse_system_application_information"
        ))?;
    DesktopApp::ALL
        .into_iter()
        .map(|app| {
            let value = values
                .iter()
                .find(|v| v["name"].as_str() == Some(app.name()))
                .context(crate::tr!("windows.incomplete_system_application_information"))?;
            Ok(AppStatus {
                app,
                debug_endpoint: false,
                pids: pids(&value["pids"]),
                path: optional_path(&value["path"]),
                registered_id: value["appId"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                problem: match value["problem"].as_str() {
                    Some("path") => Some(crate::tr!("windows.invalid_path_select_the_desktop_app_s_exe_in"; arg0 = app.name())),
                    Some("process") => Some(crate::tr!("windows.could_not_verify_the_process_path_for_check_permissions"; arg0 = app.name())),
                    _ => None,
                },
            })
        })
        .collect()
}

pub(super) fn close(status: &AppStatus, paths: &[String; 1]) -> Result<()> {
    if !status.is_running() {
        return Ok(());
    }
    // Closing a window can leave the application resident in the system tray.
    // End the verified application PIDs and their children so a restart loads
    // the new configuration instead of reconnecting to an existing process.
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .context(crate::tr!(
            "windows.could_not_locate_the_windows_system_directory"
        ))?;
    let mut command = Command::new(root.join("System32/taskkill.exe"));
    command.args(["/F", "/T"]);
    for pid in &status.pids {
        command.arg("/PID").arg(pid.to_string());
    }
    if let Err(error) = output(command) {
        // /T may already have removed another PID included in the same request.
        // A nonzero result is harmless only when the application actually exited.
        let after = inspect(paths).context(crate::tr!(
            "windows.the_close_command_failed_and_the_app_status_could"
        ))?;
        let after = after
            .iter()
            .find(|app| app.app() == status.app())
            .context(crate::tr!(
                "windows.could_not_read_the_app_status_after_closing"
            ))?;
        if after.is_running() || after.problem().is_some() {
            return Err(error).context(crate::tr!("windows.could_not_stop_the_process_tree_check_whether_the"; arg0 = status.app().name()));
        }
    }
    Ok(())
}

pub(super) fn launch(status: &AppStatus) -> Result<()> {
    let theme_arguments = crate::themes::launch_arguments()?;
    // AppsFolder navigation discards debugging arguments. Always launch with
    // the persistent theme endpoint, including while themes are switched off.
    let mut direct_error = None;
    if let Some(path) = &status.path {
        let mut command = Command::new(path);
        command.args(&theme_arguments);
        if let Some(parent) = path.parent() {
            command.current_dir(parent);
        }
        match command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(error) => direct_error = Some(error),
        }
    }
    if let Some(id) = &status.registered_id {
        return activate_with_arguments(id, &theme_arguments).with_context(|| {
            match (&status.path, direct_error) {
                (Some(path), Some(error)) => crate::tr!("windows.could_not_start_directly_windows_app_activation_also_failed"; arg0 = path.display(), error = error),
                _ => crate::tr!("windows.windows_app_activation_failed_and_the_executable_could_not")
                    .into(),
            }
        });
    }
    if let Some(error) = direct_error {
        return Err(error).context(crate::tr!(
            "windows.could_not_start_chatgpt_exe_directly_check_the_path"
        ));
    }
    bail!(crate::tr!(
        "windows.could_not_open_the_app_with_theme_support_select"
    ));
}

fn activate_with_arguments(id: &str, arguments: &[std::ffi::OsString]) -> Result<()> {
    anyhow::ensure!(
        id.split_once('!')
            .is_some_and(|(family, app)| !family.is_empty()
                && !app.is_empty()
                && !app.contains('!')),
        crate::tr!("windows.invalid_windows_app_activation_id_refresh_and_try_again")
    );
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            UI::Shell::{AO_NONE, ApplicationActivationManager, IApplicationActivationManager},
        },
        core::HSTRING,
    };
    // These arguments originate in the theme service, never in a theme ZIP.
    let line = arguments
        .iter()
        .map(|arg| quote_argument(&arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> Result<()> {
            let manager: IApplicationActivationManager =
                CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)
                    .context(crate::tr!(
                        "windows.could_not_initialize_the_windows_app_launcher"
                    ))?;
            manager
                .ActivateApplication(&HSTRING::from(id), &HSTRING::from(line), AO_NONE)
                .context(crate::tr!(
                    "windows.could_not_open_the_app_with_theme_arguments_quit"
                ))?;
            Ok(())
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

// CommandLineToArgvW quoting, including a possible trailing backslash.
fn quote_argument(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            result.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            result.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        result.push(ch);
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_store_app_is_discovered_despite_duplicate_or_missing_start_entries() {
        let temp = tempfile::tempdir().unwrap();
        let package_root = temp.path().join("Codex package with spaces");
        std::fs::create_dir_all(package_root.join("app")).unwrap();
        let executable = package_root.join("app/ChatGPT.exe");
        std::fs::write(&executable, b"test fixture").unwrap();
        std::fs::write(package_root.join("AppxManifest.xml"), r#"<?xml version="1.0"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
  <Applications>
    <Application Id="App" Executable="app/ChatGPT.exe" EntryPoint="Windows.FullTrustApplication" />
    <Application Id="Helper" Executable="app/helper.exe" EntryPoint="Windows.FullTrustApplication" />
  </Applications>
</Package>"#).unwrap();
        // Keep the real discovery algorithm and replace only OS inventory.
        let mocks = r#"
function Get-StartApps {
    if ($env:CODEX_SWITCH_TEST_NO_START -eq '1') { return }
    [pscustomobject]@{ Name = 'ChatGPT'; AppID = 'com.openai.codex' }
    [pscustomobject]@{ Name = 'ChatGPT'; AppID = 'OpenAI.Codex_test!App' }
}
function Get-AppxPackage {
    [pscustomobject]@{ Name = 'OpenAI.Codex'; InstallLocation = $env:CODEX_SWITCH_TEST_PACKAGE; PackageFamilyName = 'OpenAI.Codex_test' }
}
function Get-Process {
    param($Id, $Name)
    if ($null -ne $Id) { [pscustomobject]@{ SessionId = 1 } }
}
"#;
        for no_start in ["0", "1"] {
            let mut command = powershell(&format!("{mocks}\n{DISCOVERY_SCRIPT}"));
            command.env("CODEX_SWITCH_APPS", r#"[{"name":"ChatGPT","path":""}]"#);
            command.env("CODEX_SWITCH_TEST_PACKAGE", &package_root);
            command.env("CODEX_SWITCH_TEST_NO_START", no_start);
            let result: Value = serde_json::from_slice(&output(command).unwrap()).unwrap();
            assert_eq!(result[0]["appId"], "OpenAI.Codex_test!App");
            assert_eq!(
                PathBuf::from(result[0]["path"].as_str().unwrap()),
                executable
            );
            assert_eq!(result[0]["pids"], json!([]));
            assert_eq!(result[0]["problem"], "");
        }
    }

    #[test]
    fn desktop_shortcut_cannot_be_used_as_packaged_app_activation_id() {
        let error = activate_with_arguments("com.openai.codex", &[]).unwrap_err();
        assert_eq!(
            error.to_string(),
            crate::tr!("windows.invalid_windows_app_activation_id_refresh_and_try_again")
        );
    }
}
