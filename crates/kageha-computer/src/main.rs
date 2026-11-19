use kageha_computer::dispatch;
#[cfg(test)]
use kageha_computer::instance_nonce;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
const LIMIT: usize = 8 * 1024 * 1024;

fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let part = reader.fill_buf()?;
        if part.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated JSON frame",
                ))
            };
        }
        let count = part
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(part.len());
        if frame.len() + count > LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request exceeds 8 MiB",
            ));
        }
        let complete = part[count - 1] == b'\n';
        frame.extend_from_slice(&part[..count]);
        reader.consume(count);
        if complete {
            return Ok(Some(frame));
        }
    }
}
fn serve<R: BufRead, W: Write>(mut reader: R, mut writer: W) -> io::Result<()> {
    let mut generation = 0;
    while let Some(frame) = read_frame(&mut reader)? {
        let response = match serde_json::from_slice(&frame) {
            Ok(v) => dispatch(v, &mut generation),
            Err(e) => json!({"id":null,"error":{"code":"invalid_json","message":e.to_string()}}),
        };
        let mut encoded = serde_json::to_vec(&response)?;
        if encoded.len() >= LIMIT {
            encoded = serde_json::to_vec(
                &json!({"id":response["id"],"error":{"code":"response_too_large","message":"native response exceeds 8 MiB; request fewer elements"}}),
            )?;
        }
        writer.write_all(&encoded)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

fn serve_control(
    path: &str,
    allow_input: bool,
    resume_control: bool,
    mcp: bool,
    workspace: &str,
    client: &str,
    socket: Option<&str>,
) -> io::Result<()> {
    use kageha_computer::{control::Controller, NativeBackend};
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return Err(io::Error::other("journal path must be absolute"));
    }
    let (mut reader, mut writer): (Box<dyn BufRead>, Box<dyn Write>) = if let Some(socket) = socket
    {
        let stream = accept_socket(socket)?;
        (
            Box::new(io::BufReader::new(stream.try_clone()?)),
            Box::new(stream),
        )
    } else {
        (
            Box::new(io::BufReader::new(io::stdin())),
            Box::new(io::stdout()),
        )
    };
    let mut controller =
        Controller::open(NativeBackend::default(), path).map_err(io::Error::other)?;
    if allow_input {
        controller.set_authorizer(|_, _, _| Ok(()));
    }
    if resume_control {
        controller.resume_control().map_err(io::Error::other)?;
    }
    if mcp {
        return kageha_computer::mcp::serve(&mut controller, workspace, client, reader, writer);
    }
    while let Some(frame) = read_frame(&mut reader)? {
        let response = match serde_json::from_slice::<Value>(&frame) {
            Ok(request)
                if request["id"].is_u64()
                    && request["method"] == "control"
                    && request.as_object().is_some_and(|map| {
                        map.keys()
                            .all(|key| ["id", "method", "args"].contains(&key.as_str()))
                    }) =>
            {
                json!({"id":request["id"],"result":controller.handle(request["args"].clone())})
            }
            _ => {
                json!({"id":null,"error":{"code":"invalid_request","message":"control mode requires {id:unsigned,method:control,args:object}; legacy operations are not permitted"}})
            }
        };
        let mut encoded = serde_json::to_vec(&response)?;
        if encoded.len() >= LIMIT {
            encoded = serde_json::to_vec(
                &json!({"id":response["id"],"error":{"code":"response_too_large","message":"reduce observation scope; do not replay input"}}),
            )?;
        }
        writer.write_all(&encoded)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

#[cfg(unix)]
fn accept_socket(path: &str) -> io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    use std::time::{Duration, Instant};
    let path = std::path::Path::new(path);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("socket parent required"))?;
    if std::fs::metadata(parent)?.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other("socket directory must be private"));
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(8);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                let _ = std::fs::remove_file(path);
                return Err(e);
            }
        }
    };
    drop(listener);
    let _ = std::fs::remove_file(path);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(unix)]
fn serve_socket(path: &str) -> io::Result<()> {
    let stream = accept_socket(path)?;
    serve(io::BufReader::new(stream.try_clone()?), stream)
}

#[cfg(target_os = "macos")]
fn proxy_app(args: &[String], app: &std::path::Path) -> io::Result<()> {
    use std::os::unix::{fs::DirBuilderExt, net::UnixStream};
    use std::time::{Duration, Instant};
    let dir =
        std::path::Path::new("/tmp").join(format!("kg-app-{}", kageha_computer::instance_nonce()));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let socket = dir.join("ipc");
    let opened = std::process::Command::new("/usr/bin/open")
        .args(["-n", "-g", "-a"])
        .arg(app)
        .arg("--args")
        .args(args)
        .arg("--socket")
        .arg(&socket)
        .status()?;
    if !opened.success() {
        return Err(io::Error::other(
            "LaunchServices refused the computer helper",
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut stream = loop {
        match UnixStream::connect(&socket) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(e),
        }
    };
    let mut input = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut input);
        let _ = input.shutdown(std::net::Shutdown::Write);
    });
    io::copy(&mut stream, &mut io::stdout().lock())?;
    Ok(())
}

fn main() -> io::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let socket = if let Some(i) = args.iter().position(|s| s == "--socket") {
        if i + 1 >= args.len() {
            return Err(io::Error::other("socket path required"));
        }
        let path = args.remove(i + 1);
        args.remove(i);
        Some(path)
    } else {
        None
    };
    #[cfg(target_os = "macos")]
    if socket.is_none() {
        let exe = std::env::current_exe()?;
        if let Some(app) = exe
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
        {
            return proxy_app(&args, app);
        }
    }
    if args.len() == 2 && args[0] == "permissions" {
        let kind = match args[1].as_str() {
            "--request-screen-recording" => "screen_recording",
            "--request-accessibility" => "accessibility",
            "--request-input-monitoring" => "input_monitoring",
            _ => return Err(io::Error::other("Unsupported permission request")),
        };
        let mut channel = socket.as_deref().map(accept_socket).transpose()?;
        let reply = kageha_computer::request_permission(kind).map_err(io::Error::other)?;
        if let Some(stream) = channel.as_mut() {
            writeln!(stream, "{}", reply)?;
        } else {
            println!("{}", reply);
        }
        return Ok(());
    }
    if args.len() >= 3 && args[0] == "control" && args[1] == "--journal" {
        let mut input = false;
        let mut resume_control = false;
        let mut mcp = false;
        let mut scope = ("standalone".to_owned(), "agent".to_owned());
        let mut at = 3;
        while at < args.len() {
            match args[at].as_str() {
                "--allow-input" if !input => input = true,
                "--resume-control" if !resume_control => resume_control = true,
                "--mcp" if !mcp => mcp = true,
                "--workspace" if at + 1 < args.len() => {
                    at += 1;
                    scope.0 = args[at].clone();
                }
                "--client" if at + 1 < args.len() => {
                    at += 1;
                    scope.1 = args[at].clone();
                }
                _ => return Err(io::Error::other("invalid or duplicate control argument")),
            }
            at += 1;
        }
        return serve_control(
            &args[2],
            input,
            resume_control,
            mcp,
            &scope.0,
            &scope.1,
            socket.as_deref(),
        );
    }
    #[cfg(unix)]
    if args.len() == 3 && args[0] == "serve" && args[1] == "--socket" {
        return serve_socket(&args[2]);
    }
    if args != ["serve", "--stdio"] && args != ["serve"] && args != ["--stdio"] {
        eprintln!("usage: kageha-computer serve [--stdio | --socket PATH]\n       kageha-computer control --journal ABSOLUTE_PRIVATE_DB_PATH [--allow-input] [--mcp --workspace ID --client ID]");
        std::process::exit(2);
    }
    if let Some(socket) = socket {
        return serve_socket(&socket);
    }
    serve(io::stdin().lock(), io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn owned_socket_serves_one_connection_and_exits_on_eof() {
        use std::os::unix::{fs::DirBuilderExt, net::UnixStream};
        let directory = std::env::temp_dir().join(format!("kg-socket-test-{}", instance_nonce()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let path = directory.join("ipc");
        let server_path = path.clone();
        let worker = std::thread::spawn(move || serve_socket(server_path.to_str().unwrap()));
        let connect_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut stream = loop {
            assert!(
                std::time::Instant::now() < connect_deadline,
                "socket startup timed out"
            );
            match UnixStream::connect(&path) {
                Ok(stream) => break stream,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        };
        stream
            .write_all(b"{\"id\":1,\"method\":\"hello\",\"args\":{}}\n")
            .unwrap();
        let mut reader = io::BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["result"]["backend"], "kageha-rust");
        assert!(!path.exists());
        drop(reader);
        worker.join().unwrap().unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn hello_reports_protocol() {
        let r = dispatch(json!({"id":1,"method":"hello"}), &mut 0);
        assert_eq!(r["result"]["protocol"], 1);
    }
    #[test]
    fn unknown_method_fails_closed() {
        assert_eq!(
            dispatch(json!({"id":3,"method":"shell"}), &mut 0)["error"]["code"],
            "unsupported_method"
        );
    }
    #[test]
    fn malformed_id_rejected() {
        assert_eq!(
            dispatch(json!({"method":"hello"}), &mut 0)["error"]["code"],
            "invalid_request"
        );
    }
    #[test]
    fn frame_requires_newline() {
        assert!(read_frame(&mut &b"{}"[..]).is_err());
    }
    #[test]
    fn frames_do_not_merge() {
        let mut input = &b"{}\n{}\n"[..];
        assert_eq!(read_frame(&mut input).unwrap().unwrap(), b"{}\n");
        assert_eq!(read_frame(&mut input).unwrap().unwrap(), b"{}\n");
    }
    #[test]
    fn oversized_frame_rejected() {
        let bytes = vec![b'x'; LIMIT + 1];
        assert!(read_frame(&mut bytes.as_slice()).is_err());
    }
}
