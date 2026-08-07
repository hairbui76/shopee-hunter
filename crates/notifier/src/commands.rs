//! Telegram admin command parsing + authorization (ROADMAP Phase 32).
//!
//! This module is pure: it parses an incoming message into a typed command and
//! decides whether the sender is allowed to run it. Dispatch (querying state,
//! toggling controls) lives in the app layer, which owns those resources.

use std::collections::HashSet;

/// A recognized operator command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminCommand {
    Status,
    Session,
    Sources,
    Jobs,
    Recent,
    PauseClaims,
    ResumeClaims,
    Watchlist,
    Help,
}

impl AdminCommand {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Status => "/status",
            Self::Session => "/session",
            Self::Sources => "/sources",
            Self::Jobs => "/jobs",
            Self::Recent => "/recent",
            Self::PauseClaims => "/pause_claims",
            Self::ResumeClaims => "/resume_claims",
            Self::Watchlist => "/watchlist",
            Self::Help => "/help",
        }
    }

    /// Whether the command mutates state (extra allowlist is always required,
    /// but mutating commands are the ones that must never run unauthenticated).
    pub fn is_mutating(&self) -> bool {
        matches!(self, Self::PauseClaims | Self::ResumeClaims)
    }
}

/// Parse the leading token of a message into a command. Accepts an optional
/// `@botname` suffix (`/status@mybot`) and ignores trailing arguments.
pub fn parse_command(text: &str) -> Option<AdminCommand> {
    let first = text.split_whitespace().next()?;
    let cmd = first.split('@').next().unwrap_or(first);
    Some(match cmd {
        "/status" => AdminCommand::Status,
        "/session" => AdminCommand::Session,
        "/sources" => AdminCommand::Sources,
        "/jobs" => AdminCommand::Jobs,
        "/recent" => AdminCommand::Recent,
        "/pause_claims" => AdminCommand::PauseClaims,
        "/resume_claims" => AdminCommand::ResumeClaims,
        "/watchlist" => AdminCommand::Watchlist,
        "/help" | "/start" => AdminCommand::Help,
        _ => return None,
    })
}

/// Owner chat-id allowlist. Only these chat ids may run commands at all.
#[derive(Debug, Clone, Default)]
pub struct OwnerAllowlist {
    ids: HashSet<String>,
}

impl OwnerAllowlist {
    pub fn new(ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            ids: ids.into_iter().collect(),
        }
    }

    pub fn is_owner(&self, chat_id: &str) -> bool {
        self.ids.contains(chat_id)
    }
}

/// Result of authorizing a command from a chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Run the command.
    Authorized(AdminCommand),
    /// Sender is not an allowlisted owner — ignore/deny.
    Unauthorized,
    /// Not a recognized command.
    Unknown,
}

/// Authorize a raw message from a chat. Unauthorized chats can never run any
/// command, including read-only ones (the bot is single-owner).
pub fn authorize(allowlist: &OwnerAllowlist, chat_id: &str, text: &str) -> AuthOutcome {
    match parse_command(text) {
        None => AuthOutcome::Unknown,
        Some(cmd) => {
            if allowlist.is_owner(chat_id) {
                AuthOutcome::Authorized(cmd)
            } else {
                AuthOutcome::Unauthorized
            }
        }
    }
}

/// The help text listing available commands (no secrets).
pub fn help_text() -> String {
    let cmds = [
        AdminCommand::Status,
        AdminCommand::Session,
        AdminCommand::Sources,
        AdminCommand::Jobs,
        AdminCommand::Recent,
        AdminCommand::PauseClaims,
        AdminCommand::ResumeClaims,
        AdminCommand::Watchlist,
    ];
    let mut s = String::from("shopee-hunter commands:\n");
    for c in cmds {
        s.push_str(c.as_str());
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_with_botname_and_args() {
        assert_eq!(parse_command("/status"), Some(AdminCommand::Status));
        assert_eq!(parse_command("/status@myBot"), Some(AdminCommand::Status));
        assert_eq!(parse_command("/jobs now please"), Some(AdminCommand::Jobs));
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command("/unknown"), None);
    }

    #[test]
    fn only_owner_can_run_commands() {
        let allow = OwnerAllowlist::new(["12345".to_string()]);
        assert_eq!(
            authorize(&allow, "12345", "/pause_claims"),
            AuthOutcome::Authorized(AdminCommand::PauseClaims)
        );
        // Non-owner is denied even for read-only commands.
        assert_eq!(
            authorize(&allow, "99999", "/status"),
            AuthOutcome::Unauthorized
        );
        // Unknown command from an owner is Unknown, not Authorized.
        assert_eq!(authorize(&allow, "12345", "hi"), AuthOutcome::Unknown);
    }

    #[test]
    fn mutating_flag_is_correct() {
        assert!(AdminCommand::PauseClaims.is_mutating());
        assert!(AdminCommand::ResumeClaims.is_mutating());
        assert!(!AdminCommand::Status.is_mutating());
    }

    #[test]
    fn help_lists_commands_without_secrets() {
        let h = help_text();
        assert!(h.contains("/status"));
        assert!(h.contains("/pause_claims"));
        assert!(!h.to_lowercase().contains("token"));
    }
}
