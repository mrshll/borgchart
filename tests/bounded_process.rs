use borgchart::bounded_process::{self, Error, Limits, Stream};
use std::process::Command;
use std::time::{Duration, Instant};

fn limits() -> Limits {
    Limits {
        deadline: Instant::now() + Duration::from_secs(3),
        stdout_bytes: 4096,
        stderr_bytes: 4096,
    }
}

#[test]
fn exact_argv_and_nonzero_status_are_preserved() {
    let literal = "$(false); $TOKEN * 'quoted'";
    let output = bounded_process::run(
        Command::new("/usr/bin/printf").args(["%s", literal]),
        None,
        limits(),
    )
    .unwrap();
    assert_eq!(output.stdout, literal.as_bytes());
    assert!(output.status.success());
    let output = bounded_process::run(
        Command::new("/bin/sh").args(["-c", "printf failed >&2; exit 7"]),
        None,
        limits(),
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stderr, b"failed");
}

#[test]
fn input_is_closed_and_output_pipes_are_serviced_while_writing() {
    let input = vec![b'x'; 512 * 1024];
    let mut cap = limits();
    cap.stdout_bytes = input.len() + 100_000;
    cap.stderr_bytes = 100_000;
    let output = bounded_process::run(Command::new("/bin/sh").args(["-c", "dd if=/dev/zero bs=1000 count=80 2>/dev/null; dd if=/dev/zero bs=1000 count=80 1>&2 2>/dev/null; cat"]), Some(&input), cap).unwrap();
    assert!(output.status.success());
    assert_eq!(&output.stdout[80_000..], input.as_slice());
    assert_eq!(output.stderr.len(), 80_000);
}

#[test]
fn stdout_and_stderr_have_independent_caps() {
    for (script, stream) in [
        ("printf 12345", Stream::Stdout),
        ("printf 12345 >&2", Stream::Stderr),
    ] {
        let mut cap = limits();
        cap.stdout_bytes = 4;
        cap.stderr_bytes = 4;
        assert_eq!(
            bounded_process::run(Command::new("/bin/sh").args(["-c", script]), None, cap)
                .unwrap_err(),
            Error::OutputLimit(stream)
        );
    }
    let mut cap = limits();
    cap.stdout_bytes = 0;
    cap.stderr_bytes = 0;
    assert!(
        bounded_process::run(&mut Command::new("/usr/bin/true"), None, cap)
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn expired_deadline_never_spawns_and_spawn_errors_are_sanitized() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let mut cap = limits();
    cap.deadline = Instant::now();
    assert_eq!(
        bounded_process::run(Command::new("/usr/bin/touch").arg(&marker), None, cap).unwrap_err(),
        Error::Deadline
    );
    assert!(!marker.exists());
    assert_eq!(
        bounded_process::run(
            &mut Command::new("/nonexistent/secret-bearing-path"),
            None,
            limits()
        )
        .unwrap_err(),
        Error::Spawn(std::io::ErrorKind::NotFound)
    );
}

#[test]
fn descendants_holding_pipes_do_not_extend_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("survived");
    let start = Instant::now();
    let mut cap = limits();
    cap.deadline = start + Duration::from_millis(200);
    let result = bounded_process::run(
        Command::new("/bin/sh")
            .args([
                "-c",
                "trap '' TERM; (sleep 1; printf leaked > \"$1\") & exit 0",
                "fixture",
            ])
            .arg(&marker),
        None,
        cap,
    );
    assert_eq!(result.unwrap_err(), Error::Deadline);
    assert!(start.elapsed() < Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(1100));
    assert!(!marker.exists(), "descendant escaped process-group cleanup");
}

#[test]
fn capture_deadline_keeps_partial_output_and_kills_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let mut cap = limits();
    cap.deadline = start + Duration::from_secs(1);
    let captured = bounded_process::capture(
        Command::new("/bin/sh")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir(dir.path())
            .args([
                "-c",
                "sleep 30 & printf '%s\\n' \"$$\" \"$!\" > pids; printf partial; printf error >&2; wait",
            ]),
        cap,
    )
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    let pids = std::fs::read_to_string(dir.path().join("pids")).unwrap();
    assert_eq!(pids.lines().count(), 2);
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let alive: Vec<_> = pids
            .lines()
            .filter(|pid| {
                Command::new("/bin/kill")
                    .env_clear()
                    .args(["-0", pid])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            })
            .collect();
        if alive.is_empty() {
            break;
        }
        if Instant::now() >= until {
            for pid in &alive {
                let _ = Command::new("/bin/kill")
                    .env_clear()
                    .args(["-KILL", pid])
                    .status();
            }
            panic!("capture deadline left processes running: {alive:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(captured.status.is_none());
    assert_eq!(captured.stdout, b"partial");
    assert_eq!(captured.stderr, b"error");
}
