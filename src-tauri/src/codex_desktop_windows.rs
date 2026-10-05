pub(crate) fn is_codex_desktop_image(path: &str, local_app_data: Option<&str>) -> bool {
    let path = normalize_path(path);
    if path.contains("\\windowsapps\\openai.codex_") && path.ends_with("\\app\\codex.exe") {
        return true;
    }
    local_app_data.is_some_and(|base| {
        path == format!(
            "{}\\programs\\codex\\codex.exe",
            normalize_path(base).trim_end_matches('\\')
        )
    })
}

pub(crate) fn main_process_id(processes: &[(u32, u32)]) -> Option<u32> {
    processes
        .iter()
        .find(|(_, parent)| !processes.iter().any(|(pid, _)| pid == parent))
        .map(|(pid, _)| *pid)
}

fn normalize_path(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{is_codex_desktop_image, main_process_id};

    const LOCAL_APP_DATA: Option<&str> = Some(r"C:\Users\张三\AppData\Local");

    #[test]
    fn store_package_main_executable_is_codex_desktop() {
        assert!(is_codex_desktop_image(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.623.13972.0_x64__2p2nqsd0c76g0\app\Codex.exe",
            LOCAL_APP_DATA,
        ));
    }

    #[test]
    fn legacy_user_install_is_codex_desktop() {
        assert!(is_codex_desktop_image(
            r"C:\Users\张三\AppData\Local\Programs\Codex\Codex.exe",
            LOCAL_APP_DATA,
        ));
    }

    #[test]
    fn bundled_and_standalone_cli_are_not_codex_desktop() {
        assert!(!is_codex_desktop_image(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.623.13972.0_x64__2p2nqsd0c76g0\app\resources\codex.exe",
            LOCAL_APP_DATA,
        ));
        assert!(!is_codex_desktop_image(
            r"C:\Users\张三\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.exe",
            LOCAL_APP_DATA,
        ));
        assert!(!is_codex_desktop_image(
            r"C:\Users\张三\AppData\Local\Programs\Codex\Codex.exe",
            None,
        ));
    }

    #[test]
    fn electron_helpers_resolve_to_their_main_process() {
        let processes = [(12, 10), (10, 4), (13, 10)];

        assert_eq!(main_process_id(&processes), Some(10));
    }

    #[test]
    fn no_desktop_process_has_no_main_process() {
        assert_eq!(main_process_id(&[]), None);
    }
}
