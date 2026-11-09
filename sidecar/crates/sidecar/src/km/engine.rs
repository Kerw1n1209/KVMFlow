//! Supervises one `deskflow-core` process. The engine gets its own temp and
//! config homes, so it never reads or rewrites a standalone Deskflow's
//! settings, and it is refused while another Deskflow engine is running:
//! two engines would fight over the same keyboard and mouse.
use super::{profile::Role, KmError, Result};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc::Sender, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const MAX_LINE: u64 = 4096;
const POLL: Duration = Duration::from_millis(100);
const GRACE: Duration = Duration::from_millis(1500);
const RECORD: &str = "engine.pid";

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Output(String),
    /// The process ended; `stopped` is false when it exited on its own.
    Exited {
        stopped: bool,
        detail: String,
    },
}

pub struct Launch<'a> {
    pub binary: &'a Path,
    pub role: Role,
    pub settings: &'a Path,
    /// Private homes for this engine, from [`namespace`].
    pub namespace: &'a Path,
    pub port: u16,
}

/// A short per-profile directory under the system temp dir. macOS places the
/// engine's IPC socket here, and socket paths are limited to 104 bytes.
pub fn namespace(profile: &Path) -> PathBuf {
    let hash = Sha256::digest(profile.to_string_lossy().as_bytes());
    let name: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    std::env::temp_dir().join(format!("kvmflow-km-{name}"))
}

pub struct Engine {
    child: Arc<Mutex<Option<Child>>>,
    events: Sender<Event>,
    pid: u32,
    record: PathBuf,
}

impl Engine {
    pub fn start(launch: &Launch, events: Sender<Event>) -> Result<Self> {
        let temp = launch.namespace.join("tmp");
        std::fs::create_dir_all(&temp)?;
        let record = launch.namespace.join(RECORD);
        end_orphan(&record);
        assert_no_running_engine(&temp)?;
        if launch.role == Role::Server {
            std::net::TcpListener::bind(("0.0.0.0", launch.port))
                .map_err(|_| KmError::new("E_KM_PORT", "keyboard and mouse port is in use"))?;
        }
        let mut command = Command::new(launch.binary);
        command
            .arg(match launch.role {
                Role::Server => "server",
                Role::Client => "client",
            })
            .arg("--settings")
            .arg(launch.settings)
            .env("XDG_CONFIG_HOME", launch.namespace.join("config"))
            .env("XDG_STATE_HOME", launch.namespace.join("state"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "macos")]
        command.env("TMPDIR", format!("{}/", temp.display()));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .spawn()
            .map_err(|e| KmError::new("E_KM_START", format!("could not start the engine: {e}")))?;
        let pid = child.id();
        let entry = format!("{pid}\n{}\n", launch.binary.display());
        let _ = super::write_private(&record, entry.as_bytes(), true);
        for pipe in [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let events = events.clone();
            thread::spawn(move || forward_output(pipe, &events));
        }
        let child = Arc::new(Mutex::new(Some(child)));
        let watched = Arc::clone(&child);
        let exits = events.clone();
        thread::spawn(move || loop {
            thread::sleep(POLL);
            let mut slot = watched.lock().unwrap_or_else(|e| e.into_inner());
            let Some(process) = slot.as_mut() else { return };
            if let Ok(Some(status)) = process.try_wait() {
                *slot = None;
                let _ = exits.send(Event::Exited {
                    stopped: false,
                    detail: status.to_string(),
                });
                return;
            }
        });
        Ok(Self {
            child,
            events,
            pid,
            record,
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_running(&self) -> bool {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Asks the engine to exit, then forces it after a grace period. Returns
    /// only once the process is gone, so a new engine never overlaps it.
    pub fn stop(&self) -> Result<()> {
        let mut slot = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut process) = slot.take() else {
            let _ = std::fs::remove_file(&self.record);
            return Ok(());
        };
        terminate(&mut process);
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline {
            if process.try_wait()?.is_some() {
                break;
            }
            thread::sleep(POLL / 2);
        }
        if process.try_wait()?.is_none() {
            process.kill()?;
        }
        let status = process.wait()?;
        let _ = std::fs::remove_file(&self.record);
        let _ = self.events.send(Event::Exited {
            stopped: true,
            detail: status.to_string(),
        });
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// SIGTERM lets the engine release input and restore the cursor. Windows has
/// no equivalent for a windowless process, so it is ended directly.
fn terminate(process: &mut Child) {
    #[cfg(unix)]
    send_signal(process.id(), "-TERM");
    #[cfg(not(unix))]
    let _ = process.kill();
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: &str) {
    let _ = Command::new("/bin/kill")
        .arg(signal)
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The executable a process runs, or None once it is gone.
#[cfg(unix)]
fn process_image(pid: u32) -> Option<PathBuf> {
    let output = Command::new("/bin/ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let path = text.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(windows)]
fn process_image(pid: u32) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Get-Process -Id {pid}).Path"),
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let path = text.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// A KVMFlow that crashed leaves its engine holding the keyboard and mouse.
/// Only a process still running the recorded executable is ended, so a reused
/// process id or a standalone Deskflow is left alone.
fn end_orphan(record: &Path) {
    let Ok(text) = std::fs::read_to_string(record) else {
        return;
    };
    let _ = std::fs::remove_file(record);
    let Some((pid, binary)) = text.trim().split_once('\n') else {
        return;
    };
    let Ok(pid) = pid.parse::<u32>() else { return };
    let binary = Path::new(binary);
    let running = || process_image(pid).is_some_and(|image| same_file(&image, binary));
    #[cfg(unix)]
    let attempts = ["-TERM", "-KILL"];
    #[cfg(windows)]
    let attempts = ["/F"];
    for attempt in attempts {
        if !running() {
            return;
        }
        #[cfg(unix)]
        send_signal(pid, attempt);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), attempt])
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .output();
        }
        let deadline = Instant::now() + GRACE;
        while running() && Instant::now() < deadline {
            thread::sleep(POLL / 2);
        }
    }
}

/// Debug output and anything mentioning the clipboard stay out of the log;
/// over-long lines are cut so a misbehaving engine cannot grow memory.
fn forward_output(pipe: Box<dyn Read + Send>, events: &Sender<Event>) {
    let mut reader = BufReader::new(pipe);
    let mut line = Vec::new();
    loop {
        line.clear();
        match (&mut reader).take(MAX_LINE).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !line.ends_with(b"\n") {
            let mut rest = Vec::new();
            loop {
                rest.clear();
                match (&mut reader).take(MAX_LINE).read_until(b'\n', &mut rest) {
                    Ok(n) if n > 0 && !rest.ends_with(b"\n") => continue,
                    _ => break,
                }
            }
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        let lower = text.to_ascii_lowercase();
        if text.is_empty()
            || lower.contains("debug")
            || lower.contains("verbose")
            || lower.contains("clipboard")
        {
            continue;
        }
        if events.send(Event::Output(text.to_string())).is_err() {
            return;
        }
    }
}

/// Only opens the known IPC endpoints; it never sends a command to, or ends,
/// an engine another application started.
fn assert_no_running_engine(temp: &Path) -> Result<()> {
    let running = || {
        KmError::new(
            "E_KM_ENGINE_RUNNING",
            "another Deskflow engine is running; quit Deskflow and try again",
        )
    };
    #[cfg(unix)]
    for socket in [
        std::env::temp_dir().join("deskflow-core"),
        temp.join("deskflow-core"),
    ] {
        use std::io::ErrorKind::{ConnectionRefused, NotFound};
        match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(_) => return Err(running()),
            Err(e) if matches!(e.kind(), NotFound | ConnectionRefused) => {}
            Err(e) => {
                return Err(KmError::new(
                    "E_KM_START",
                    format!("could not check for an engine: {e}"),
                ))
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = temp;
        const ERROR_PIPE_BUSY: i32 = 231;
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(r"\\.\pipe\deskflow-core")
        {
            Ok(_) => return Err(running()),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => return Err(running()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(KmError::new(
                    "E_KM_START",
                    format!("could not check for an engine: {e}"),
                ))
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, sync::mpsc};

    fn fake_engine(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("deskflow-core");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn launch<'a>(binary: &'a Path, namespace: &'a Path, role: Role, port: u16) -> Launch<'a> {
        Launch {
            binary,
            role,
            settings: Path::new("/profile/Deskflow.conf"),
            namespace,
            port,
        }
    }

    /// A port just released can be handed straight to another test's bind.
    static PORTS: Mutex<()> = Mutex::new(());

    fn free_port() -> u16 {
        std::net::TcpListener::bind(("0.0.0.0", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn runs_with_private_homes_and_stops_on_request() {
        let _ports = PORTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "echo \"args $*\"\necho \"home $XDG_CONFIG_HOME\"\necho 'DEBUG: noisy'\n\
             echo 'clipboard: secret' >&2\necho 'ready'\nexec sleep 30",
        );
        let ns = dir.path().join("ns");
        let (tx, rx) = mpsc::channel();
        let engine = Engine::start(&launch(&binary, &ns, Role::Server, free_port()), tx).unwrap();
        let mut lines = Vec::new();
        while lines.last().map(String::as_str) != Some("ready") {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Output(line) => lines.push(line),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(
            lines,
            [
                "args server --settings /profile/Deskflow.conf".to_string(),
                format!("home {}", ns.join("config").display()),
                "ready".to_string(),
            ]
        );
        assert!(engine.is_running());
        let started = Instant::now();
        engine.stop().unwrap();
        assert!(started.elapsed() < GRACE, "SIGTERM should end it promptly");
        assert!(!engine.is_running());
        let exit = rx
            .iter()
            .find(|e| matches!(e, Event::Exited { .. }))
            .unwrap();
        assert!(matches!(exit, Event::Exited { stopped: true, .. }));
    }

    #[test]
    fn an_engine_that_ignores_sigterm_is_forced() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "trap '' TERM\necho ready\nwhile :; do sleep 1; done",
        );
        let ns = dir.path().join("ns");
        let (tx, rx) = mpsc::channel();
        let engine = Engine::start(&launch(&binary, &ns, Role::Client, 0), tx).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Output("ready".into())
        );
        engine.stop().unwrap();
        assert!(!engine.is_running());
    }

    #[test]
    fn reports_an_engine_that_exits_by_itself() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), "exit 3");
        let ns = dir.path().join("ns");
        let (tx, rx) = mpsc::channel();
        let engine = Engine::start(&launch(&binary, &ns, Role::Client, 0), tx).unwrap();
        let exit = rx
            .iter()
            .find(|e| matches!(e, Event::Exited { .. }))
            .unwrap();
        assert!(matches!(exit, Event::Exited { stopped: false, .. }));
        assert!(!engine.is_running());
        engine.stop().unwrap();
    }

    #[test]
    fn refuses_a_taken_port_or_a_running_engine() {
        let _ports = PORTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), "exit 0");
        let ns = dir.path().join("ns");
        let taken = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let (tx, _rx) = mpsc::channel();
        let error = Engine::start(&launch(&binary, &ns, Role::Server, port), tx.clone())
            .err()
            .unwrap();
        assert_eq!(error.code, "E_KM_PORT");
        let _listener =
            std::os::unix::net::UnixListener::bind(ns.join("tmp/deskflow-core")).unwrap();
        let error = Engine::start(&launch(&binary, &ns, Role::Client, 0), tx)
            .err()
            .unwrap();
        assert_eq!(error.code, "E_KM_ENGINE_RUNNING");
    }

    /// A copy of `sleep` under the engine's name, so `ps` reports its own path.
    fn orphan(dir: &Path) -> (std::process::Child, PathBuf) {
        let binary = dir.join("deskflow-core");
        std::fs::copy("/bin/sleep", &binary).unwrap();
        let child = Command::new(&binary).arg("30").spawn().unwrap();
        (child, binary)
    }

    fn wait_for_exit(child: &mut std::process::Child) -> bool {
        let deadline = Instant::now() + GRACE * 3;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            thread::sleep(POLL / 2);
        }
        false
    }

    #[test]
    fn ends_the_engine_a_crashed_run_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let (mut child, binary) = orphan(dir.path());
        let record = dir.path().join(RECORD);
        std::fs::write(&record, format!("{}\n{}\n", child.id(), binary.display())).unwrap();
        // A freshly spawned process reports its parent's image until exec.
        while !process_image(child.id()).is_some_and(|image| same_file(&image, &binary)) {
            thread::sleep(POLL / 10);
        }
        end_orphan(&record);
        assert!(wait_for_exit(&mut child), "the orphan is still running");
        assert!(!record.exists());
    }

    #[test]
    fn a_reused_process_id_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (mut child, _) = orphan(dir.path());
        let record = dir.path().join(RECORD);
        let other = dir.path().join("other");
        std::fs::write(&other, "").unwrap();
        std::fs::write(&record, format!("{}\n{}\n", child.id(), other.display())).unwrap();
        end_orphan(&record);
        assert!(child.try_wait().unwrap().is_none());
        assert!(!record.exists());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_clean_stop_clears_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), "exec sleep 30");
        let ns = dir.path().join("ns");
        let (tx, _rx) = mpsc::channel();
        let engine = Engine::start(&launch(&binary, &ns, Role::Client, 0), tx).unwrap();
        assert!(ns.join(RECORD).exists());
        engine.stop().unwrap();
        assert!(!ns.join(RECORD).exists());
    }

    #[test]
    fn long_lines_are_cut() {
        let (tx, rx) = mpsc::channel();
        let input = format!("{}\nnext\n", "x".repeat(10_000));
        forward_output(Box::new(std::io::Cursor::new(input.into_bytes())), &tx);
        drop(tx);
        let lines: Vec<_> = rx.iter().collect();
        assert_eq!(lines.len(), 2);
        assert!(matches!(&lines[0], Event::Output(l) if l.len() == MAX_LINE as usize));
        assert_eq!(lines[1], Event::Output("next".into()));
    }

    #[test]
    fn namespace_is_stable_and_short() {
        let a = namespace(Path::new("/a"));
        assert_eq!(a, namespace(Path::new("/a")));
        assert_ne!(a, namespace(Path::new("/b")));
        assert!(a.join("tmp/deskflow-core").as_os_str().len() < 104);
    }
}
