use kageha_contracts::execution::Control;
use kageha_workspace::process::run;
use std::{
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[test]
fn concurrent_stdin_stdout_does_not_deadlock() {
    let input = vec![b'a'; 512 * 1024];
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "/usr/bin/head -c 32768 /dev/zero; /bin/cat"]);
    let output = run(
        &mut command,
        input,
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: Some(Instant::now() + Duration::from_secs(2)),
        },
        1024 * 1024,
    )
    .unwrap();
    assert!(output.success);
    assert_eq!(output.bytes.len(), 32768 + 512 * 1024);
}
#[test]
fn deadline_cancellation_and_output_limit_kill_the_group() {
    let start = Instant::now();
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    assert_eq!(
        run(
            &mut command,
            vec![],
            &Control {
                cancelled: &AtomicBool::new(false),
                deadline: Some(Instant::now() + Duration::from_millis(50))
            },
            1024
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::TimedOut
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    let cancel = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(30));
            cancel.store(true, Ordering::Release);
        });
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        assert_eq!(
            run(
                &mut command,
                vec![],
                &Control {
                    cancelled: &cancel,
                    deadline: Some(Instant::now() + Duration::from_secs(2))
                },
                1024
            )
            .unwrap_err()
            .kind(),
            std::io::ErrorKind::Interrupted
        );
    });
    assert!(run(
        &mut Command::new("/usr/bin/yes"),
        vec![],
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: Some(Instant::now() + Duration::from_secs(2))
        },
        1024
    )
    .is_err());
}
#[test]
fn leader_exit_cannot_leave_descendants_holding_pipes() {
    let start = Instant::now();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "/bin/sleep 30 & echo descendant-proof"]);
    let output = run(
        &mut command,
        vec![],
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: Some(Instant::now() + Duration::from_secs(2)),
        },
        1024,
    )
    .unwrap();
    assert!(output.success);
    assert!(String::from_utf8(output.bytes)
        .unwrap()
        .contains("descendant-proof"));
    assert!(start.elapsed() < Duration::from_secs(1));
}
#[test]
fn early_exit_and_nonzero_status_are_explicit() {
    let control = Control {
        cancelled: &AtomicBool::new(false),
        deadline: Some(Instant::now() + Duration::from_secs(2)),
    };
    assert!(
        run(
            &mut Command::new("/usr/bin/true"),
            vec![b'a'; 512 * 1024],
            &control,
            1024
        )
        .unwrap()
        .success
    );
    assert!(
        !run(&mut Command::new("/usr/bin/false"), vec![], &control, 1024)
            .unwrap()
            .success
    );
}
