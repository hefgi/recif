//! Minimal TTY detection without pulling an extra crate.

/// Return `true` if stdin is a terminal (interactive).
pub fn stdin_is_tty() -> bool {
    is_tty(0)
}

/// Return `true` if stdout is a terminal.
pub fn stdout_is_tty() -> bool {
    is_tty(1)
}

#[cfg(unix)]
fn is_tty(fd: i32) -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    // SAFETY: `isatty` is a read-only libc call on a raw fd; no memory is touched.
    unsafe { isatty(fd) == 1 }
}

#[cfg(not(unix))]
fn is_tty(_fd: i32) -> bool {
    false
}
