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
        const defaultId = 'com.openai.chat';
        let path = spec.path;
        let bundleId = defaultId;
        let problem = '';
        if (!path) {
            const url = workspace.URLForApplicationWithBundleIdentifier(defaultId);
            path = url ? unwrap(url.path) : '';
            if (!path) path = unwrap(workspace.fullPathForApplication(spec.name));
        }
        const normalize = path => unwrap($(path).stringByStandardizingPath.stringByResolvingSymlinksInPath);
        if (path) path = normalize(path);
        if (path) {
            const bundle = $.NSBundle.bundleWithPath(path);
            const id = bundle ? unwrap(bundle.bundleIdentifier) : '';
            if (!id || id !== defaultId) {
                problem = 'Invalid ' + spec.name + ' application. Check Settings.';
            } else { bundleId = id; }
        }
        const pids = [];
        for (let i = 0; i < running.count; i++) {
            const app = running.objectAtIndex(i);
            if (unwrap(app.bundleIdentifier) !== bundleId || app.isTerminated) continue;
            const runningPath = app.bundleURL ? normalize(unwrap(app.bundleURL.path)) : '';
            if (spec.path && runningPath !== path) continue;
            pids.push(Number(app.processIdentifier));
            if (!path) path = runningPath;
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
    if !theme_arguments.is_empty() {
        command.arg("--args").args(theme_arguments);
    }
    output(command).context("无法打开 macOS 应用，请检查设置中的 .app 路径")?;
    Ok(())
}
