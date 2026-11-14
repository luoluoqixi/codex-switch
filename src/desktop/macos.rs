use super::*;
use serde_json::{Value, json};

const DISCOVERY_SCRIPT: &str = r###"ObjC.import('AppKit');
ObjC.import('Foundation');

// NSWorkspace enumerates desktop applications by bundle identity.
function run(argv) {
    const specs = JSON.parse(argv[0]);
    const workspace = $.NSWorkspace.sharedWorkspace;
    const running = workspace.runningApplications;
    const unwrap = value => value ? (ObjC.unwrap(value) || '') : '';
    return JSON.stringify(specs.map(spec => {
        // Current ChatGPT/Codex releases and the original ChatGPT desktop app.
        const bundleIds = ['com.openai.codex', 'com.openai.chat'];
        const normalize = path => unwrap($(path).stringByStandardizingPath.stringByResolvingSymlinksInPath);
        const identifier = path => {
            const bundle = $.NSBundle.bundleWithPath(path);
            return bundle ? unwrap(bundle.bundleIdentifier) : '';
        };
        let path = spec.path ? normalize(spec.path) : '';
        let problem = '';
        if (!path) {
            const candidates = bundleIds.map(id => {
                const url = workspace.URLForApplicationWithBundleIdentifier(id);
                return url ? unwrap(url.path) : '';
            });
            candidates.push(unwrap(workspace.fullPathForApplication(spec.name)),
                '/Applications/' + spec.name + '.app',
                unwrap($.NSHomeDirectory()) + '/Applications/' + spec.name + '.app');
            for (const candidate of candidates) {
                if (!candidate) continue;
                const normalized = normalize(candidate);
                if (!bundleIds.includes(identifier(normalized))) continue;
                path = normalized;
                break;
            }
        }
        let bundleId = path ? identifier(path) : '';
        if (path && !bundleIds.includes(bundleId)) {
            problem = 'Invalid ' + spec.name + ' application. Check Settings.';
        }
        const pids = [];
        for (let i = 0; i < running.count; i++) {
            const app = running.objectAtIndex(i);
            const id = unwrap(app.bundleIdentifier);
            if (!bundleIds.includes(id) || (bundleId && id !== bundleId) || app.isTerminated) continue;
            const runningPath = app.bundleURL ? normalize(unwrap(app.bundleURL.path)) : '';
            if (spec.path && runningPath !== path) continue;
            pids.push(Number(app.processIdentifier));
            if (!path) {
                path = runningPath;
                bundleId = id;
            }
        }
        return {name: spec.name, path, pids, problem};
    }));
}"###;

fn jxa(script: &str) -> Command {
    let mut command = Command::new("/usr/bin/osascript");
    command.args(["-l", "JavaScript", "-e", script]);
    command
}

pub(super) fn inspect(paths: &[String; 1]) -> Result<Vec<AppStatus>> {
    let specs: Vec<_> = DesktopApp::ALL
        .iter()
        .map(|app| json!({"name": app.name(), "path": paths[app.index()]}))
        .collect();
    let mut command = jxa(DISCOVERY_SCRIPT);
    command.arg(serde_json::to_string(&specs)?);
    let bytes = output(command)?;
    let values: Vec<Value> = serde_json::from_slice(&bytes).context("无法解析 macOS 应用信息")?;
    DesktopApp::ALL
        .into_iter()
        .map(|app| {
            let value = values
                .iter()
                .find(|v| v["name"].as_str() == Some(app.name()))
                .context("macOS 应用信息不完整")?;
            Ok(AppStatus {
                app,
                debug_endpoint: false,
                pids: pids(&value["pids"]),
                path: optional_path(&value["path"]),
                problem: value["problem"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|_| format!("{} 路径无效，请在设置中选择对应的 .app 应用。", app.name())),
            })
        })
        .collect()
}

pub(super) fn close(status: &AppStatus, _paths: &[String; 1]) -> Result<()> {
    const SCRIPT: &str = r#"ObjC.import('AppKit');
function run(argv) {
    const ids = JSON.parse(argv[0]);
    for (const pid of ids) {
        const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(pid);
        if (app && !app.isTerminated && !app.terminate) { throw Error('Application refused to quit.'); }
    }
    return 'ok';
}"#;
    let mut command = jxa(SCRIPT);
    command.arg(serde_json::to_string(&status.pids)?);
    output(command).context("无法请求应用退出，请检查应用内是否有待处理的提示")?;
    Ok(())
}

pub(super) fn launch(status: &AppStatus) -> Result<()> {
    let path = status
        .path
        .as_ref()
        .context("请在设置中指定 .app 应用路径")?;
    let mut command = Command::new("/usr/bin/open");
    command.arg("-a").arg(path);
    let theme_arguments = crate::themes::launch_arguments()?;
    command.arg("--args").args(theme_arguments);
    output(command).context("无法打开 macOS 应用，请检查设置中的 .app 路径")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_bundle(root: &Path, name: &str, identifier: &str) -> PathBuf {
        let bundle = root.join(format!("{name}.app"));
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#
            ),
        )
        .unwrap();
        bundle.canonicalize().unwrap()
    }

    #[test]
    fn configured_path_accepts_current_and_legacy_chatgpt_bundle_ids() {
        let temp = tempfile::tempdir().unwrap();
        for (name, identifier) in [
            ("Current ChatGPT", "com.openai.codex"),
            ("Legacy ChatGPT", "com.openai.chat"),
        ] {
            let path = app_bundle(temp.path(), name, identifier);
            let statuses = inspect(&[path.to_string_lossy().into_owned()]).unwrap();
            let status = &statuses[0];
            assert!(status.problem().is_none(), "{status:?}");
            assert_eq!(status.path.as_ref().unwrap().canonicalize().unwrap(), path);
            assert!(status.has_installation_path());
            // Another installed copy with the same ID must not match this path.
            assert!(!status.is_running());
        }
    }

    #[test]
    fn configured_path_rejects_unrelated_or_missing_application() {
        let temp = tempfile::tempdir().unwrap();
        let unrelated = app_bundle(temp.path(), "Other", "com.example.other");
        for path in [unrelated, temp.path().join("Missing.app")] {
            let statuses = inspect(&[path.to_string_lossy().into_owned()]).unwrap();
            let status = &statuses[0];
            assert!(status.problem().is_some(), "{status:?}");
            assert!(!status.has_installation_path());
            assert!(!status.is_running());
        }
    }
}
