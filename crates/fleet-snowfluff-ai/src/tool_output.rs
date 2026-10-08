//! Bounding one tool result inside an agent run (`agent-runtime`'s "Tool output
//! is bounded within a run").
//!
//! A tool can return far more than a model needs, or than its context window
//! can hold: a Notion page fetched through MCP had no size limit at all before
//! this. A result over the bound is cut off at the bound, ends with a marker
//! saying so, and the rest is saved to the application's spill directory so the
//! assistant can read it if it needs to.
//!
//! The rest is saved as numbered parts, each small enough for `read_file` to
//! return whole. `read_file` reads at most 20 KiB of a file and has no offset,
//! so one big spill file could never be read past its first 20 KiB, which is
//! exactly what the model already has.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::token_estimate::estimate_tokens;

/// The most estimated tokens one tool result may add to the conversation.
pub const TOOL_OUTPUT_MAX_TOKENS: usize = 6_000;
/// Share of the model's context window one tool result may use, so a single
/// result cannot fill a small window.
pub const TOOL_OUTPUT_WINDOW_PERCENT: usize = 25;
/// Size of each saved part. Below `read_file`'s 20 KiB limit so a part is
/// returned whole.
pub const SPILL_PART_BYTES: usize = 18 * 1024;
/// The most parts saved for one result; anything beyond is dropped and the
/// marker says so.
pub const SPILL_MAX_PARTS: usize = 50;
/// How many spill files are kept before the oldest are removed.
pub const SPILL_MAX_FILES: usize = 200;
/// How long a spill file is kept.
pub const SPILL_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Room kept for the marker itself when working out how much of the result to
/// show.
const MARKER_RESERVE_TOKENS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputBound {
    pub max_tokens: usize,
    /// Where the rest of an oversize result is saved. `None` saves nothing.
    pub spill_dir: Option<PathBuf>,
}

impl Default for ToolOutputBound {
    fn default() -> Self { Self { max_tokens: TOOL_OUTPUT_MAX_TOKENS, spill_dir: None } }
}

impl ToolOutputBound {
    /// The fixed bound, lowered to a share of the model's window when that is
    /// smaller.
    pub fn for_window(window_tokens: Option<usize>, spill_dir: Option<PathBuf>) -> Self {
        let max_tokens = match window_tokens {
            Some(window) => TOOL_OUTPUT_MAX_TOKENS.min(window * TOOL_OUTPUT_WINDOW_PERCENT / 100),
            None => TOOL_OUTPUT_MAX_TOKENS,
        };
        Self { max_tokens, spill_dir }
    }
}

static SPILL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn safe_name(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(40)
        .collect();
    if cleaned.is_empty() {
        "tool".to_string()
    } else {
        cleaned
    }
}

/// Byte offsets at which `text` can be split, one per character plus the end.
fn boundaries(text: &str) -> Vec<usize> {
    text.char_indices().map(|(index, _)| index).chain(std::iter::once(text.len())).collect()
}

/// The longest prefix of `text`, on a character boundary, whose estimated size
/// is within `max_tokens`.
fn longest_prefix_within(text: &str, max_tokens: usize) -> usize {
    let cuts = boundaries(text);
    let (mut low, mut high) = (0usize, cuts.len() - 1);
    while low < high {
        let middle = (low + high).div_ceil(2);
        if estimate_tokens(&text[..cuts[middle]]) <= max_tokens {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    cuts[low]
}

/// Removes spill files older than `max_age`, then the oldest ones beyond
/// `max_files`.
pub fn prune_spill_dir(dir: &Path, max_files: usize, max_age: Duration, now: SystemTime) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(PathBuf, SystemTime)> = read
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| (entry.path(), meta.modified().unwrap_or(now)))
        })
        .collect();
    files.retain(|(path, modified)| {
        let expired = now.duration_since(*modified).is_ok_and(|age| age > max_age);
        if expired {
            let _ = std::fs::remove_file(path);
        }
        !expired
    });
    files.sort_by_key(|(_, modified)| *modified);
    while files.len() > max_files {
        let (path, _) = files.remove(0);
        let _ = std::fs::remove_file(path);
    }
}

struct Spilled {
    first: PathBuf,
    parts: usize,
    cut_short: bool,
}

/// Saves `rest` as numbered parts under `dir`. `None` when the directory cannot
/// be written.
fn spill(dir: &Path, label: &str, rest: &str) -> Option<Spilled> {
    std::fs::create_dir_all(dir).ok()?;
    prune_spill_dir(dir, SPILL_MAX_FILES, SPILL_MAX_AGE, SystemTime::now());
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let unique = SPILL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = format!("{}-{stamp}-{unique}", safe_name(label));

    let mut offset = 0usize;
    let mut parts = 0usize;
    let mut first = None;
    while offset < rest.len() && parts < SPILL_MAX_PARTS {
        let mut end = (offset + SPILL_PART_BYTES).min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        parts += 1;
        let path = dir.join(format!("{base}-part-{parts:03}.txt"));
        std::fs::write(&path, &rest[offset..end]).ok()?;
        first.get_or_insert(path);
        offset = end;
    }
    Some(Spilled { first: first?, parts, cut_short: offset < rest.len() })
}

/// `content` unchanged if it fits `bound`; otherwise its beginning followed by
/// a marker, with the rest saved in the spill directory.
pub fn bound_tool_output(content: &str, label: &str, bound: &ToolOutputBound) -> String {
    let total = estimate_tokens(content);
    if total <= bound.max_tokens {
        return content.to_string();
    }
    let budget = if bound.max_tokens > MARKER_RESERVE_TOKENS * 2 {
        bound.max_tokens - MARKER_RESERVE_TOKENS
    } else {
        bound.max_tokens / 2
    };
    let cut = longest_prefix_within(content, budget);
    let (shown, rest) = content.split_at(cut);
    let shown_tokens = estimate_tokens(shown);

    let saved = bound.spill_dir.as_deref().and_then(|dir| spill(dir, label, rest));
    let note = match saved {
        Some(spilled) if spilled.parts == 1 && !spilled.cut_short => format!(
            "The rest was saved in {}. Read it with read_file if you need it.",
            spilled.first.display()
        ),
        Some(spilled) => format!(
            "The rest was saved in {} numbered parts, starting at {}. Read them in order with \
             read_file if you need them.{}",
            spilled.parts,
            spilled.first.display(),
            if spilled.cut_short {
                " Only the first parts could be saved; the very end of the output is not available."
            } else {
                ""
            }
        ),
        None => "The rest could not be saved.".to_string(),
    };
    format!(
        "{shown}\n\n[This output was cut off after about {shown_tokens} of {total} estimated \
         tokens. {note}]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fleet-snowfluff-tool-output-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn bound(max_tokens: usize, dir: Option<PathBuf>) -> ToolOutputBound {
        ToolOutputBound { max_tokens, spill_dir: dir }
    }

    fn parts_in(dir: &Path) -> Vec<(PathBuf, String)> {
        let mut parts: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| (e.path(), std::fs::read_to_string(e.path()).unwrap()))
            .collect();
        parts.sort();
        parts
    }

    #[test]
    fn output_within_the_bound_is_unchanged() {
        let text = "a short result";
        assert_eq!(bound_tool_output(text, "web_search", &bound(1000, None)), text);
    }

    #[test]
    fn an_oversize_result_is_cut_off_with_a_visible_marker_and_the_rest_is_saved() {
        let dir = temp_dir("oversize");
        let text = "word ".repeat(20_000);
        let bounded = bound_tool_output(&text, "notion-fetch", &bound(1_000, Some(dir.clone())));

        assert!(bounded.len() < text.len());
        assert!(bounded.contains("This output was cut off"));
        assert!(bounded.contains(dir.to_string_lossy().as_ref()));
        assert!(!parts_in(&dir).is_empty());
    }

    #[test]
    fn the_shown_text_plus_the_saved_parts_reproduce_the_original_exactly() {
        let dir = temp_dir("lossless");
        let text: String = (0..30_000).map(|i| format!("line {i} 今天想喝綠茶\n")).collect();
        let bounded = bound_tool_output(&text, "read", &bound(2_000, Some(dir.clone())));

        let shown = bounded.split("\n\n[This output was cut off").next().unwrap();
        let saved: String = parts_in(&dir).into_iter().map(|(_, body)| body).collect();
        assert_eq!(format!("{shown}{saved}"), text);
    }

    #[test]
    fn each_saved_part_fits_what_read_file_returns_whole() {
        let dir = temp_dir("part-size");
        let text = "今天想喝綠茶，".repeat(30_000);
        bound_tool_output(&text, "mcp", &bound(1_000, Some(dir.clone())));
        let parts = parts_in(&dir);
        assert!(parts.len() > 1);
        for (path, body) in parts {
            assert!(body.len() <= SPILL_PART_BYTES, "{path:?} is {} bytes", body.len());
            assert!(body.len() <= 20 * 1024, "must stay under read_file's 20 KiB limit");
        }
    }

    #[test]
    fn a_cjk_result_is_bounded_by_estimated_tokens_not_characters() {
        let text = "今天想喝綠茶，麻糬又不吃飯了。".repeat(2_000);
        let bounded = bound_tool_output(&text, "cjk", &bound(1_000, None));
        let shown = bounded.split("\n\n[This output was cut off").next().unwrap();
        assert!(estimate_tokens(shown) <= 1_000);
        assert!(shown.chars().count() < 1_100, "about one token per character, not four");
    }

    #[test]
    fn without_a_spill_directory_the_marker_says_the_rest_was_not_saved() {
        let text = "word ".repeat(20_000);
        let bounded = bound_tool_output(&text, "tool", &bound(1_000, None));
        assert!(bounded.contains("could not be saved"));
    }

    #[test]
    fn an_unwritable_spill_directory_degrades_to_the_same_note() {
        let file = temp_dir("unwritable").with_extension("blocker");
        std::fs::write(&file, "i am a file, not a directory").unwrap();
        let text = "word ".repeat(20_000);
        let bounded = bound_tool_output(&text, "tool", &bound(1_000, Some(file.join("sub"))));
        assert!(bounded.contains("could not be saved"));
    }

    #[test]
    fn a_result_too_big_for_the_part_cap_says_only_the_first_parts_were_saved() {
        let dir = temp_dir("part-cap");
        let text = "x".repeat(SPILL_PART_BYTES * (SPILL_MAX_PARTS + 5));
        let bounded = bound_tool_output(&text, "huge", &bound(1_000, Some(dir.clone())));
        assert!(bounded.contains("Only the first parts could be saved"));
        assert_eq!(parts_in(&dir).len(), SPILL_MAX_PARTS);
    }

    #[test]
    fn a_small_window_lowers_the_bound() {
        assert_eq!(ToolOutputBound::for_window(Some(4096), None).max_tokens, 1024);
        assert_eq!(
            ToolOutputBound::for_window(Some(32768), None).max_tokens,
            TOOL_OUTPUT_MAX_TOKENS
        );
        assert_eq!(ToolOutputBound::for_window(None, None).max_tokens, TOOL_OUTPUT_MAX_TOKENS);
    }

    #[test]
    fn old_and_excess_spill_files_are_pruned() {
        let dir = temp_dir("prune");
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        for (name, age_days) in [("old-a", 30u64), ("old-b", 20), ("new-c", 1), ("new-d", 0)] {
            let path = dir.join(name);
            std::fs::write(&path, name).unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(now - Duration::from_secs(age_days * 86_400)).unwrap();
        }
        prune_spill_dir(&dir, 10, Duration::from_secs(7 * 86_400), now);
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!left.contains(&"old-a".to_string()) && !left.contains(&"old-b".to_string()));
        assert_eq!(left.len(), 2);

        prune_spill_dir(&dir, 1, Duration::from_secs(7 * 86_400), now);
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["new-d".to_string()], "the oldest beyond the count limit goes first");
    }
}
