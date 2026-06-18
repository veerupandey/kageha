//! Single-threaded, bounded subprocess I/O. No shell and no detached reader/writer threads.
use kageha_contracts::execution::Control;
use std::{
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, Stdio},
    time::Duration,
};
pub struct Output {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub bytes: Vec<u8>,
}
impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("success", &self.success)
            .field("exit_code", &self.exit_code)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}
struct OwnedChild {
    child: Child,
    reaped: bool,
}
impl OwnedChild {
    fn terminate(&mut self) {
        if !self.reaped {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
    fn exited(&self) -> io::Result<bool> {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let code = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if code < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(error);
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.terminate();
    }
}
fn nonblocking(value: &impl AsRawFd) -> io::Result<()> {
    let fd = value.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub fn run(
    command: &mut Command,
    input: Vec<u8>,
    control: &Control<'_>,
    limit: usize,
) -> io::Result<Output> {
    let bounded = Control {
        cancelled: control.cancelled,
        deadline: Some(
            control
                .deadline
                .unwrap_or(std::time::Instant::now() + Duration::from_secs(120)),
        ),
    };
    let control = &bounded;
    control.check()?;
    if input.len() > 64 * 1024 * 1024 || limit > 64 * 1024 * 1024 {
        return Err(io::Error::other(
            "Subprocess input/output exceeds 64 MiB limit",
        ));
    }
    command
        .env_remove("KAGEHA_API_KEY")
        .env_remove("KAGEHA_SERVER_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut owned = OwnedChild {
        child: command.spawn()?,
        reaped: false,
    };
    let mut stdin = Some(owned.child.stdin.take().unwrap());
    let mut stdout = owned.child.stdout.take().unwrap();
    nonblocking(stdin.as_ref().unwrap())?;
    nonblocking(&stdout)?;
    let mut offset = 0;
    let mut bytes = Vec::new();
    let mut buffer = [0; 16384];
    let mut eof = false;
    let mut success = None;
    let mut exit_code = None;
    loop {
        control.check()?;
        let mut progress = false;
        if let Some(pipe) = stdin.as_mut() {
            if offset == input.len() {
                stdin = None;
            } else {
                match pipe.write(&input[offset..input.len().min(offset + 16384)]) {
                    Ok(n) => {
                        offset += n;
                        progress = n > 0;
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                        stdin = None;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        if !eof {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if bytes.len() + n > limit {
                        return Err(io::Error::other("Native output exceeds limit"));
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    progress = true;
                }
                Err(e) if matches!(e.kind(),io::ErrorKind::WouldBlock|io::ErrorKind::Interrupted) => {}
                Err(e) => return Err(e),
            }
        }
        if success.is_none() && owned.exited()? {
            // Keep the unreaped leader as the process-group identity until descendants are stopped.
            unsafe {
                libc::kill(-(owned.child.id() as i32), libc::SIGKILL);
            }
            let status = owned.child.wait()?;
            owned.reaped = true;
            success = Some(status.success());
            exit_code = status.code();
            stdin = None;
        }
        if eof && success.is_some() {
            return Ok(Output {
                success: success.unwrap(),
                exit_code,
                bytes,
            });
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
