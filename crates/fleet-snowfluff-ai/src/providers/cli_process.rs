//! Shared helper for every CLI-subprocess-backed provider (`claude`,
//! and `codex` once it exists) -- not Anthropic- or Claude-specific,
//! unlike `claude_code_cli`, which is why it lives in its own small
//! module rather than inside either provider's file.

use crate::message::ProviderError;

/// Maps a failed-to-spawn error to `RuntimeUnavailable` (binary missing
/// or not executable) vs. some other OS-level failure.
pub(crate) fn map_spawn_error(cli: &str, err: std::io::Error) -> ProviderError {
    if err.kind() == std::io::ErrorKind::NotFound {
        ProviderError::RuntimeUnavailable(format!("`{cli}` is not installed or not on PATH"))
    } else {
        ProviderError::RuntimeUnavailable(format!("could not run `{cli}`: {err}"))
    }
}
