use kageha_sandbox::ProcessSandbox;
#[test]
fn rejects_unbounded_roots_and_never_uses_an_unconfined_fallback() {
    let sandbox = ProcessSandbox {
        read_roots: vec!["/".into()],
        ..Default::default()
    };
    assert!(sandbox.command("/bin/sh", &[]).is_err());
    let missing = ProcessSandbox {
        read_roots: vec!["/does-not-exist-kageha".into()],
        ..Default::default()
    };
    assert!(missing.command("/bin/sh", &[]).is_err());
}
#[test]
fn explicit_reads_are_read_only_and_symlinks_cannot_escape() {
    if !kageha_sandbox::available() {
        assert!(ProcessSandbox::default().command("/bin/sh", &[]).is_err());
        return;
    }
    let allowed = tempfile::tempdir().unwrap();
    let read_only = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(read_only.path().join("note"), "readable").unwrap();
    std::fs::write(outside.path().join("secret"), "outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), allowed.path().join("escape")).unwrap();
    let script = format!(
        "cat '{r}/note' > '{w}/copy'; printf bad > '{r}/note'; cat '{w}/escape/secret' > '{w}/leak'; printf bad > '{w}/escape/new'",
        r=read_only.path().display(), w=allowed.path().display(),
    );
    let output = ProcessSandbox {
        read_roots: vec![read_only.path().into()],
        write_roots: vec![allowed.path().into()],
        network: false,
        hidden_roots: vec![],
    }
    .command("/bin/sh", &["-c".into(), script])
    .unwrap()
    .output()
    .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read(allowed.path().join("copy")).unwrap(),
        b"readable"
    );
    assert_eq!(
        std::fs::read(read_only.path().join("note")).unwrap(),
        b"readable"
    );
    assert_eq!(std::fs::read(allowed.path().join("leak")).unwrap(), b"");
    assert!(!outside.path().join("new").exists());
}
#[test]
fn real_process_allows_workspace_and_denies_external_file_and_network() {
    if !kageha_sandbox::available() {
        assert!(ProcessSandbox::default().command("/bin/sh", &[]).is_err());
        return;
    }
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "secret").unwrap();
    let sandbox = ProcessSandbox {
        write_roots: vec![allowed.path().into()],
        ..Default::default()
    };
    let script = format!(
        "printf ok > '{}/ok'; cat '{}/secret' > '{}/leak'; printf bad > '{}/bad'",
        allowed.path().display(),
        outside.path().display(),
        allowed.path().display(),
        outside.path().display()
    );
    let output = sandbox
        .command("/bin/sh", &["-c".into(), script])
        .unwrap()
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(allowed.path().join("ok").exists(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(allowed.path().join("ok")).unwrap(),
        "ok"
    );
    assert!(!outside.path().join("bad").exists());
    assert_eq!(std::fs::read(allowed.path().join("leak")).unwrap(), b"");
    use std::{
        io::{Read, Write},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let server_count = count.clone();
    let server_stop = stop.clone();
    let server = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(5);
        while !server_stop.load(Ordering::Acquire) && Instant::now() < end {
            if let Ok((mut socket, _)) = listener.accept() {
                server_count.fetch_add(1, Ordering::AcqRel);
                socket
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                let mut bytes = [0; 1024];
                let _ = socket.read(&mut bytes);
                let _ = socket.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });
    let args = vec![
        "--ignored".into(),
        "--exact".into(),
        "sandbox_socket_probe".into(),
        "--nocapture".into(),
    ];
    let program = std::env::current_exe().unwrap();
    let addr = format!("127.0.0.1:{port}");
    let denied = sandbox
        .command(program.to_str().unwrap(), &args)
        .unwrap()
        .env("KAGEHA_SOCKET_PROBE", &addr)
        .env("KAGEHA_EXPECT_NETWORK", "denied")
        .output()
        .unwrap();
    assert!(denied.status.success(), "{denied:?}");
    assert_eq!(
        count.load(Ordering::Acquire),
        0,
        "Denied network must not reach the server"
    );
    let permitted = ProcessSandbox {
        network: true,
        ..sandbox
    }
    .command(program.to_str().unwrap(), &args)
    .unwrap()
    .env("KAGEHA_SOCKET_PROBE", &addr)
    .env("KAGEHA_EXPECT_NETWORK", "allowed")
    .output()
    .unwrap();
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    assert!(permitted.status.success(), "{permitted:?}");

    assert_eq!(count.load(Ordering::Acquire), 1);
}

#[test]
#[ignore = "launched twice as an isolated sandbox process by confinement test"]
fn sandbox_socket_probe() {
    use std::io::{Read, Write};
    let address = std::env::var("KAGEHA_SOCKET_PROBE")
        .unwrap()
        .parse()
        .unwrap();
    let connection =
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(1));
    if std::env::var("KAGEHA_EXPECT_NETWORK").unwrap() == "denied" {
        assert!(connection.is_err());
        return;
    }
    let mut stream = connection.unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: fixture\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.ends_with("ok"));
}
#[test]
fn hidden_subtrees_of_a_read_root_are_unreadable_except_nested_write_roots() {
    if !kageha_sandbox::available() {
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("agents/own")).unwrap();
    std::fs::create_dir_all(root.join("agents/other")).unwrap();
    std::fs::write(root.join("brief.md"), "brief").unwrap();
    std::fs::write(root.join("state/token"), "secret").unwrap();
    std::fs::write(root.join("agents/other/notes.md"), "sibling").unwrap();
    let own = root.join("agents/own");
    let script = format!(
        "cat '{r}/brief.md' > '{o}/copy'; cat '{r}/state/token' > '{o}/token'; cat '{r}/agents/other/notes.md' > '{o}/sibling'; printf bad > '{r}/bad'",
        r = root.display(),
        o = own.display(),
    );
    let output = ProcessSandbox {
        read_roots: vec![root.into()],
        write_roots: vec![own.clone()],
        hidden_roots: vec![root.join("state"), root.join("agents")],
        network: false,
    }
    .command("/bin/sh", &["-c".into(), script])
    .unwrap()
    .output()
    .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(std::fs::read(own.join("copy")).unwrap(), b"brief");
    assert_eq!(std::fs::read(own.join("token")).unwrap(), b"");
    assert_eq!(std::fs::read(own.join("sibling")).unwrap(), b"");
    assert!(!root.join("bad").exists());
}
#[test]
fn device_files_process_substitution_and_ancestor_metadata_preserve_confinement() {
    if !kageha_sandbox::available() {
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let tmp = workspace.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(root.join("sibling"), "sibling").unwrap();
    std::fs::write(workspace.join("file"), "file").unwrap();
    let bash = ["/bin/bash", "/usr/bin/bash"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("bash");
    let run = |script: &str| {
        ProcessSandbox {
            write_roots: vec![workspace.clone()],
            ..Default::default()
        }
        .command(bash, &["-c".into(), script.into()])
        .unwrap()
        .current_dir(&workspace)
        .env("TMPDIR", &tmp)
        .output()
        .unwrap()
    };
    let output = run(concat!(
        "set -e; cat <<EOF\nheredoc\nEOF\n",
        "diff <(echo a) <(echo a) && echo same; ",
        "echo x > /dev/null; head -c 1 /dev/urandom > /dev/null; head -c 1 /dev/zero | wc -c | tr -d ' '; ",
        "ls .; ls -ld .. > /dev/null; ls .. > /dev/null"
    ));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "heredoc\nsame\n1\nfile\ntmp\n"
    );
    let read = run("cat ../sibling");
    assert!(!read.status.success(), "{read:?}");
    assert!(read.stdout.is_empty(), "{read:?}");

    let write = run("printf x > ../new");
    #[cfg(target_os = "macos")]
    assert!(!write.status.success(), "{write:?}");
    // Linux can write to the private tmpfs. It must disappear with the sandbox.
    #[cfg(target_os = "linux")]
    if write.status.success() {
        let fresh = run("test ! -e ../new");
        assert!(fresh.status.success(), "{fresh:?}");
    }
    assert!(!root.join("new").exists());
}
