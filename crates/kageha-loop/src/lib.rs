//! Offline reduction of the Kageha run journal.
pub mod native_chat;
pub mod server;
pub mod history_migration;
pub mod document_migration;
pub mod project_migration;
pub(crate) mod knowledge_migration;
pub(crate) mod ui_knowledge;
pub mod session;
pub mod ssh;
pub mod webui;

pub use kageha_runtime::*;

pub(crate) mod browser_client;
pub(crate) mod ui_approvals;
mod ui_questions;
pub(crate) mod ui_tools;
mod ui_news;
mod ui_web_search;
mod ui_research;
mod ui_history;
mod ui_discovery;
mod ui_plugin_catalog;
mod ui_plugin_apps;
mod ui_plugin_components;
pub(crate) mod ui_plugins;
pub(crate) mod service_host;
pub(crate) mod ui_uploads;
pub(crate) mod web_browser;

pub(crate) mod ui_agents;
pub(crate) mod ui_jobs;
pub(crate) mod ui_team_dispatch;
pub(crate) mod ui_planning;
pub(crate) mod ui_goals;
pub(crate) mod ui_run_changes;
pub(crate) mod ui_persistence;
pub(crate) mod ui_settings;
pub(crate) mod ui_store;

pub(crate) mod ui_legacy;

pub(crate) mod ui_mcp;
mod ui_mcp_oauth;
mod ui_mcp_mtls;

pub(crate) mod ui_skills;
pub(crate) mod ui_projects;
pub(crate) mod ui_worktrees;
pub(crate) mod ui_hooks;
mod ui_session_lifecycle;

pub mod documents;
pub(crate) mod native_io;



pub(crate) mod ui_effects;
pub(crate) mod ui_remote;

pub(crate) mod native_auth;

pub(crate) mod native_media;

pub(crate) mod native_channels;

pub(crate) mod docker_desktop;
mod machine_io;
mod linux_session;
mod machine_workspace;
pub(crate) mod ui_acknowledgment;
pub(crate) mod ui_attachments;
pub(crate) mod ui_vision;

#[cfg(feature="computer")]
pub(crate) mod computer_host;
#[cfg(not(feature="computer"))]
#[path="computer_disabled.rs"]
pub(crate) mod computer_host;

pub(crate) mod runtime_host;
pub mod legacy_browser_recovery;

pub(crate) mod chat_archive;
mod ui_session_transfer;

pub(crate) mod browser_scope;

pub(crate) mod ui_session_events;

pub(crate) mod ui_workflow;
mod ui_task_completion;
mod ui_submission_confirmation;
mod ui_progress;
pub(crate) mod ui_workspace;
pub(crate) mod ui_handoff;
pub(crate) mod ui_workspace_api;
pub(crate) mod ui_host_files;
pub(crate) mod ui_shell;
pub(crate) mod ui_shell_socket;
pub(crate) mod supervisor;
pub(crate) mod component_restarts;
pub(crate) mod ui_tool_spill;
pub(crate) mod ui_connector_api;
pub(crate) mod ui_read_cache;
mod ui_tool_outcomes;
pub(crate) mod ui_tool_errors;
mod ui_prompt;
mod ui_verification;
pub(crate) mod ui_browser_decisions;

pub(crate) mod ui_job_controls;
pub(crate) mod ui_turn_checkpoints;
pub(crate) mod ui_turn_resume;
pub(crate) mod ui_turn_failure;
pub(crate) mod ui_credentials;
pub(crate) mod model_router;
pub mod model_migration;

pub(crate) mod ui_job_worktrees;

pub(crate) mod agent_export;

pub(crate) mod agent_host;
pub(crate) mod ui_learning;
pub(crate) mod ui_learned_context;
pub(crate) mod ui_learning_secrets;
pub(crate) mod ui_learning_metrics;
pub(crate) mod ui_reflection;
pub(crate) mod ui_skill_uses;
#[cfg(test)]
mod audited_session_tests;
#[cfg(test)]
mod tool_list_tests;
#[cfg(test)]
mod learning_isolation_tests;
#[cfg(test)]
mod connector_cli_tests;
#[cfg(test)]
mod ui_worker_workspace_tests;
#[cfg(test)]
mod ui_workspace_inputs_tests;
#[cfg(test)]
mod ui_resume_request_tests;
#[cfg(test)]
mod ui_turn_failure_tests;
#[cfg(test)]
mod ui_attachment_replay_tests;
