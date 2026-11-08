//! Process liveness probe used to recover repository locks left behind by a
//! crashed process on the same machine.

use chrono::{DateTime, Utc};

/// What we can tell about the process that wrote a lock file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessState {
    /// The process still runs (and is the one that took the lock).
    Alive,
    /// The process is gone, or its PID now belongs to a newer process.
    Dead,
    /// Cannot tell (access denied, unsupported platform): treat as alive.
    Unknown,
}

/// Probes `pid` on THIS machine. `lock_created_at` detects PID reuse: a
/// process created after the lock was written cannot be its owner.
#[cfg(windows)]
pub(crate) fn process_state(pid: u32, lock_created_at: DateTime<Utc>) -> ProcessState {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: OpenProcess takes plain integers and returns a handle or null;
    // no pointer is dereferenced.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // ERROR_INVALID_PARAMETER = no process with this id. Anything else
        // (typically access denied on another user's elevated process)
        // means it exists but we cannot inspect it.
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(code) if code as u32 == ERROR_INVALID_PARAMETER => ProcessState::Dead,
            _ => ProcessState::Unknown,
        };
    }

    let mut exit_code: u32 = 0;
    // SAFETY: `handle` is a valid process handle opened above, and
    // `exit_code` is a live, writable u32 for the duration of the call.
    let got_exit = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0;

    let empty = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (empty, empty, empty, empty);
    // SAFETY: valid handle; the four FILETIME out-pointers reference live
    // locals that outlive the call.
    let got_times =
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } != 0;

    // SAFETY: `handle` was returned by OpenProcess and is closed exactly once.
    unsafe { CloseHandle(handle) };

    if got_exit && exit_code != STILL_ACTIVE as u32 {
        return ProcessState::Dead;
    }
    if got_times
        && let Some(started) = filetime_to_utc(&created)
        // Small tolerance: the lock is written right after the process starts.
        && started > lock_created_at + chrono::Duration::seconds(2)
    {
        return ProcessState::Dead; // PID reused by a newer process.
    }
    if got_exit {
        ProcessState::Alive
    } else {
        ProcessState::Unknown
    }
}

/// Non-Windows builds cannot probe portably without extra dependencies:
/// stay conservative.
#[cfg(not(windows))]
pub(crate) fn process_state(_pid: u32, _lock_created_at: DateTime<Utc>) -> ProcessState {
    ProcessState::Unknown
}

/// Converts a Windows FILETIME (100 ns ticks since 1601-01-01 UTC).
#[cfg(windows)]
fn filetime_to_utc(ft: &windows_sys::Win32::Foundation::FILETIME) -> Option<DateTime<Utc>> {
    const TICKS_PER_SEC: u64 = 10_000_000;
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600; // 1601-01-01 → 1970-01-01
    let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    let secs = (ticks / TICKS_PER_SEC).checked_sub(EPOCH_DIFF_SECS)?;
    let nanos = ((ticks % TICKS_PER_SEC) * 100) as u32;
    DateTime::from_timestamp(i64::try_from(secs).ok()?, nanos)
}

/// Name of this machine as recorded in lock files.
pub(crate) fn current_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        // A lock taken "now" by this process: the process started earlier.
        let state = process_state(std::process::id(), Utc::now());
        assert_ne!(state, ProcessState::Dead);
    }

    #[cfg(windows)]
    #[test]
    fn exited_process_is_dead() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        drop(child); // releases our handle so the PID really disappears
        assert_eq!(process_state(pid, Utc::now()), ProcessState::Dead);
    }

    #[cfg(windows)]
    #[test]
    fn live_process_started_after_the_lock_counts_as_pid_reuse() {
        // A lock dated long BEFORE this process started cannot be ours:
        // the PID was reused, so the lock owner is gone.
        let state = process_state(
            std::process::id(),
            Utc::now() - chrono::Duration::days(3650),
        );
        assert_eq!(state, ProcessState::Dead);
    }
}
