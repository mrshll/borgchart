//! Bounded direct child execution. Never invokes a shell or logs child data.
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub deadline: Instant,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Deadline,
    OutputLimit(Stream),
    Io(io::ErrorKind),
}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadline => f.write_str("process deadline exceeded"),
            Self::OutputLimit(stream) => write!(f, "process {stream:?} limit exceeded"),
            Self::Io(kind) => write!(f, "process I/O failed ({kind:?})"),
        }
    }
}
impl std::error::Error for Error {}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Also end descendants holding pipes after their leader exited. Never
        // wait for those pipes, or for a child that cannot be reaped promptly.
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let until = Instant::now() + Duration::from_millis(250);
        loop {
            match self.0.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if Instant::now() >= until => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

fn nonblocking(fd: &impl AsFd) -> Result<(), Error> {
    let flags = rustix::fs::fcntl_getfl(fd).map_err(io::Error::from)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK).map_err(io::Error::from)?;
    Ok(())
}

fn drain(
    pipe: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    stream: Stream,
) -> Result<bool, Error> {
    let mut buffer = [0; 8192];
    match pipe.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(n) => {
            if n > limit.saturating_sub(bytes.len()) {
                return Err(Error::OutputLimit(stream));
            }
            bytes.extend_from_slice(&buffer[..n]);
            Ok(false)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// Execute exactly the configured program and argv, overriding stdio and the
/// process group. Nonzero exits are normal outputs. Errors discard output and
/// contain no child-supplied text. Cleanup may take up to 250ms beyond deadline.
pub fn run(command: &mut Command, input: Option<&[u8]>, limits: Limits) -> Result<Output, Error> {
    if Instant::now() >= limits.deadline {
        return Err(Error::Deadline);
    }
    let mut child = OwnedChild(
        command
            .process_group(0)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut stdin = child.0.stdin.take();
    let mut stdout = child.0.stdout.take().expect("piped stdout");
    let mut stderr = child.0.stderr.take().expect("piped stderr");
    if let Some(pipe) = &stdin {
        nonblocking(pipe)?;
    }
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut remaining = input.unwrap_or_default();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_eof, mut err_eof) = (false, false);
    loop {
        if Instant::now() >= limits.deadline {
            return Err(Error::Deadline);
        }
        if remaining.is_empty() {
            stdin.take();
        }
        if let Some(pipe) = &mut stdin {
            match pipe.write(remaining) {
                Ok(0) => {
                    return Err(Error::Io(io::ErrorKind::WriteZero));
                }
                Ok(n) => remaining = &remaining[n..],
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                    stdin.take();
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        if !out_eof {
            out_eof = drain(&mut stdout, &mut out, limits.stdout_bytes, Stream::Stdout)?;
        }
        if !err_eof {
            err_eof = drain(&mut stderr, &mut err, limits.stderr_bytes, Stream::Stderr)?;
        }
        if let Some(status) = child.0.try_wait()?
            && out_eof
            && err_eof
        {
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        std::thread::sleep(
            Duration::from_millis(2).min(limits.deadline.saturating_duration_since(Instant::now())),
        );
    }
}
