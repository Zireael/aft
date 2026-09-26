//! The name the calling host registered AFT's command tool under.
//!
//! OpenCode 1 and Pi register the command tool as `bash`, with the companions
//! `bash_status`, `bash_watch`, `bash_kill` and `bash_write`. OpenCode 2 ships its
//! own command tool named `shell`, and AFT replaces it there under that name,
//! so its agent sees `shell`, `shell_status`, `shell_watch`, `shell_kill` and
//! `shell_write`. Text AFT renders that tells an agent which tool to call next
//! has to use the names that agent was actually given.
//!
//! The name travels with each request, never as process- or project-wide
//! configuration: one daemon can serve an OpenCode 1 session and an OpenCode 2
//! session on the same project at once, and each must read its own names. A
//! background task keeps the name of the request that started it, so text
//! rendered later for that task (completion notes, status replies, detach
//! notices) still names the tools of the session that owns it.
//!
//! Only display text changes. The wire command names (`bash`, `bash_status`,
//! ...), the `bash.*` config keys, the `disabled_tools` entries users write and
//! the `bash-<hex>` task IDs keep their canonical spelling on every host.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Request parameter that carries the host's name for the command tool.
pub const COMMAND_TOOL_NAME_PARAM: &str = "command_tool_name";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandToolName {
    /// OpenCode 1, Pi and every other caller: the tool is `bash`.
    #[default]
    Bash,
    /// OpenCode 2: the tool is `shell`.
    Shell,
}

impl CommandToolName {
    /// Read the name from request parameters, looking inside a nested `params`
    /// object the way the bash commands accept their arguments. A missing value
    /// means `bash`, which is what every caller that predates this parameter
    /// registers; an unknown value is an error rather than a silent `bash`.
    pub fn from_params(params: &Value) -> Result<Self, String> {
        let raw = params.get(COMMAND_TOOL_NAME_PARAM).or_else(|| {
            params
                .get("params")
                .and_then(|nested| nested.get(COMMAND_TOOL_NAME_PARAM))
        });
        match raw {
            None | Some(Value::Null) => Ok(Self::Bash),
            Some(Value::String(name)) if name == "bash" => Ok(Self::Bash),
            Some(Value::String(name)) if name == "shell" => Ok(Self::Shell),
            Some(other) => Err(format!(
                "{COMMAND_TOOL_NAME_PARAM} must be \"bash\" or \"shell\", got {other}"
            )),
        }
    }

    pub fn is_bash(&self) -> bool {
        matches!(self, Self::Bash)
    }

    /// The command tool itself.
    pub fn command(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Shell => "shell",
        }
    }

    pub fn status(self) -> &'static str {
        match self {
            Self::Bash => "bash_status",
            Self::Shell => "shell_status",
        }
    }

    pub fn watch(self) -> &'static str {
        match self {
            Self::Bash => "bash_watch",
            Self::Shell => "shell_watch",
        }
    }

    pub fn kill(self) -> &'static str {
        match self {
            Self::Bash => "bash_kill",
            Self::Shell => "shell_kill",
        }
    }

    pub fn write(self) -> &'static str {
        match self {
            Self::Bash => "bash_write",
            Self::Shell => "shell_write",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_or_bash_reads_as_bash_and_shell_as_shell() {
        assert_eq!(
            CommandToolName::from_params(&json!({})).unwrap(),
            CommandToolName::Bash
        );
        assert_eq!(
            CommandToolName::from_params(&json!({"command_tool_name": "bash"})).unwrap(),
            CommandToolName::Bash
        );
        assert_eq!(
            CommandToolName::from_params(&json!({"command_tool_name": "shell"})).unwrap(),
            CommandToolName::Shell
        );
        assert_eq!(
            CommandToolName::from_params(&json!({"params": {"command_tool_name": "shell"}}))
                .unwrap(),
            CommandToolName::Shell
        );
    }

    #[test]
    fn unknown_names_are_refused() {
        assert!(CommandToolName::from_params(&json!({"command_tool_name": "zsh"})).is_err());
        assert!(CommandToolName::from_params(&json!({"command_tool_name": 1})).is_err());
    }

    #[test]
    fn every_companion_follows_the_base_name() {
        let shell = CommandToolName::Shell;
        assert_eq!(
            [
                shell.command(),
                shell.status(),
                shell.watch(),
                shell.kill(),
                shell.write()
            ],
            [
                "shell",
                "shell_status",
                "shell_watch",
                "shell_kill",
                "shell_write"
            ]
        );
        let bash = CommandToolName::Bash;
        assert_eq!(
            [
                bash.command(),
                bash.status(),
                bash.watch(),
                bash.kill(),
                bash.write()
            ],
            [
                "bash",
                "bash_status",
                "bash_watch",
                "bash_kill",
                "bash_write"
            ]
        );
    }
}
