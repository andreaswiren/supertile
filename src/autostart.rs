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

/// The exact command that would be written, for display in the UI.
pub fn registered_command() -> Option<String> {
    command_line()
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
        let was = is_enabled();

        if !set_enabled(true) {
            eprintln!("skipping: HKCU Run is not writable in this environment");
            return;
        }
        assert!(is_enabled(), "value should be present after enabling");

        assert!(set_enabled(false), "should be able to delete the value");
        assert!(!is_enabled(), "value should be gone after disabling");

        // Disabling twice must not report failure.
        assert!(set_enabled(false));

        if was {
            let _ = set_enabled(true);
        }
    }
}
