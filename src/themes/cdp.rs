//! Small synchronous CDP client. Used exclusively on the optional theme thread.
use super::{DEBUG_PORT, payload};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket};

const TIMEOUT: Duration = Duration::from_secs(2);
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
 return {codex: location.protocol === 'app:' && !excluded && (!!shell && !!sidebar || !!home || !!settings || !!branded),
 active: !!window.__CODEX_DREAM_SKIN_STATE__, excluded};
})()"#;

struct Session {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    script_id: Option<String>,
    applied: bool,
    first_seen: Instant,
    url: String,
    broken: bool,
    failed: bool,
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
            failed: false,
        })
    }
    fn reconnect(&mut self, script: &str) -> Result<()> {
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
        self.install(script)?;
        self.broken = false;
        self.applied = false;
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            ensure!(Instant::now() < deadline, "等待 Codex 主题响应超时");
            let message = self.socket.read().context("无法读取 Codex 主题响应")?;
            if let Message::Text(text) = message {
                let response: Value = serde_json::from_str(&text)?;
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
    fn clear(&mut self) -> Result<()> {
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
        remove.and(cleanup)
    }
}

pub(super) struct Runtime {
    browser: Option<String>,
    sessions: HashMap<String, Session>,
}
impl Runtime {
    pub(super) fn new() -> Self {
        Self {
            browser: None,
            sessions: HashMap::new(),
        }
    }
    pub(super) fn refresh(&mut self, script: &str) -> Result<()> {
        // No endpoint is normal until the user chooses Open/Restart themselves.
        let version = match get_json("/json/version") {
            Ok(value) => value,
            // Keep existing sockets and script identifiers across temporary
            // discovery failures so Off can still remove previously injected CSS.
            Err(_) => return Ok(()),
        };
        let browser = version["webSocketDebuggerUrl"]
            .as_str()
            .context("调试端口不是可用的 Codex 浏览器")?;
        super::verify_listener()?;
        ensure!(valid_ws(browser, "browser"), "Codex 浏览器调试地址无效");
        if self.browser.as_deref() != Some(browser) {
            self.sessions.clear();
            self.browser = Some(browser.into());
        }
        let targets = get_json("/json/list")?;
        let targets = targets.as_array().context("Codex 窗口列表格式无效")?;
        let mut alive = HashSet::new();
        let mut failure = None;
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
            if !self.sessions.contains_key(id) {
                let Some(ws) = target["webSocketDebuggerUrl"].as_str() else {
                    continue;
                };
                ensure!(
                    ws.ends_with(&format!("/devtools/page/{id}")),
                    "Codex 窗口调试标识不一致"
                );
                let mut session = Session::connect(ws)?;
                session.install(script)?;
                self.sessions.insert(id.to_string(), session);
            }
            let session = self.sessions.get_mut(id).unwrap();
            if session.failed {
                continue;
            }
            let attempt = (|| -> Result<()> {
                if session.broken {
                    session.reconnect(script)?;
                }
                let probe = session.evaluate(PROBE)?;
                if probe["excluded"] == true {
                    session.clear()?;
                    return Ok(());
                }
                if probe["codex"] != true {
                    return Ok(());
                }
                if !session.applied || probe["active"] != true {
                    let result = session.evaluate(script)?;
                    ensure!(
                        result["installed"] == true || result["cancelled"] == true,
                        "Codex 尚未确认主题生效"
                    );
                    session.applied = true;
                }
                Ok(())
            })();
            if let Err(error) = attempt {
                // Navigation can destroy an execution context while it is being
                // queried. Give the real renderer time to become ready first.
                let message = error.to_string();
                let transient = message.contains("context")
                    || message.contains("Context")
                    || message.contains("navigation")
                    || message.contains("尚未确认");
                let transport = error.downcast_ref::<tungstenite::Error>().is_some()
                    || error.downcast_ref::<std::io::Error>().is_some();
                session.broken = transport;
                if !transient && !transport {
                    session.failed = true;
                }
                if !transient || session.first_seen.elapsed() > Duration::from_secs(15) {
                    failure = Some(error);
                }
                session.applied = false;
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
        Ok(())
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
    let mut bytes = Vec::new();
    stream.take(4 * 1024 * 1024).read_to_end(&mut bytes)?;
    let header_end = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .context("Codex 调试响应无效")?;
    ensure!(
        bytes.starts_with(b"HTTP/1.1 200 ") || bytes.starts_with(b"HTTP/1.0 200 "),
        "Codex 调试请求失败"
    );
    serde_json::from_slice(&bytes[header_end + 4..]).context("Codex 调试响应不是 JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
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
            failed: false,
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
