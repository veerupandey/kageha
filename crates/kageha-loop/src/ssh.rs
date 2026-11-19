//! OpenSSH transport shared by remote commands, file transfers and forwards.
use std::io;
use std::process::Command;

pub struct Connection<'a> {
    pub target: &'a str,
    pub port: u16,
    pub identity: &'a str,
    pub connect_timeout: u32,
    pub control_path: &'a str,
}

pub enum Operation<'a> {
    Execute(&'a str),
    Control(&'a [&'a str]),
}

impl Connection<'_> {
    pub fn command(&self, operation: Operation<'_>) -> io::Result<Command> {
        if self.target.is_empty() || self.port == 0 {
            return Err(io::Error::other("invalid SSH destination"));
        }
        let mut command = Command::new("ssh");
        command.args([
            "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", &format!("ConnectTimeout={}", self.connect_timeout),
            "-o", "ControlMaster=auto",
            "-o", "ControlPersist=120s",
            "-o", &format!("ControlPath={}", self.control_path),
            "-p", &self.port.to_string(),
        ]);
        if !self.identity.is_empty() {
            command.args(["-o", "IdentitiesOnly=yes", "-i", self.identity]);
        }
        match operation {
            Operation::Execute(remote) => {
                command.args(["--", self.target, remote]);
            }
            Operation::Control(args) => {
                command.args(args).args(["--", self.target]);
            }
        }
        Ok(command)
    }
}

#[cfg(unix)]
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::process::CommandExt;

    let [target, port, identity, timeout, socket, mode, rest @ ..] = args else {
        return Err("missing SSH arguments".into());
    };
    let connection = Connection {
        target,
        port: port.parse()?,
        identity,
        connect_timeout: timeout.parse()?,
        control_path: socket,
    };
    let control: Vec<_> = rest.iter().map(String::as_str).collect();
    let operation = match (mode.as_str(), control.as_slice()) {
        ("exec", [remote]) => Operation::Execute(remote),
        ("control", ["-O", "exit"] | ["-O", "forward" | "cancel", "-L", _]) => {
            Operation::Control(&control)
        }
        _ => return Err("invalid SSH operation".into()),
    };
    Err(connection.command(operation)?.exec().into())
}
