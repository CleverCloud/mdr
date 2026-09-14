//! The terminal backend, driven through a real pseudo-terminal.
//!
//! `cargo test` gives a process no terminal, which is why the rest of the suite
//! stops at the structures the backend produces. Here the test makes its own:
//! `forkpty` gives the child a terminal of its own, so the backend can be
//! started, sent a key, and watched for a clean exit — the one path that a unit
//! test cannot reach.
//!
//! Unix only. Windows has no `forkpty`, and its console model is different
//! enough that the same test would prove something else.
#![cfg(unix)]

use std::io::Write;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

fn mdr_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // the test binary
    path.pop(); // "deps"
    path.push("mdr");
    path
}

/// What a run through the pty produced.
struct PtyRun {
    output: String,
    exit_code: Option<i32>,
}

/// Start `mdr` under a pseudo-terminal, optionally with `stdin_doc` piped to it,
/// then send `keys` and wait for the process to leave.
///
/// `piped` is the case that matters: the document arrives on stdin, so the
/// keyboard has to come from the terminal instead.
fn run_under_pty(args: &[&str], stdin_doc: Option<&[u8]>, keys: &[u8]) -> PtyRun {
    let mut master: RawFd = -1;
    let mut winsize = libc::winsize {
        ws_row: 24,
        ws_col: 90,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // SAFETY: `master` and `winsize` are owned here and outlive the call. In the
    // child (`pid == 0`) only async-signal-safe calls and `execvp` are used.
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
        // Child. Put the document on stdin when asked, then become mdr.
        if let Some(doc) = stdin_doc {
            let mut fds = [0_i32; 2];
            // SAFETY: `fds` is a two-element array, which is what `pipe` fills.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } == 0 {
                // SAFETY: `fds[1]` is the write end this child just created.
                unsafe {
                    libc::write(fds[1], doc.as_ptr().cast(), doc.len());
                    libc::close(fds[1]);
                    libc::dup2(fds[0], libc::STDIN_FILENO);
                }
            }
        }
        let exe = std::ffi::CString::new(mdr_bin().to_str().unwrap()).unwrap();
        let mut argv: Vec<std::ffi::CString> = vec![exe.clone()];
        argv.extend(args.iter().map(|a| std::ffi::CString::new(*a).unwrap()));
        let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: `ptrs` is a NULL-terminated array of NUL-terminated strings
        // that outlives the call; on success it does not return.
        unsafe { libc::execvp(exe.as_ptr(), ptrs.as_ptr()) };
        // SAFETY: the exec failed and this child must not run the test harness.
        unsafe { libc::_exit(127) };
    }

    // Parent.
    let mut file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(master) };
    let mut output = Vec::new();
    let started = Instant::now();
    let mut sent = false;
    let mut exit_code = None;

    while started.elapsed() < Duration::from_secs(20) {
        // The document is drawn first; give it time before pressing anything.
        if !sent && started.elapsed() > Duration::from_secs(3) {
            let _ = file.write_all(keys);
            let _ = file.flush();
            sent = true;
        }

        let mut buf = [0_u8; 8192];
        // SAFETY: `buf` is owned here and its real length is passed.
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            output.extend_from_slice(&buf[..n as usize]);
        }

        let mut status = 0;
        // SAFETY: `status` outlives the call; `WNOHANG` makes it non-blocking.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        if waited == pid {
            exit_code = Some(libc::WEXITSTATUS(status));
            break;
        }
        if n <= 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    if exit_code.is_none() {
        // SAFETY: `pid` is this test's child and has not been reaped.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }

    PtyRun {
        output: String::from_utf8_lossy(&output).into_owned(),
        exit_code,
    }
}

const DOC: &[u8] = b"# Titre\n\nDu texte, et une deuxieme ligne.\n";

#[test]
fn the_terminal_backend_starts_and_quits_on_a_key() {
    // The baseline: the document is an argument, so stdin is already the
    // terminal. If this fails, the harness is wrong rather than the backend.
    let file = std::env::temp_dir().join("mdr_pty_baseline.md");
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

    let _ = std::fs::remove_file(&file);
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
