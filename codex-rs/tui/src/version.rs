use std::sync::OnceLock;

static CODEX_CLI_VERSION_OVERRIDE: OnceLock<String> = OnceLock::new();

/// Records the externally supplied Codex CLI version for this process.
pub fn set_codex_cli_version_override(version: String) {
    let version = version.trim();
    if version.is_empty() {
        return;
    }

    let _ = CODEX_CLI_VERSION_OVERRIDE.set(version.to_string());
}

/// Returns the Codex CLI version shown in the UI and sent in runtime metadata.
pub(crate) fn codex_cli_version() -> &'static str {
    CODEX_CLI_VERSION_OVERRIDE
        .get()
        .map(String::as_str)
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}
