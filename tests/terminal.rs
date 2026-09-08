//! The secret prompt runs against a real terminal, so it is tested against one:
//! a pty, where echo and input flushing behave as they do for a person installing.
use std::io::{Read, Write};

/// A pty pair. The master stands in for the person typing and watching.
struct Pty {
    master: std::fs::File,
    slave: std::fs::File,
}
fn pty() -> Pty {
    use std::os::fd::FromRawFd;
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty writes two descriptors into the locals; the null pointers are
    // the documented way to accept the default termios and window size.
    let created = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(created, 0, "could not allocate a pty");
    // SAFETY: both descriptors are freshly created and owned by these files.
    unsafe {
        Pty {
            master: std::fs::File::from_raw_fd(master),
            slave: std::fs::File::from_raw_fd(slave),
        }
    }
}
/// Read whatever the terminal has produced so far, without blocking forever.
fn drain(master: &mut std::fs::File) -> String {
    use std::os::fd::AsRawFd;
    // SAFETY: sets O_NONBLOCK on a descriptor owned by the caller's file.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut seen = String::new();
    let mut buffer = [0u8; 4096];
    while let Ok(n) = master.read(&mut buffer) {
        if n == 0 {
            break;
        }
        seen.push_str(&String::from_utf8_lossy(&buffer[..n]));
    }
    seen
}

#[test]
fn a_pasted_key_is_read_whole_and_never_echoed() {
    let pty = pty();
    let key = "test-only-fake-api-key-000000000000000000000000000000000000000000000000000";
    // Stands in for the person: wait for the prompt, then paste the key and the
    // answer to the question after it in one go. Flushing pending input at that
    // point is what truncated the key to a single character.
    let mut master = pty.master;
    let typist = std::thread::spawn(move || {
        let mut seen = String::new();
        let mut buffer = [0u8; 256];
        while !seen.contains("KEY (input hidden):") {
            let n = master.read(&mut buffer).unwrap();
            seen.push_str(&String::from_utf8_lossy(&buffer[..n]));
        }
        write!(master, "{key}\ny\n").unwrap();
        master.flush().unwrap();
        master
    });

    // Prompting runs on its own thread: discarding the input leaves the read
    // blocked forever, and a bounded wait reports that instead of hanging CI.
    let slave = pty.slave.try_clone().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut terminal = horde::provisioning::Terminal::on(slave).unwrap();
        let read = terminal.secret("KEY (input hidden): ").unwrap();
        // Echo is restored afterwards, and the answer sent with the key still arrives.
        let again = terminal.confirm("Another?", false).unwrap();
        let _ = sender.send((read, again));
    });
    let (read, again) = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the prompt never returned: input was discarded");
    assert_eq!(read, key, "the key was not read whole");
    assert!(again, "input sent alongside the secret was discarded");

    let mut master = typist.join().unwrap();
    let shown = drain(&mut master);
    assert!(
        !shown.contains(key),
        "the key was echoed to the terminal: {shown:?}"
    );
}
