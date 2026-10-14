//! Subprocesses for the benchmarks that drive whole build tools: each in a
//! process group of its own, its output to a log, killed with everything it
//! started at a deadline, and an HTTP probe for the services they run.

#![allow(
    dead_code,
    reason = "each benchmark that includes this module uses a part of it"
)]

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The status line's code of a `GET`, or `None` while nothing listens.
pub fn http_status(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let address = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:80")
    };
    let mut stream = address
        .to_socket_addrs()
        .ok()?
        .find_map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2)).ok())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut head = [0u8; 64];
    let mut read = 0;
    while read < 12 {
        let n = stream.read(&mut head[read..]).ok()?;
        if n == 0 {
            break;
        }
        read += n;
    }
    String::from_utf8_lossy(&head[..read])
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// A finished command: its status, `None` when it could not start or was
/// killed at its deadline.
pub struct Exec {
    pub status: Option<ExitStatus>,
    pub failure: Option<String>,
    pub wall: Duration,
}

impl Exec {
    pub fn success(&self) -> bool {
        self.status.is_some_and(|s| s.success())
    }

    pub fn describe(&self) -> String {
        match (&self.status, &self.failure) {
            (_, Some(f)) => f.clone(),
            (Some(s), None) => format!(
                "exited with {}",
                s.code().map_or_else(|| s.to_string(), |c| c.to_string())
            ),
            (None, None) => "did not finish".to_string(),
        }
    }
}

/// Run `command` to its end, its output appended to `log` (stdout to
/// `stdout` instead, when given), killed with its process group after
/// `timeout`. The wall time runs from the spawn to the exit.
pub fn exec(command: &mut Command, log: &Path, stdout: Option<&Path>, timeout: Duration) -> Exec {
    let start = Instant::now();
    let mut child = match spawn(command, log, stdout) {
        Ok(child) => child,
        Err(e) => {
            return Exec {
                status: None,
                failure: Some(format!("could not start: {e}")),
                wall: start.elapsed(),
            };
        }
    };
    let (status, timed_out) = wait(&mut child, timeout);
    let wall = start.elapsed();
    match (status, timed_out) {
        (_, true) => {
            stop(&mut child);
            Exec {
                status: None,
                failure: Some(format!("timed out after {} s", timeout.as_secs())),
                wall,
            }
        }
        (Ok(status), false) => Exec {
            status: Some(status),
            failure: None,
            wall,
        },
        (Err(e), false) => Exec {
            status: None,
            failure: Some(e.to_string()),
            wall,
        },
    }
}

/// Block until the child exits, so its exit is seen when it happens rather
/// than at the next poll: a watchdog thread kills its group at the
/// deadline, which ends the wait.
#[cfg(unix)]
fn wait(child: &mut Child, timeout: Duration) -> (std::io::Result<ExitStatus>, bool) {
    let group = i32::try_from(child.id()).unwrap_or(0);
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        let expired = matches!(
            finished.recv_timeout(timeout),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        if expired && group > 0 {
            // SAFETY: `kill` takes no pointers; a stale group id fails with ESRCH.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        expired
    });
    let status = child.wait();
    let _ = done.send(());
    (status, watchdog.join().unwrap_or(false))
}

#[cfg(not(unix))]
fn wait(child: &mut Child, timeout: Duration) -> (std::io::Result<ExitStatus>, bool) {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (Ok(status), false),
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                return (child.wait(), true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return (Err(e), false),
        }
    }
}

/// Start `command` in a process group of its own, so that [`stop`] reaches
/// the JVM `jrs run` or a wrapper started too.
pub fn spawn(command: &mut Command, log: &Path, stdout: Option<&Path>) -> std::io::Result<Child> {
    let mut file = File::options().create(true).append(true).open(log)?;
    writeln!(file, "$ {command:?}")?;
    let out = match stdout {
        Some(path) => File::create(path)?,
        None => file.try_clone()?,
    };
    command.stdin(Stdio::null()).stdout(out).stderr(file);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.spawn()
}

/// `SIGTERM` to the child's process group, so shutdown hooks run and the
/// port is free for the next step; `SIGKILL` after fifteen seconds.
pub fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        let Ok(group) = i32::try_from(child.id()) else {
            let _ = child.kill();
            let _ = child.wait();
            return;
        };
        // SAFETY: `kill` takes no pointers; a stale group id fails with ESRCH.
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // The group outlives its leader when the JVM ignored the signal.
        // SAFETY: as above.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}
