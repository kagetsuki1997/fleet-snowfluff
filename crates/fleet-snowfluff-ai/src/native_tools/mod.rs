//! The 4 native tools offered to Ollama's `ToolCallingProvider` path
//! (`agent-core-and-task-router`'s Group 5): `web_search`, `read_file`/
//! `list_directory`, `run_command`, and `get_system_context`. Each is
//! its own `Tool` implementation in its own file; nothing here beyond
//! module wiring.

pub mod file_tools;
pub mod run_command;
pub mod system_context;
pub mod web_search;
