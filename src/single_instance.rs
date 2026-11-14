//! A per-user OS lock plus loopback notifications to show the existing window.
use anyhow::{Context, Result};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const SHOW_REQUEST: &[u8] = b"com.luoluoqixi.codex-switch/show";
const SHOW_ACK: &[u8] = b"ok";

pub(crate) struct SingleInstance {
    _lock: File,
    requests: async_channel::Receiver<()>,
    socket: UdpSocket,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SingleInstance {
    pub(crate) fn acquire() -> Result<Option<Self>> {
        Self::acquire_in(&state_directory()?)
    }

    pub(crate) fn activation_requests(&self) -> async_channel::Receiver<()> {
        self.requests.clone()
    }

    fn acquire_in(directory: &Path) -> Result<Option<Self>> {
        std::fs::create_dir_all(directory).context("无法创建单实例锁目录")?;
        // Never delete or replace this file: another process may already have it
        // open. Only ownership of the OS lock determines whether an app is alive.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("instance.lock"))
            .context("无法打开单实例锁文件")?;
        match lock.try_lock() {
            Ok(()) => Self::listen(lock, directory).map(Some),
            Err(TryLockError::WouldBlock) => {
                if let Err(error) = notify_existing(directory) {
                    eprintln!("程序已在运行，但未能唤起主窗口：{error:#}");
                }
                Ok(None)
            }
            Err(TryLockError::Error(error)) => Err(error).context("无法获取单实例锁"),
        }
    }

    fn listen(lock: File, directory: &Path) -> Result<Self> {
        // Let the OS select a free loopback port; no fixed port can conflict with
        // another application. Keep metadata separate from the Windows-locked file.
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).context("无法创建单实例唤醒通道")?;
        let address = socket.local_addr()?;
        let listener = socket.try_clone()?;
        let (sender, requests) = async_channel::bounded(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let worker = std::thread::Builder::new()
            .name("instance-activation".into())
            .spawn(move || {
                let mut bytes = [0; SHOW_REQUEST.len() + 1];
                loop {
                    // Blocks without polling while the app is idle.
                    let received = listener.recv_from(&mut bytes);
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    match received {
                        Ok((length, peer))
                            if peer.ip().is_loopback() && &bytes[..length] == SHOW_REQUEST =>
                        {
                            // Coalesce repeated launches while GPUI is still starting.
                            let _ = sender.try_send(());
                            let _ = listener.send_to(SHOW_ACK, peer);
                        }
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                ErrorKind::Interrupted
                                    | ErrorKind::ConnectionReset
                                    | ErrorKind::ConnectionRefused
                            ) => {}
                        Err(error) => {
                            eprintln!("单实例唤醒通道已停止：{error}");
                            break;
                        }
                    }
                }
            })
            .context("无法启动单实例唤醒接收器")?;
        let instance = Self {
            _lock: lock,
            requests,
            socket,
            stopping,
            worker: Some(worker),
        };
        // The receiver is already listening before a second process can discover it.
        std::fs::write(
            directory.join("instance.endpoint"),
            format!("{} {}\n", std::process::id(), address.port()),
        )
        .context("无法保存单实例唤醒通道信息")?;
        Ok(instance)
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // Wake the blocking receiver before releasing the lock. No separate service
        // or listener survives the application, including after startup failures.
        if let Ok(address) = self.socket.local_addr() {
            let _ = self.socket.send_to(b"", address);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn notify_existing(directory: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match send_activation(directory) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            // The first instance may have the lock but not yet have published its
            // endpoint. Retry only on this short-lived secondary startup path.
            Err(_) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

fn send_activation(directory: &Path) -> Result<()> {
    let metadata = std::fs::read_to_string(directory.join("instance.endpoint"))?;
    let mut fields = metadata.split_whitespace();
    let pid: u32 = fields.next().context("缺少单实例进程 ID")?.parse()?;
    let port: u16 = fields.next().context("缺少单实例唤醒端口")?.parse()?;
    anyhow::ensure!(pid != 0 && port != 0, "单实例唤醒通道信息无效");
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    socket.connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    #[cfg(target_os = "windows")]
    unsafe {
        // Pass foreground permission from the user-launched process to the owner
        // before asking GPUI to activate its window. Windows may otherwise refuse.
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
    }
    socket.send(SHOW_REQUEST)?;
    let mut reply = [0; SHOW_ACK.len() + 1];
    let length = socket.recv(&mut reply)?;
    anyhow::ensure!(&reply[..length] == SHOW_ACK, "单实例唤醒通道未确认请求");
    Ok(())
}

fn state_directory() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let local_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|home| PathBuf::from(home).join("AppData/Local"))
            })
            .context("无法获取当前用户的应用数据目录")?;
        Ok(local_data.join("Codex Switch"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var_os("HOME").context("无法获取用户主目录")?;
        #[cfg(target_os = "macos")]
        let relative = "Library/Application Support/Codex Switch";
        #[cfg(not(target_os = "macos"))]
        let relative = ".local/state/codex-switch";
        Ok(PathBuf::from(home).join(relative))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    const TEST_DIRECTORY: &str = "CODEX_SWITCH_INSTANCE_TEST_DIRECTORY";
    const TEST_ACTION: &str = "CODEX_SWITCH_INSTANCE_TEST_ACTION";

    // Separate processes exercise the real OS lock rather than only a local flag.
    fn child(directory: &Path, action: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "single_instance::tests::child_process",
                "--nocapture",
            ])
            .env(TEST_DIRECTORY, directory)
            .env(TEST_ACTION, action)
            .stdout(Stdio::null());
        command
    }

    #[test]
    fn child_process() {
        let Some(directory) = std::env::var_os(TEST_DIRECTORY) else {
            return;
        };
        let directory = PathBuf::from(directory);
        let instance = SingleInstance::acquire_in(&directory).unwrap();
        match std::env::var(TEST_ACTION).unwrap().as_str() {
            "blocked" => assert!(instance.is_none()),
            "acquire" => assert!(instance.is_some()),
            "hold" => {
                let _instance = instance.expect("child must own the lock");
                std::fs::write(directory.join("ready"), b"ready").unwrap();
                loop {
                    std::thread::park();
                }
            }
            action => panic!("unknown test action: {action}"),
        }
    }

    #[test]
    fn blocks_other_processes_and_releases_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let instance = SingleInstance::acquire_in(directory.path())
            .unwrap()
            .unwrap();
        assert!(
            child(directory.path(), "blocked")
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(instance.activation_requests().try_recv(), Ok(()));
        drop(instance);
        assert!(
            child(directory.path(), "acquire")
                .status()
                .unwrap()
                .success()
        );
    }

    struct TestChild(Child);

    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn releases_after_forced_process_exit() {
        let directory = tempfile::tempdir().unwrap();
        let mut child = TestChild(child(directory.path(), "hold").spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.path().join("ready").exists() {
            assert!(child.0.try_wait().unwrap().is_none(), "child exited early");
            assert!(Instant::now() < deadline, "child did not acquire the lock");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            SingleInstance::acquire_in(directory.path())
                .unwrap()
                .is_none()
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(
            SingleInstance::acquire_in(directory.path())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn lock_errors_do_not_allow_another_instance() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("instance.lock"), b"old lock file").unwrap();
        // A leftover file alone must not prevent a later launch.
        assert!(
            SingleInstance::acquire_in(directory.path())
                .unwrap()
                .is_some()
        );
        let file = directory.path().join("not-a-directory");
        std::fs::write(&file, b"").unwrap();
        assert!(SingleInstance::acquire_in(&file).is_err());
    }

    #[test]
    fn ignores_unrelated_packets_and_handles_later_launches() {
        let directory = tempfile::tempdir().unwrap();
        let instance = SingleInstance::acquire_in(directory.path())
            .unwrap()
            .unwrap();
        let requests = instance.activation_requests();
        let unrelated = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        unrelated
            .connect(instance.socket.local_addr().unwrap())
            .unwrap();
        unrelated
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        unrelated.send(b"not an activation request").unwrap();
        assert!(unrelated.recv(&mut [0; 32]).is_err());
        assert!(requests.is_empty());

        for _ in 0..3 {
            assert!(
                child(directory.path(), "blocked")
                    .status()
                    .unwrap()
                    .success()
            );
            assert_eq!(requests.try_recv(), Ok(()));
        }
        drop(instance);
        assert!(requests.is_closed());
    }

    #[test]
    fn replaces_endpoint_left_by_a_previous_process() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("instance.endpoint"),
            b"stale endpoint",
        )
        .unwrap();
        let instance = SingleInstance::acquire_in(directory.path())
            .unwrap()
            .unwrap();
        assert!(
            child(directory.path(), "blocked")
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(instance.activation_requests().try_recv(), Ok(()));
    }
}
