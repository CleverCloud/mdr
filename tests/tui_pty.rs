//! The terminal backend, driven through a real pseudo-terminal.
//!
//! `cargo test` gives a process no terminal, which is why the rest of the suite
//! stops at the structures the backend produces. Here the test makes its own:
//! `forkpty` gives the child a terminal of its own, so the backend can be
//! started, sent a key, and watched for a clean exit — the one path a unit test
//! cannot reach.
//!
//! Unix only, and only when the terminal backend is compiled in. Windows has no
//! `forkpty`, and its console model is different enough that the same test would
//! be proving something else.
#![cfg(all(unix, feature = "tui-backend"))]

use std::ffi::CString;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// Everything the child needs, built before the fork.
///
/// After `fork` in a multi-threaded process — and a test binary is one — only
/// async-signal-safe calls are sound: another thread may have been holding the
/// allocator's lock at the moment of the fork. So the strings, the argument
/// vector and the environment are all prepared here, and the child does nothing
/// but `execve`.
struct ChildPlan {
    exe: CString,
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
    // Keeps the strings the pointers above borrow from alive.
    _argv_storage: Vec<CString>,
    _envp_storage: Vec<CString>,
    doc: Option<Vec<u8>>,
}

fn plan_child(args: &[&str], stdin_doc: Option<&[u8]>, sandbox: &std::path::Path) -> ChildPlan {
    let exe = CString::new(env!("CARGO_BIN_EXE_mdr")).unwrap();

    let argv_storage: Vec<CString> = std::iter::once(exe.clone())
        .chain(args.iter().map(|a| CString::new(*a).unwrap()))
        .collect();
    let mut argv: Vec<*const libc::c_char> = argv_storage.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());

    // A closed environment. Without this the child resolves the *developer's*
    // config: mdr creates one on first run and corrects an old backend name in
    // place, so a test run would write to `~/.config/mdr/config.kdl`.
    let sandbox = sandbox.to_str().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let envp_storage: Vec<CString> = [
        format!("PATH={path}"),
        format!("HOME={sandbox}"),
        format!("USERPROFILE={sandbox}"),
        format!("XDG_CONFIG_HOME={sandbox}/xdg"),
        format!("APPDATA={sandbox}/appdata"),
        format!("TMPDIR={sandbox}/tmp"),
        format!("TMP={sandbox}/tmp"),
        format!("TEMP={sandbox}/tmp"),
        "TERM=xterm-256color".to_string(),
    ]
    .into_iter()
    .map(|v| CString::new(v).unwrap())
    .collect();
    let mut envp: Vec<*const libc::c_char> = envp_storage.iter().map(|v| v.as_ptr()).collect();
    envp.push(std::ptr::null());

    ChildPlan {
        exe,
        argv,
        envp,
        _argv_storage: argv_storage,
        _envp_storage: envp_storage,
        doc: stdin_doc.map(<[u8]>::to_vec),
    }
}

/// What a run through the pty produced.
struct PtyRun {
    output: String,
    /// `Some(code)` only when the child exited normally; a signal leaves `None`
    /// rather than being reported as a status.
    exit_code: Option<i32>,
}

/// Start mdr under a pseudo-terminal, optionally with a document piped to it,
/// then send `keys` and wait for it to leave.
fn run_under_pty(args: &[&str], stdin_doc: Option<&[u8]>, keys: &[u8]) -> PtyRun {
    // One sandbox per call, not per process: the tests in this binary run in
    // parallel, so a name built from the pid would be the *same* directory for
    // both — and each would delete the other's.
    let sandbox = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(sandbox.path().join("tmp")).unwrap();

    let plan = plan_child(args, stdin_doc, sandbox.path());

    let mut master: RawFd = -1;
    let mut winsize = libc::winsize {
        ws_row: 24,
        ws_col: 90,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // SAFETY: `master` and `winsize` outlive the call. The child branch below
    // only calls async-signal-safe functions on data prepared before the fork.
    let pid = unsafe {
        libc::forkpty(
            &raw mut master,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut winsize,
        )
    };
    assert!(
        pid >= 0,
        "forkpty failed: {}",
        std::io::Error::last_os_error()
    );

    if pid == 0 {
        // SAFETY: every pointer comes from `plan`, which was built before the
        // fork and is still mapped. Nothing here allocates.
        unsafe {
            if let Some(doc) = plan.doc.as_ref() {
                // A half-built stdin would run a different case than the one
                // the test means to exercise, so any failure ends the child
                // with a status the parent cannot mistake for success.
                let mut fds = [0_i32; 2];
                if libc::pipe(fds.as_mut_ptr()) != 0 {
                    libc::_exit(126);
                }
                let doc_len = isize::try_from(doc.len()).unwrap_or(isize::MAX);
                if libc::write(fds[1], doc.as_ptr().cast(), doc.len()) != doc_len {
                    libc::_exit(126);
                }
                libc::close(fds[1]);
                if libc::dup2(fds[0], libc::STDIN_FILENO) < 0 {
                    libc::_exit(126);
                }
                libc::close(fds[0]);
            }
            libc::execve(plan.exe.as_ptr(), plan.argv.as_ptr(), plan.envp.as_ptr());
            libc::_exit(127);
        }
    }

    // Parent. The master is made non-blocking so a child that stops writing
    // cannot park the loop past its deadline — which only holds if this
    // actually took, so it is checked rather than assumed.
    // SAFETY: `master` is the descriptor `forkpty` just returned.
    unsafe {
        let flags = libc::fcntl(master, libc::F_GETFL);
        assert!(flags >= 0, "F_GETFL: {}", std::io::Error::last_os_error());
        let set = libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
        assert!(set >= 0, "F_SETFL: {}", std::io::Error::last_os_error());
    }

    let mut output = Vec::new();
    let started = Instant::now();
    let mut sent = false;
    let mut exit_code = None;
    let mut exited = false;

    while started.elapsed() < Duration::from_secs(20) {
        // Wait for the document itself rather than for a duration: the key
        // has to arrive after the first frame, and a fixed delay only assumes
        // that. The deadline below still bounds the whole run.
        if !sent && output_contains(&output, b"Titre") {
            // SAFETY: writing `keys`, whose length is passed, to the master.
            unsafe { libc::write(master, keys.as_ptr().cast(), keys.len()) };
            sent = true;
        }

        let mut buf = [0_u8; 8192];
        // SAFETY: `buf` is owned here and its real length is passed.
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            output.extend_from_slice(&buf[..usize::try_from(n).unwrap()]);
        }

        let mut status = 0;
        // SAFETY: `status` outlives the call; `WNOHANG` keeps it from blocking.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        if waited == pid {
            exited = true;
            // Whatever the child wrote on its way out — the cleanup sequence
            // among it — may still be sitting in the pty buffer.
            loop {
                let mut tail = [0_u8; 8192];
                // SAFETY: `tail` is owned here and its real length is passed.
                let n = unsafe { libc::read(master, tail.as_mut_ptr().cast(), tail.len()) };
                if n <= 0 {
                    break;
                }
                output.extend_from_slice(&tail[..usize::try_from(n).unwrap()]);
            }
            // A status is only an exit code when the child actually exited; a
            // child killed by a signal must not come back as 0.
            if libc::WIFEXITED(status) {
                exit_code = Some(libc::WEXITSTATUS(status));
            }
            break;
        }
        if n <= 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    if !exited {
        // SAFETY: `pid` is this test's child and has not been reaped.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
    // SAFETY: `master` is owned here and not used again.
    unsafe { libc::close(master) };
    drop(sandbox);

    PtyRun {
        output: String::from_utf8_lossy(&output).into_owned(),
        exit_code,
    }
}

/// Whether `needle` has shown up in what the child has written so far.
fn output_contains(output: &[u8], needle: &[u8]) -> bool {
    output.windows(needle.len()).any(|w| w == needle)
}

const DOC: &[u8] = b"# Titre\n\nDu texte, et une deuxieme ligne.\n";

#[test]
fn the_terminal_backend_starts_and_quits_on_a_key() {
    // The baseline: the document is an argument, so stdin is already the
    // terminal. It is what tells a harness problem apart from a backend one —
    // if both tests fail, suspect the harness; if only the piped one does, the
    // keyboard path is the difference.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("doc.md");
    std::fs::write(&file, DOC).unwrap();

    let run = run_under_pty(&["--backend", "tui", file.to_str().unwrap()], None, b"q");

    assert!(
        run.output.contains("Titre"),
        "the document should have been drawn, got:\n{}",
        run.output
    );
    assert_eq!(
        run.exit_code,
        Some(0),
        "'q' should quit cleanly, got:\n{}",
        run.output
    );
}

#[test]
fn a_piped_document_still_reads_the_keyboard() {
    // `cat doc.md | mdr --backend tui`. The document occupies stdin, so the keys
    // have to come from the terminal — which is what used to fail: the document
    // was drawn, then the first key press killed the process with "Failed to
    // initialize input reader" and left the shell on the alternate screen.
    let run = run_under_pty(&["--backend", "tui"], Some(DOC), b"q");

    assert!(
        run.output.contains("Titre"),
        "the piped document should have been drawn, got:\n{}",
        run.output
    );
    assert!(
        !run.output.contains("Failed to initialize input reader"),
        "the keyboard should have been available, got:\n{}",
        run.output
    );
    assert_eq!(
        run.exit_code,
        Some(0),
        "'q' should quit cleanly with the document piped in, got:\n{}",
        run.output
    );
    assert!(
        run.output.contains("\u{1b}[?1049l"),
        "the alternate screen should have been left behind, got:\n{}",
        run.output.escape_debug()
    );
}
