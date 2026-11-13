//! Small synchronous CDP client. Used exclusively on the optional theme thread.
use super::{DEBUG_PORT, ThemeStatus, payload};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket};

const TIMEOUT: Duration = Duration::from_secs(2);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) fn is_available() -> bool {
    super::verify_listener().is_ok()
        && get_json("/json/version")
            .ok()
            .and_then(|v| v["webSocketDebuggerUrl"].as_str().map(str::to_owned))
            .is_some_and(|url| valid_ws(&url, "browser"))
}
const PROBE: &str = r#"(() => {
 const route = new URLSearchParams(location.search).get('initialRoute') || '';
 const excluded = location.pathname.endsWith('/avatar-overlay-composition-surface.html') || route.startsWith('/avatar-overlay');
 const shell = document.querySelector('main:is(.main-surface, [data-app-shell-main-surface], [class*="_MainContentSurface_"])');
 const sidebar = document.querySelector('aside.app-shell-left-panel');
 const home = document.querySelector('[role="main"]:has([data-testid="home-icon"])');
 const settings = document.querySelector('[data-settings-panel-slug="general-settings"], [data-testid="theme-preview"]');
 const branded = document.querySelector('[data-testid="app-shell-header-context-menu-surface"]');
 const state = window.__CODEX_DREAM_SKIN_STATE__;
 const styled = state?.styleMode === 'adopted' ? document.adoptedStyleSheets.includes(state.styleSheet) : !!state?.styleNode?.isConnected;
 const ready = !!document.body && document.readyState !== 'loading';
 return {codex: location.protocol === 'app:' && !excluded && ready && (!!shell && !!sidebar || !!home || !!settings || !!branded || location.pathname.endsWith('/index.html')),
 active: !!state && !window.__CODEX_DREAM_SKIN_DISABLED__ && styled, revision: state?.revision ?? null, excluded};
})()"#;

struct Session {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    script_id: Option<String>,
    applied: bool,
    first_seen: Instant,
    url: String,
    broken: bool,
    failure: Option<String>,
    revision: u64,
    generation: Option<String>,
}
impl Session {
    fn connect(url: &str) -> Result<Self> {
        ensure!(valid_ws(url, "page"), "应用返回的调试连接地址无效");
        let stream = connect()?;
        let (socket, _) = tungstenite::client(url, stream).context("无法连接 Codex 调试通道")?;
        Ok(Self {
            socket,
            next_id: 0,
            script_id: None,
            applied: false,
            first_seen: Instant::now(),
            url: url.into(),
            broken: false,
            failure: None,
            revision: 0,
            generation: None,
        })
    }
    fn reconnect(&mut self) -> Result<()> {
        let replacement = Self::connect(&self.url)?;
        self.socket = replacement.socket;
        if let Some(id) = self.script_id.take() {
            // A fresh DevTools session may already have removed its predecessor's
            // scripts. Either way, register at most one current source below.
            let _ = self.call(
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({"identifier":id}),
            );
        }
        self.broken = false;
        self.applied = false;
        self.revision = 0;
        self.first_seen = Instant::now();
        Ok(())
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.socket.send(Message::Text(
            json!({"id": id, "method": method, "params": params})
                .to_string()
                .into(),
        ))?;
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        loop {
            ensure!(Instant::now() < deadline, "等待 Codex 主题响应超时");
            self.socket.get_mut().set_read_timeout(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            ))?;
            let message = self.socket.read().context("无法读取 Codex 主题响应")?;
            if let Message::Text(text) = message {
                let response: Value = serde_json::from_str(&text)?;
                if response["method"] == "Runtime.executionContextsCleared"
                    || response["method"] == "Page.frameNavigated"
                        && response["params"]["frame"].get("parentId").is_none()
                {
                    self.applied = false;
                }
                if response["id"].as_u64() != Some(id) {
                    continue;
                }
                if let Some(error) = response.get("error") {
                    bail!(
                        "Codex 调试调用失败：{}",
                        error["message"].as_str().unwrap_or("未知错误")
                    );
                }
                return Ok(response["result"].clone());
            }
        }
    }
    fn evaluate(&mut self, expression: &str) -> Result<Value> {
        let result = self.call(
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
        )?;
        if let Some(exception) = result.get("exceptionDetails") {
            bail!(
                "主题渲染出错：{}",
                exception["exception"]["description"]
                    .as_str()
                    .or_else(|| exception["text"].as_str())
                    .unwrap_or("未知错误")
            );
        }
        Ok(result["result"]["value"].clone())
    }
    fn install(&mut self, script: &str) -> Result<()> {
        self.call("Page.enable", json!({}))?;
        if let Some(identifier) = self.script_id.take() {
            self.call(
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({"identifier":identifier}),
            )?;
        }
        let result = self.call(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": script}),
        )?;
        self.script_id = Some(
            result["identifier"]
                .as_str()
                .context("Codex 未返回主题脚本标识")?
                .into(),
        );
        Ok(())
    }
    fn apply(&mut self, script: &str, revision: u64) -> Result<bool> {
        if self.broken {
            self.reconnect()?;
        }
        if self.revision != revision {
            self.install(script)?;
            self.revision = revision;
            self.applied = false;
            self.failure = None;
            self.first_seen = Instant::now();
        }
        if let Some(error) = &self.failure {
            bail!("{error}");
        }
        let probe = self.evaluate(PROBE)?;
        if probe["excluded"] == true {
            self.clear()?;
            return Ok(false);
        }
        if probe["codex"] != true {
            ensure!(
                self.first_seen.elapsed() < Duration::from_secs(15),
                "调试通道已连接，但 Codex 主窗口尚未就绪或页面结构已变化"
            );
            return Ok(false);
        }
        if !self.applied
            || probe["active"] != true
            || probe["revision"].as_str() != self.generation.as_deref()
        {
            let result = self.evaluate(script)?;
            if result["cancelled"] == true {
                return Ok(false);
            }
            ensure!(result["installed"] == true, "Codex 尚未确认主题生效");
            self.generation = Some(
                result["revision"]
                    .as_str()
                    .context("主题渲染器未返回版本标识")?
                    .into(),
            );
            let confirmation = self.evaluate(PROBE)?;
            ensure!(
                confirmation["active"] == true
                    && confirmation["revision"].as_str() == self.generation.as_deref(),
                "Codex 尚未确认主题样式生效"
            );
            self.applied = true;
        }
        Ok(true)
    }
    fn clear(&mut self) -> Result<()> {
        if self.broken {
            self.reconnect()?;
        }
        let remove = if let Some(id) = self.script_id.take() {
            self.call(
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({"identifier": id}),
            )
            .map(|_| ())
        } else {
            Ok(())
        };
        let cleanup = self.evaluate(payload::CLEANUP).map(|_| ());
        self.applied = false;
        self.revision = 0;
        self.generation = None;
        remove.and(cleanup)
    }
}

pub(super) struct Runtime {
    browser: Option<String>,
    sessions: HashMap<String, Session>,
    revision: u64,
}
impl Runtime {
    pub(super) fn new() -> Self {
        Self {
            browser: None,
            sessions: HashMap::new(),
            revision: 0,
        }
    }
    pub(super) fn replace(&mut self, script: &str) -> Result<ThemeStatus> {
        self.revision += 1;
        self.refresh(script)
    }
    pub(super) fn refresh(&mut self, script: &str) -> Result<ThemeStatus> {
        super::verify_listener()?;
        let version = match get_json("/json/version") {
            Ok(value) => value,
            // Keep existing sockets and script identifiers across temporary
            // discovery failures so Off can still remove previously injected CSS.
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::ConnectionReset
                    )
                }) =>
            {
                return Ok(ThemeStatus::WaitingForConnection);
            }
            Err(error) => return Err(error).context("无法读取 Codex 调试通道"),
        };
        let browser = version["webSocketDebuggerUrl"]
            .as_str()
            .context("调试端口不是可用的 Codex 浏览器")?;
        ensure!(valid_ws(browser, "browser"), "Codex 浏览器调试地址无效");
        if self.browser.as_deref() != Some(browser) {
            self.sessions.clear();
            self.browser = Some(browser.into());
        }
        let targets = get_json("/json/list")?;
        let targets = targets.as_array().context("Codex 窗口列表格式无效")?;
        let mut alive = HashSet::new();
        let mut failure = None;
        let mut applied = 0;
        for target in targets {
            let Some(id) = target["id"].as_str() else {
                continue;
            };
            let Some(url) = target["url"].as_str() else {
                continue;
            };
            if target["type"] != "page"
                || !url.starts_with("app://")
                || url.contains("avatar-overlay")
            {
                continue;
            }
            alive.insert(id.to_string());
            let attempt = (|| -> Result<bool> {
                if !self.sessions.contains_key(id) {
                    let Some(ws) = target["webSocketDebuggerUrl"].as_str() else {
                        return Ok(false);
                    };
                    ensure!(
                        ws.ends_with(&format!("/devtools/page/{id}")),
                        "Codex 窗口调试标识不一致"
                    );
                    let session = Session::connect(ws)?;
                    self.sessions.insert(id.to_string(), session);
                }
                let session = self.sessions.get_mut(id).unwrap();
                session.apply(script, self.revision)
            })();
            if let Ok(true) = attempt {
                applied += 1;
            } else if let Err(error) = attempt {
                // Navigation can destroy an execution context while it is being
                // queried. Give the real renderer time to become ready first.
                let message = error.to_string();
                let transient = message.contains("context")
                    || message.contains("Context")
                    || message.contains("navigation")
                    || message.contains("主窗口尚未就绪")
                    || message.contains("尚未确认");
                let transport = error.downcast_ref::<tungstenite::Error>().is_some()
                    || error.downcast_ref::<std::io::Error>().is_some();
                let session = self.sessions.get_mut(id);
                if let Some(session) = session {
                    session.broken = transport;
                    if !transient && !transport {
                        session.failure = Some(format!("{error:#}"));
                    }
                    if !transient || session.first_seen.elapsed() > Duration::from_secs(15) {
                        failure = Some(error);
                    }
                    session.applied = false;
                } else {
                    failure = Some(error);
                }
            }
        }
        let removed: Vec<_> = self
            .sessions
            .keys()
            .filter(|id| !alive.contains(*id))
            .cloned()
            .collect();
        for id in removed {
            if let Some(mut session) = self.sessions.remove(&id)
                && targets
                    .iter()
                    .any(|target| target["id"].as_str() == Some(&id))
                && let Err(error) = session.clear()
            {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(if applied > 0 {
            ThemeStatus::Applied(applied)
        } else {
            ThemeStatus::WaitingForWindow
        })
    }
    pub(super) fn clear(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        for session in self.sessions.values_mut() {
            if let Err(error) = session.clear() {
                failures.push(format!("{error:#}"));
            }
        }
        self.sessions.clear();
        if failures.is_empty() {
            Ok(())
        } else {
            bail!(
                "主题后台任务已停止，但部分窗口未能清理主题；请手动重新打开 Codex。{}",
                failures.join("；")
            );
        }
    }
}

fn valid_ws(url: &str, kind: &str) -> bool {
    let prefix = format!("ws://127.0.0.1:{DEBUG_PORT}/devtools/{kind}/");
    let alternate = format!("ws://localhost:{DEBUG_PORT}/devtools/{kind}/");
    url.strip_prefix(&prefix)
        .or_else(|| url.strip_prefix(&alternate))
        .is_some_and(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn connect() -> Result<TcpStream> {
    let address = SocketAddr::from(([127, 0, 0, 1], DEBUG_PORT));
    let stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    Ok(stream)
}

fn get_json(path: &str) -> Result<Value> {
    let mut stream = connect()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{DEBUG_PORT}\r\nConnection: close\r\n\r\n"
    )?;
    read_http_json(stream)
}

fn read_http_json(stream: impl Read) -> Result<Value> {
    let mut reader = BufReader::new(stream);
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        ensure!(header.len() < 64 * 1024, "Codex 调试响应头过长");
        let mut byte = [0];
        reader
            .read_exact(&mut byte)
            .context("Codex 调试响应头不完整")?;
        header.push(byte[0]);
    }
    ensure!(
        header.starts_with(b"HTTP/1.1 200 ") || header.starts_with(b"HTTP/1.0 200 "),
        "Codex 调试请求失败"
    );
    let header = std::str::from_utf8(&header).context("Codex 调试响应头编码无效")?;
    let length = header
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(key, value)| {
            key.eq_ignore_ascii_case("Content-Length")
                .then_some(value.trim())
        })
        .context("Codex 调试响应缺少 Content-Length")?
        .parse::<usize>()
        .context("Codex 调试响应长度无效")?;
    ensure!(length <= 4 * 1024 * 1024, "Codex 调试响应过大");
    let mut body = vec![0; length];
    // Chromium keeps this connection alive even with Connection: close.
    // Read the framed body, rather than waiting for an EOF that never arrives.
    reader
        .read_exact(&mut body)
        .context("Codex 调试响应内容不完整")?;
    serde_json::from_slice(&body).context("Codex 调试响应不是 JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_with_applied_theme(
        state_path: &std::path::Path,
    ) -> (
        super::super::ThemeService,
        std::thread::JoinHandle<Vec<String>>,
    ) {
        use super::super::{ThemeService, Worker, run_worker};
        use std::{net::TcpListener, sync::mpsc, thread};

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let mut methods = Vec::new();
            loop {
                let message = match socket.read() {
                    Ok(Message::Close(_)) => break,
                    Ok(message) => message,
                    Err(tungstenite::Error::Protocol(
                        tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                    )) => break,
                    Err(tungstenite::Error::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("theme socket did not disconnect cleanly: {error}"),
                };
                let Message::Text(message) = message else {
                    panic!("expected CDP request")
                };
                let request: Value = serde_json::from_str(&message).unwrap();
                let method = request["method"].as_str().unwrap();
                match method {
                    "Page.removeScriptToEvaluateOnNewDocument" => {
                        assert_eq!(request["params"]["identifier"], "future-script");
                    }
                    "Runtime.evaluate" => {
                        assert_eq!(request["params"]["expression"], payload::CLEANUP);
                    }
                    _ => panic!("unexpected CDP request: {method}"),
                }
                methods.push(method.to_owned());
                socket
                    .send(Message::Text(
                        json!({
                            "id": request["id"], "result": {"result": {"value": true}}
                        })
                        .to_string()
                        .into(),
                    ))
                    .unwrap();
            }
            methods
        });
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let url = format!("ws://{address}/devtools/page/test");
        let (socket, _) = tungstenite::client(&url, stream).unwrap();
        let mut runtime = Runtime::new();
        runtime.sessions.insert(
            "test".into(),
            Session {
                socket,
                next_id: 0,
                script_id: Some("future-script".into()),
                applied: true,
                first_seen: Instant::now(),
                url,
                broken: false,
                failure: None,
                revision: 1,
                generation: Some("saved-theme".into()),
            },
        );
        let service = ThemeService::new(state_path.to_owned());
        {
            let mut library = service.inner.lock().unwrap();
            library.restored = true;
            library.selected = Some("saved-theme".into());
            library.transparencies.insert("saved-theme".into(), 67);
            *library.status.lock().unwrap() = ThemeStatus::Applied(1);
            library.save().unwrap();
            let (commands, receiver) = mpsc::channel();
            let errors = library.errors.clone();
            let status = library.status.clone();
            library.worker = Some(Worker {
                commands,
                thread: Some(thread::spawn(move || {
                    run_worker(receiver, errors, status, runtime)
                })),
            });
        }
        (service, server)
    }

    #[test]
    fn exiting_or_dropping_service_disconnects_without_removing_theme_or_saved_settings() {
        for explicit_shutdown in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let state_path = temp.path().join("theme-state.json");
            let (service, server) = service_with_applied_theme(&state_path);
            let saved = std::fs::read(&state_path).unwrap();
            if explicit_shutdown {
                service.shutdown().unwrap();
                service.shutdown().unwrap();
                let library = service.inner.lock().unwrap();
                assert!(library.worker.is_none());
                assert_eq!(library.selected.as_deref(), Some("saved-theme"));
                assert_eq!(library.transparencies["saved-theme"], 67);
            }
            drop(service);
            assert!(
                server.join().unwrap().is_empty(),
                "exit must not issue cleanup commands"
            );
            assert_eq!(std::fs::read(&state_path).unwrap(), saved);
        }
    }

    #[test]
    fn selecting_off_still_removes_theme_and_stops_the_worker() {
        let temp = tempfile::tempdir().unwrap();
        let state_path = temp.path().join("theme-state.json");
        let (service, server) = service_with_applied_theme(&state_path);
        service.select(temp.path().to_str().unwrap(), None).unwrap();
        assert!(service.inner.lock().unwrap().worker.is_none());
        assert_eq!(
            server.join().unwrap(),
            [
                "Page.removeScriptToEvaluateOnNewDocument",
                "Runtime.evaluate"
            ]
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(state_path).unwrap()).unwrap();
        assert!(saved["selected"].is_null());
        assert_eq!(saved["transparency"]["saved-theme"], 67);
    }

    #[test]
    fn http_json_finishes_at_content_length_without_waiting_for_disconnect() {
        struct OpenConnection(std::io::Cursor<Vec<u8>>);
        impl Read for OpenConnection {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if self.0.position() == self.0.get_ref().len() as u64 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "server keeps connection open",
                    ));
                }
                // Both the headers and body may arrive in separate packets.
                let length = bytes.len().min(3);
                self.0.read(&mut bytes[..length])
            }
        }
        let body = br#"{"Browser":"Codex","targets":[]}"#;
        let mut response = format!(
            "HTTP/1.1 200 OK\r\ncOnTeNt-LeNgTh: {}\r\nContent-Type: application/json\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let result = read_http_json(OpenConnection(std::io::Cursor::new(response))).unwrap();
        assert_eq!(result["Browser"], "Codex");
    }

    #[test]
    fn http_json_rejects_truncated_or_unframed_responses() {
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nContent-Length: bad\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n{}",
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\n\r\n{}",
        ] {
            assert!(read_http_json(response.as_bytes()).is_err());
        }
    }

    #[test]
    fn replacing_theme_reuses_the_socket_and_replaces_the_previous_startup_script() {
        use std::{net::TcpListener, thread};
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let mut generation = None;
            let mut registered = HashSet::new();
            let mut registrations = 0;
            let mut executions = Vec::new();
            loop {
                let Message::Text(message) = socket.read().unwrap() else {
                    panic!("expected request")
                };
                let request: Value = serde_json::from_str(&message).unwrap();
                let method = request["method"].as_str().unwrap();
                let expression = request["params"]["expression"].as_str().unwrap_or("");
                let result = match method {
                    "Page.addScriptToEvaluateOnNewDocument" => {
                        registrations += 1;
                        let id = format!("script-{registrations}");
                        registered.insert(id.clone());
                        assert_eq!(registered.len(), 1, "old source must be removed first");
                        json!({"identifier":id})
                    }
                    "Page.removeScriptToEvaluateOnNewDocument" => {
                        assert!(
                            registered.remove(request["params"]["identifier"].as_str().unwrap())
                        );
                        json!({})
                    }
                    "Runtime.evaluate" if expression == PROBE => {
                        json!({"result":{"value":{"codex":true,"active":generation.is_some(),"revision":generation,"excluded":false}}})
                    }
                    "Runtime.evaluate" if expression == payload::CLEANUP => {
                        json!({"result":{"value":true}})
                    }
                    "Runtime.evaluate" => {
                        generation = Some(expression.to_owned());
                        executions.push(expression.to_owned());
                        json!({"result":{"value":{"installed":true,"revision":generation}}})
                    }
                    _ => json!({}),
                };
                socket
                    .send(Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
                if expression == payload::CLEANUP {
                    break;
                }
            }
            assert!(registered.is_empty());
            (registrations, executions)
        });
        let stream = TcpStream::connect(address).unwrap();
        let (socket, _) =
            tungstenite::client(format!("ws://{address}/devtools/page/test"), stream).unwrap();
        let mut session = Session {
            socket,
            next_id: 0,
            script_id: None,
            applied: false,
            first_seen: Instant::now(),
            url: format!("ws://{address}/devtools/page/test"),
            broken: false,
            failure: None,
            revision: 0,
            generation: None,
        };
        assert!(session.apply("first-theme", 1).unwrap());
        assert!(session.apply("first-theme", 1).unwrap());
        assert!(session.apply("second-theme", 2).unwrap());
        session.clear().unwrap();
        let (registrations, executions) = server.join().unwrap();
        assert_eq!(registrations, 2);
        assert_eq!(executions, ["first-theme", "second-theme"]);
    }

    #[test]
    #[ignore = "requires a running Codex on port 9347 and CODEX_SWITCH_LIVE_THEME"]
    fn live_codex_payload_compiles() -> Result<()> {
        super::super::verify_listener()?;
        let version = get_json("/json/version")?;
        eprintln!("Browser: {}", version["Browser"]);
        let targets = get_json("/json/list")?;
        let target = targets
            .as_array()
            .unwrap()
            .iter()
            .find(|target| {
                target["type"] == "page"
                    && target["url"].as_str().is_some_and(|url| {
                        url.starts_with("app://") && !url.contains("avatar-overlay")
                    })
            })
            .context("Codex 主页面不存在")?;
        let mut session = Session::connect(target["webSocketDebuggerUrl"].as_str().unwrap())?;
        eprintln!("Probe: {}", session.evaluate(PROBE)?);
        let directory = std::env::var_os("CODEX_SWITCH_LIVE_THEME")
            .context("missing CODEX_SWITCH_LIVE_THEME")?;
        let script = payload::build(std::path::Path::new(&directory), None)?;
        session.call("Runtime.enable", json!({}))?;
        let result = session.call("Runtime.compileScript", json!({"expression":script,"sourceURL":"codex-switch-theme-check.js","persistScript":false}))?;
        ensure!(
            result.get("exceptionDetails").is_none(),
            "主题脚本编译失败：{result}"
        );
        Ok(())
    }

    #[test]
    #[ignore = "temporarily switches themes in a running Codex; requires themes initially off and CODEX_SWITCH_LIVE_THEME"]
    fn live_theme_switch_transparency_and_off_without_restart() -> Result<()> {
        use super::super::ThemeService;
        use std::{fs, path::Path};
        super::super::verify_listener()?;
        let browser = get_json("/json/version")?["webSocketDebuggerUrl"].clone();
        let targets = get_json("/json/list")?;
        let target = targets
            .as_array()
            .unwrap()
            .iter()
            .find(|target| {
                target["type"] == "page"
                    && target["url"].as_str().is_some_and(|url| {
                        url.starts_with("app://") && !url.contains("avatar-overlay")
                    })
            })
            .context("Codex 主页面不存在")?;
        let mut observer = Session::connect(target["webSocketDebuggerUrl"].as_str().unwrap())?;
        ensure!(
            observer.evaluate(PROBE)?["active"] != true,
            "实测前需要关闭主题，以便测试后恢复原状"
        );
        let directory = std::env::var_os("CODEX_SWITCH_LIVE_THEME")
            .context("missing CODEX_SWITCH_LIVE_THEME")?;
        let directory = Path::new(&directory);
        let root = directory.parent().unwrap().to_str().unwrap();
        let temp = tempfile::tempdir()?;
        let fixture = temp.path().join("theme");
        fs::create_dir(&fixture)?;
        fs::write(
            fixture.join("background.png"),
            include_bytes!("../../assets/icon.png"),
        )?;
        fs::write(
            fixture.join("theme.json"),
            r#"{"name":"Codex Switch runtime test","image":"background.png","appearance":"auto"}"#,
        )?;
        let service = ThemeService::new(temp.path().join("state.json"));
        let workflow = (|| -> Result<()> {
            let started = Instant::now();
            eprintln!(
                "Apply existing theme: {}",
                service.select(root, Some(directory.to_string_lossy().into_owned()))?
            );
            let thread = service
                .inner
                .lock()
                .unwrap()
                .worker
                .as_ref()
                .unwrap()
                .thread
                .as_ref()
                .unwrap()
                .thread()
                .id();
            let first = observer.evaluate(PROBE)?;
            ensure!(first["active"] == true, "原主题未生效");
            eprintln!("First apply completed in {:?}", started.elapsed());
            let started = Instant::now();
            eprintln!(
                "Hot switch: {}",
                service.select(
                    temp.path().to_str().unwrap(),
                    Some(fixture.to_string_lossy().into_owned())
                )?
            );
            let second = observer.evaluate(PROBE)?;
            ensure!(
                second["active"] == true && first["revision"] != second["revision"],
                "热切换未更新主题"
            );
            eprintln!("Hot switch completed in {:?}", started.elapsed());
            eprintln!("Transparency: {}", service.set_transparency(67)?);
            let alpha = observer.evaluate(
                "document.documentElement.style.getPropertyValue('--ds-user-surface-alpha')",
            )?;
            ensure!(alpha == "0.33", "透明度未生效：{alpha}");
            ensure!(
                service
                    .inner
                    .lock()
                    .unwrap()
                    .worker
                    .as_ref()
                    .unwrap()
                    .thread
                    .as_ref()
                    .unwrap()
                    .thread()
                    .id()
                    == thread,
                "切换时不应重建主题线程"
            );
            eprintln!(
                "Off: {}",
                service.select(temp.path().to_str().unwrap(), None)?
            );
            ensure!(
                observer.evaluate(PROBE)?["active"] == false,
                "关闭主题后仍有样式"
            );
            ensure!(
                service.inner.lock().unwrap().worker.is_none(),
                "关闭主题后仍有后台线程"
            );
            let clean = observer.evaluate("({skin:document.documentElement.hasAttribute('data-dream-skin'),alpha:document.documentElement.style.getPropertyValue('--ds-user-surface-alpha'),state:!!window.__CODEX_DREAM_SKIN_STATE__})")?;
            ensure!(
                clean["skin"] == false && clean["alpha"] == "" && clean["state"] == false,
                "主题未完整清理：{clean}"
            );
            eprintln!(
                "Re-enable: {}",
                service.select(root, Some(directory.to_string_lossy().into_owned()))?
            );
            ensure!(
                observer.evaluate(PROBE)?["active"] == true,
                "关闭后无法重新开启主题"
            );
            service.set_transparency(67)?;
            let saved = fs::read(temp.path().join("state.json"))?;
            service.shutdown()?;
            ensure!(
                observer.evaluate(PROBE)?["active"] == true,
                "退出工具后主题未保留"
            );
            ensure!(
                service.inner.lock().unwrap().worker.is_none(),
                "退出工具后主题线程未停止"
            );
            ensure!(
                fs::read(temp.path().join("state.json"))? == saved,
                "退出工具修改了已保存的主题设置"
            );
            let restored = ThemeService::new(temp.path().join("state.json"));
            let snapshot = restored.snapshot(root)?;
            ensure!(
                snapshot.selected.as_deref() == directory.to_str()
                    && snapshot.transparency == 67
                    && matches!(snapshot.status, ThemeStatus::Applied(_)),
                "重新打开工具后未恢复主题和透明度"
            );
            let alpha = observer.evaluate(
                "document.documentElement.style.getPropertyValue('--ds-user-surface-alpha')",
            )?;
            ensure!(alpha == "0.33", "重新打开后透明度未生效：{alpha}");
            restored.select(root, None)?;
            service.select(root, None)?;
            ensure!(
                get_json("/json/version")?["webSocketDebuggerUrl"] == browser,
                "测试期间 Codex 被重启"
            );
            Ok(())
        })();
        let cleanup = service.select(root, None);
        let renderer_cleanup = observer.evaluate(payload::CLEANUP);
        workflow?;
        cleanup?;
        renderer_cleanup?;
        ensure!(
            observer.evaluate(PROBE)?["active"] == false,
            "测试完成后未恢复无主题状态"
        );
        Ok(())
    }
    #[test]
    fn websocket_never_connects_to_a_theme_provided_host() {
        assert!(valid_ws(
            &format!("ws://127.0.0.1:{DEBUG_PORT}/devtools/page/ABC-123"),
            "page"
        ));
        for url in [
            "ws://example.com:9347/devtools/page/a",
            "ws://127.0.0.1:80/devtools/page/a",
            "ws://127.0.0.1:9347/devtools/page/../browser/a",
            "ws://127.0.0.1:9347/devtools/page/a?host=evil",
        ] {
            assert!(!valid_ws(url, "page"), "{url}");
        }
    }

    #[test]
    fn cdp_reports_renderer_exceptions_and_removes_future_injection_on_disable() {
        use std::{net::TcpListener, thread};
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let mut methods = Vec::new();
            for _ in 0..5 {
                let Message::Text(message) = socket.read().unwrap() else {
                    panic!("expected CDP request")
                };
                let request: Value = serde_json::from_str(&message).unwrap();
                let method = request["method"].as_str().unwrap();
                methods.push(method.to_owned());
                let result = match method {
                    "Page.addScriptToEvaluateOnNewDocument" => {
                        json!({"identifier":"future-script"})
                    }
                    "Runtime.evaluate" if request["params"]["expression"] == "broken()" => {
                        json!({"exceptionDetails":{"exception":{"description":"Error: image decode failed"}}})
                    }
                    _ => json!({"result":{"value":true}}),
                };
                // Unsolicited events must not be confused with command replies.
                socket
                    .send(Message::Text(
                        json!({"method":"Page.loadEventFired","params":{}})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
                socket
                    .send(Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
            }
            methods
        });
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (socket, _) =
            tungstenite::client(format!("ws://{address}/devtools/page/test"), stream).unwrap();
        let mut session = Session {
            socket,
            next_id: 0,
            script_id: None,
            applied: false,
            first_seen: Instant::now(),
            url: format!("ws://{address}/devtools/page/test"),
            broken: false,
            failure: None,
            revision: 0,
            generation: None,
        };
        session.install("test script").unwrap();
        assert!(
            session
                .evaluate("broken()")
                .unwrap_err()
                .to_string()
                .contains("image decode failed")
        );
        session.clear().unwrap();
        assert_eq!(
            server.join().unwrap(),
            vec![
                "Page.enable",
                "Page.addScriptToEvaluateOnNewDocument",
                "Runtime.evaluate",
                "Page.removeScriptToEvaluateOnNewDocument",
                "Runtime.evaluate"
            ]
        );
    }
}
