use std::{os::windows::process::CommandExt, process::Command};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) fn hidden_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(test)]
mod tests {
    use super::hidden_command;

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
}
