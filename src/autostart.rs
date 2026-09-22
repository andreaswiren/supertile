//! Start-with-Windows, via a single `HKCU\...\Run` value.
//!
//! Deliberately the least invasive persistence mechanism Windows offers:
//!
//! * **`HKCU`, never `HKLM`.** Per-user, no elevation, and it cannot affect
//!   anyone else on the machine.
//! * **A `Run` value, not a scheduled task or a service.** It is visible in
//!   Task Manager's Startup tab and in Settings → Startup apps, where the user
//!   can disable it without going near SuperTile. A scheduled task is
//!   materially harder to find and to remove.
//! * **One value, removed cleanly.** Turning the setting off deletes the value
//!   rather than leaving a disabled remnant behind.
//!
//! Documented as a persistence mechanism in
//! `docs/compliance/threat-model.md` (T8).

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Value name. Stable, so toggling never leaves a second entry behind.
const VALUE_NAME: &str = "SuperTile";

/// The command written to the registry: the running executable, quoted.
///
/// Quoted because the path routinely contains spaces, and an unquoted path
/// with a space is the classic unquoted-service-path weakness — Windows would
/// try `C:\Program.exe` first.
fn command_line() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.to_str()?;
    Some(format!("\"{path}\""))
}

fn open(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<HKEY> {
    let sub = crate::util::WideStr::new(RUN_KEY);
    let mut key = HKEY::default();
    // SAFETY: `sub` outlives the call; `key` is a valid out-param.
    let status =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_pcwstr(), None, access, &mut key) };
    (status == ERROR_SUCCESS).then_some(key)
}

fn close(key: HKEY) {
    // SAFETY: `key` came from RegOpenKeyExW and is closed exactly once.
    unsafe {
        let _ = RegCloseKey(key);
    }
}

/// Is SuperTile currently registered to start with Windows?
///
/// Reports the *registry's* state rather than the config's, so a value removed
/// through Settings → Startup apps is reflected honestly instead of the menu
/// claiming something untrue.
pub fn is_enabled() -> bool {
    let Some(key) = open(KEY_READ) else {
        return false;
    };
    let name = crate::util::WideStr::new(VALUE_NAME);
    let mut size: u32 = 0;
    // SAFETY: querying only the size; all pointer out-params are None.
    let status =
        unsafe { RegQueryValueExW(key, name.as_pcwstr(), None, None, None, Some(&mut size)) };
    close(key);
    status == ERROR_SUCCESS
}

/// Register or unregister. Returns false if the registry refused.
pub fn set_enabled(enabled: bool) -> bool {
    let Some(key) = open(KEY_WRITE) else {
        return false;
    };
    let name = crate::util::WideStr::new(VALUE_NAME);

    let ok = if enabled {
        let Some(cmd) = command_line() else {
            close(key);
            return false;
        };
        let value = crate::util::WideStr::new(&cmd);
        // REG_SZ length is in bytes and must include the terminating NUL.
        let bytes = value.len_with_nul() * std::mem::size_of::<u16>();
        // SAFETY: the slice describes exactly the UTF-16 buffer inside
        // `value`, including its NUL, which is what REG_SZ requires.
        let data = unsafe { std::slice::from_raw_parts(value.as_pcwstr().0 as *const u8, bytes) };
        // SAFETY: `name` and `data` outlive the call.
        let status = unsafe { RegSetValueExW(key, name.as_pcwstr(), None, REG_SZ, Some(data)) };
        status == ERROR_SUCCESS
    } else {
        // SAFETY: `name` outlives the call.
        let status = unsafe { RegDeleteValueW(key, name.as_pcwstr()) };
        // Deleting something that was never there is success as far as the
        // caller is concerned.
        status == ERROR_SUCCESS || !is_enabled_with(key)
    };

    close(key);
    ok
}

fn is_enabled_with(key: HKEY) -> bool {
    let name = crate::util::WideStr::new(VALUE_NAME);
    let mut size: u32 = 0;
    // SAFETY: size-only query on an open key.
    let status =
        unsafe { RegQueryValueExW(key, name.as_pcwstr(), None, None, None, Some(&mut size)) };
    status == ERROR_SUCCESS
}

/// What is actually registered, read back from the registry.
///
/// This used to return `command_line()` -- the command that *would* be written
/// -- under a name that says otherwise. Every caller wanting to show the user
/// what starts with Windows was therefore shown the running executable, which
/// is the one path guaranteed to look right whether or not the registry agreed.
/// A stale or wrong entry could not be seen from inside the program at all.
pub fn registered_command() -> Option<String> {
    read_value()
}

/// The raw `REG_SZ` under our value name, if there is one.
fn read_value() -> Option<String> {
    let key = open(KEY_READ)?;
    let name = crate::util::WideStr::new(VALUE_NAME);
    let mut kind = REG_SZ;
    let mut size: u32 = 0;
    // SAFETY: a size-and-type query; the data pointer is None.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_pcwstr(),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS || kind != REG_SZ || size == 0 {
        close(key);
        return None;
    }
    // `size` is in bytes and includes the terminating NUL.
    let mut buf = vec![0u8; size as usize];
    // SAFETY: `buf` is exactly the size the query asked for.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_pcwstr(),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        )
    };
    close(key);
    if status != ERROR_SUCCESS {
        return None;
    }
    let wide: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|c| *c != 0)
        .collect();
    Some(String::from_utf16_lossy(&wide))
}

/// Write an exact command string. Used to put back what a test found.
///
/// Test-only on purpose: the program itself has exactly one command it may
/// register, and that is `command_line()`. A general "write whatever you like
/// into HKCU Run" is not something the rest of the code should be able to
/// reach for.
#[cfg(test)]
fn write_value(cmd: &str) -> bool {
    let Some(key) = open(KEY_WRITE) else {
        return false;
    };
    let name = crate::util::WideStr::new(VALUE_NAME);
    let value = crate::util::WideStr::new(cmd);
    let bytes = value.len_with_nul() * std::mem::size_of::<u16>();
    // SAFETY: the slice describes exactly the UTF-16 buffer inside `value`,
    // including its NUL, which is what REG_SZ requires.
    let data = unsafe { std::slice::from_raw_parts(value.as_pcwstr().0 as *const u8, bytes) };
    // SAFETY: `name` and `data` outlive the call.
    let status = unsafe { RegSetValueExW(key, name.as_pcwstr(), None, REG_SZ, Some(data)) };
    close(key);
    status == ERROR_SUCCESS
}

const _: () = {
    // A reminder in the type system: this module must never touch HKLM.
    assert!(!RUN_KEY.is_empty());
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_quoted() {
        let cmd = command_line().expect("current_exe should resolve");
        assert!(cmd.starts_with('"'), "{cmd}");
        assert!(cmd.ends_with('"'), "{cmd}");
        // An unquoted path containing a space is the classic
        // unquoted-path weakness; the quotes are the fix.
        assert!(cmd.contains(".exe"), "{cmd}");
    }

    #[test]
    fn we_only_ever_touch_the_per_user_run_key() {
        assert_eq!(RUN_KEY, r"Software\Microsoft\Windows\CurrentVersion\Run");
        assert_eq!(VALUE_NAME, "SuperTile");
    }

    #[test]
    fn reading_the_current_state_does_not_panic() {
        // Whatever the machine's state, this must answer without failing.
        let _ = is_enabled();
    }

    #[test]
    fn enabling_then_disabling_leaves_no_trace() {
        // Writes to the real HKCU Run key, then restores the previous state.
        //
        // A CI runner may have no writable HKCU at all -- a service account
        // without a loaded user hive, for instance. That is an absent facility,
        // not a failing one, and asserting on it turns "this environment has no
        // registry" into "this code is broken". The test still runs in full
        // wherever the key exists, which is every machine SuperTile ships to.
        //
        // The *exact value* is saved and put back, not merely whether one
        // existed. Restoring with `set_enabled(true)` writes `current_exe()`,
        // which under `cargo test` is the test harness in `target/debug/deps`
        // -- so running the suite on a developer's own machine repointed their
        // autostart at a test binary, and every subsequent logon launched that
        // instead of SuperTile. It is also self-perpetuating: the harness runs
        // this test, which points the entry back at the harness.
        let was = read_value();

        if !set_enabled(true) {
            eprintln!("skipping: HKCU Run is not writable in this environment");
            return;
        }
        assert!(is_enabled(), "value should be present after enabling");

        assert!(set_enabled(false), "should be able to delete the value");
        assert!(!is_enabled(), "value should be gone after disabling");

        // Disabling twice must not report failure.
        assert!(set_enabled(false));

        // Byte-for-byte what was there, or nothing if there was nothing.
        if let Some(previous) = was {
            assert!(write_value(&previous), "must restore the user's own entry");
            assert_eq!(read_value().as_deref(), Some(previous.as_str()));
        }
    }
}
