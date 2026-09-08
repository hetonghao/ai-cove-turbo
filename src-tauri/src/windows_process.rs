use std::{
    ffi::{OsStr, c_void},
    mem::size_of,
    os::windows::process::CommandExt,
    process::Command,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const MAX_PATH: usize = 260;

#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; MAX_PATH],
}

struct Snapshot(*mut c_void);

impl Drop for Snapshot {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a process snapshot from `CreateToolhelp32Snapshot`.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

unsafe extern "system" {
    fn CreateToolhelp32Snapshot(dw_flags: u32, th32_process_id: u32) -> *mut c_void;
    fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

pub(crate) fn process_id_by_name(name: &str) -> Option<u32> {
    // SAFETY: `CreateToolhelp32Snapshot` is the documented process-list entry point.
    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if is_invalid_handle(handle) {
        return None;
    }
    let snapshot = Snapshot(handle);
    let mut entry = ProcessEntry32W {
        dw_size: u32::try_from(size_of::<ProcessEntry32W>()).ok()?,
        cnt_usage: 0,
        th32_process_id: 0,
        th32_default_heap_id: 0,
        th32_module_id: 0,
        cnt_threads: 0,
        th32_parent_process_id: 0,
        pc_pri_class_base: 0,
        dw_flags: 0,
        sz_exe_file: [0; MAX_PATH],
    };
    // SAFETY: `entry` is a correctly sized `PROCESSENTRY32W` for this snapshot.
    if unsafe { Process32FirstW(snapshot.0, &raw mut entry) } == 0 {
        return None;
    }
    loop {
        if process_name_matches(&exe_file_name(&entry.sz_exe_file), name) {
            return Some(entry.th32_process_id);
        }
        // SAFETY: `entry` remains the snapshot's iteration buffer until the handle is closed.
        if unsafe { Process32NextW(snapshot.0, &raw mut entry) } == 0 {
            return None;
        }
    }
}

fn is_invalid_handle(handle: *mut c_void) -> bool {
    handle.is_null() || handle == (-1isize as *mut c_void)
}

fn exe_file_name(name: &[u16; MAX_PATH]) -> String {
    String::from_utf16_lossy(name.split(|&unit| unit == 0).next().unwrap_or(&[]))
}

fn process_name_matches(exe_file: &str, wanted: &str) -> bool {
    let file_name = exe_file.rsplit(['\\', '/']).next().unwrap_or(exe_file);
    let wanted = wanted.strip_suffix(".exe").unwrap_or(wanted);
    file_name.eq_ignore_ascii_case(wanted)
        || file_name.eq_ignore_ascii_case(&format!("{wanted}.exe"))
}

#[cfg(test)]
mod tests {
    use super::{hidden_command, process_name_matches};

    #[test]
    fn child_process_runs_without_a_console_window() {
        // Given a probe that reports the attached console window handle.
        let script = r#"Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class ConsoleWindow { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'; [ConsoleWindow]::GetConsoleWindow().ToInt64()"#;

        // When the probe is launched through Turbo's Windows command seam.
        let output = hidden_command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
            .expect("PowerShell console probe should run");

        // Then Windows must not attach a visible console to the child process.
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "0");
    }

    #[test]
    fn process_name_matches_codex_image() {
        assert!(process_name_matches("Codex.exe", "Codex"));
        assert!(process_name_matches(
            r"C:\Program Files\Codex\Codex.exe",
            "Codex"
        ));
        assert!(!process_name_matches("powershell.exe", "Codex"));
    }
}
