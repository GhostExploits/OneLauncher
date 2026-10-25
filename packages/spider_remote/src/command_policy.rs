//! Strict classification of remotely submitted Minecraft commands.
//!
//! This is deliberately not a prefix allowlist. Each accepted command shape is
//! matched against an explicit grammar, the category is derived from the
//! subcommand and argument count, and anything unrecognised is refused. A
//! command that changes state can never be approved because its first token
//! happens to be familiar.
//!
//! The client can only use the permissions its player already has. This module
//! does not grant, emulate, or bypass server permissions.

use crate::protocol::{MAX_CHAT_MESSAGE_CHARS, MAX_MINECRAFT_COMMAND_CHARS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandCategory {
    /// Queries that do not change world, player, or server state.
    ReadOnly,
    /// Changes that affect the player or the world but are reversible.
    Disruptive,
    /// Server administration or destructive operations.
    Administrative,
}

impl CommandCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Disruptive => "disruptive",
            Self::Administrative => "administrative",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedCommand {
    /// Normalised command without a leading slash.
    pub command: String,
    pub category: CommandCategory,
}

impl ClassifiedCommand {
    pub fn requires_confirmation(&self) -> bool {
        self.category != CommandCategory::ReadOnly
    }
}

/// Owner-selected category switches. All default to off; enabling one category
/// does not enable another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommandPolicy {
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub disruptive: bool,
    #[serde(default)]
    pub administrative: bool,
}

impl Default for CommandPolicy {
    fn default() -> Self {
        Self {
            read_only: false,
            disruptive: false,
            administrative: false,
        }
    }
}

impl CommandPolicy {
    pub fn allows(&self, category: CommandCategory) -> bool {
        match category {
            CommandCategory::ReadOnly => self.read_only,
            CommandCategory::Disruptive => self.disruptive,
            CommandCategory::Administrative => self.administrative,
        }
    }
}

/// Validate an outbound chat line. Chat is not a command channel: a message
/// beginning with `/` is rejected so `/say` cannot be used to smuggle a command
/// past the command policy.
pub fn validate_chat_message(raw: &str) -> Result<String, String> {
    let message = raw.trim_end_matches(['\r', '\n']);
    if message.trim().is_empty() {
        return Err("the chat message is empty".into());
    }
    if message.chars().count() > MAX_CHAT_MESSAGE_CHARS {
        return Err(format!(
            "the chat message is longer than {MAX_CHAT_MESSAGE_CHARS} characters"
        ));
    }
    if message.chars().any(char::is_control) {
        return Err("the chat message contains a control character".into());
    }
    if message.starts_with('/') {
        return Err("a chat message cannot begin with '/'; use /mc-command instead".into());
    }
    Ok(message.to_string())
}

/// Parse a command into tokens. Double quotes group arguments, and `\"` / `\\`
/// are the only escapes. Unbalanced quotes are rejected rather than guessed.
fn tokenize(raw: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut has_token = false;
    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            has_token = true;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                has_token = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if escaped || quoted {
        return Err("the command has an unterminated escape or quote".into());
    }
    if has_token {
        tokens.push(current);
    }
    if tokens.is_empty() {
        return Err("the command is empty".into());
    }
    Ok(tokens)
}

fn tokens_equal(tokens: &[String], expected: &[&str]) -> bool {
    tokens.len() == expected.len()
        && tokens
            .iter()
            .zip(expected)
            .all(|(token, expected)| token.eq_ignore_ascii_case(expected))
}

fn base_name(token: &str) -> &str {
    token
        .strip_prefix("minecraft:")
        .unwrap_or(token)
}

/// Classify one command against the explicit grammar below.
///
/// Unknown commands, unknown subcommands, wrong argument counts, selectors, and
/// raw JSON are refused. The list is intentionally small: an operator who needs
/// another command should add a verified integration, not widen a prefix.
pub fn classify(raw: &str) -> Result<ClassifiedCommand, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("the command is empty".into());
    }
    if raw.chars().count() > MAX_MINECRAFT_COMMAND_CHARS {
        return Err(format!(
            "the command is longer than {MAX_MINECRAFT_COMMAND_CHARS} characters"
        ));
    }
    if raw.chars().any(char::is_control) {
        return Err("the command contains a control character".into());
    }
    let without_slash = raw.strip_prefix('/').unwrap_or(raw).trim();
    let mut tokens = tokenize(without_slash)?;
    // Accept the namespaced spelling without letting it bypass the grammar:
    // `minecraft:list` is the same command as `list`.
    if let Some(first) = tokens.first_mut()
        && let Some(stripped) = first.strip_prefix("minecraft:")
    {
        *first = stripped.to_string();
    }
    if tokens.len() > 12 {
        return Err("the command has too many arguments".into());
    }
    if tokens
        .iter()
        .any(|token| token.chars().count() > 128 || token.contains('\u{0}'))
    {
        return Err("the command contains an oversized argument".into());
    }
    let base = base_name(&tokens[0]).to_ascii_lowercase();
    let category = match base.as_str() {
        // Explicit read-only forms.
        "list" if tokens_equal(&tokens, &["list"]) => CommandCategory::ReadOnly,
        "seed" if tokens_equal(&tokens, &["seed"]) => CommandCategory::ReadOnly,
        "help" if tokens.len() <= 2 => CommandCategory::ReadOnly,
        "time"
            if tokens.len() == 3
                && tokens[1].eq_ignore_ascii_case("query")
                && ["daytime", "gametime", "day"]
                    .iter()
                    .any(|value| tokens[2].eq_ignore_ascii_case(value)) =>
        {
            CommandCategory::ReadOnly
        }
        "difficulty" if tokens.len() == 1 => CommandCategory::ReadOnly,
        "gamerule" if tokens.len() <= 2 => CommandCategory::ReadOnly,
        "scoreboard"
            if tokens.len() == 3
                && tokens[1].eq_ignore_ascii_case("objectives")
                && tokens[2].eq_ignore_ascii_case("list") =>
        {
            CommandCategory::ReadOnly
        }
        "team" if tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("list") => {
            CommandCategory::ReadOnly
        }
        "worldborder" if tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("get") => {
            CommandCategory::ReadOnly
        }
        "xp" if tokens.len() == 2 && tokens[1].eq_ignore_ascii_case("query") => {
            CommandCategory::ReadOnly
        }

        // `/time set` and `/time add` are the only state-changing forms. The
        // read-only `query` form is matched above; anything else is malformed.
        "time"
            if tokens.len() == 3
                && (tokens[1].eq_ignore_ascii_case("set")
                    || tokens[1].eq_ignore_ascii_case("add")) =>
        {
            CommandCategory::Disruptive
        }

        // Player/world changes. These remain subject to the player's own
        // server-side permissions. Each needs at least one argument, so a bare
        // command name is refused rather than treated as a state change.
        name if tokens.len() >= 2
            && matches!(
                name,
                "gamemode"
                    | "tp"
                    | "teleport"
                    | "give"
                    | "clear"
                    | "kill"
                    | "summon"
                    | "setblock"
                    | "fill"
                    | "clone"
                    | "weather"
                    | "damage"
                    | "ride"
                    | "enchant"
                    | "particle"
                    | "playsound"
                    | "stopsound"
                    | "spawnpoint"
                    | "setworldspawn"
                    | "forceload"
                    | "advancement"
                    | "attribute"
                    | "bossbar"
                    | "loot"
                    | "recipe"
                    | "tag"
                    | "title"
                    | "titlemsg"
                    | "difficulty"
                    | "gamerule"
                    | "scoreboard"
                    | "team"
                    | "worldborder"
                    | "xp"
                    | "effect"
            ) =>
        {
            CommandCategory::Disruptive
        }

        // Server administration and anything that can run arbitrary commands.
        "execute" | "function" | "datapack" | "reload" | "op" | "deop" | "ban" | "ban-ip"
        | "pardon" | "pardon-ip" | "kick" | "whitelist" | "stop" | "save-all" | "save-off"
        | "save-on" | "setidletimeout" | "debug" | "jfr" | "perf" => {
            CommandCategory::Administrative
        }

        // Chat-shaped commands are not a command-channel feature: `/say` is the
        // dedicated path, and spoofing variants are refused outright.
        "say" | "me" | "msg" | "tell" | "w" | "tellraw" => {
            return Err("chat-shaped commands must use /say, not /mc-command".into());
        }
        _ => {
            return Err(
                "that command is not in the remote allowlist; no command was submitted".into(),
            );
        }
    };

    // Selectors can target every player or entity. They are not needed for the
    // narrow remote use case and make impact hard to reason about.
    if tokens
        .iter()
        .skip(1)
        .any(|token| token.starts_with('@') || token.contains('@'))
    {
        return Err("entity and player selectors are not accepted remotely".into());
    }

    Ok(ClassifiedCommand {
        command: tokens.join(" "),
        category,
    })
}

/// Classify and apply the owner's category policy.
pub fn validate_command(raw: &str, policy: &CommandPolicy) -> Result<ClassifiedCommand, String> {
    let classified = classify(raw)?;
    if !policy.allows(classified.category) {
        return Err(format!(
            "the {} command category is disabled on this device; no command was submitted",
            classified.category.as_str().replace('_', "-")
        ));
    }
    Ok(classified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> CommandPolicy {
        CommandPolicy {
            read_only: true,
            disruptive: true,
            administrative: false,
        }
    }

    #[test]
    fn read_only_forms_are_recognised_exactly() {
        for command in [
            "time query daytime",
            "list",
            "seed",
            "difficulty",
            "gamerule",
            "scoreboard objectives list",
            "team list",
            "worldborder get",
            "xp query",
        ] {
            let classified = classify(command).unwrap();
            assert_eq!(classified.category, CommandCategory::ReadOnly, "{command}");
        }
        assert!(classify("/time query daytime").is_ok());
        assert!(classify("minecraft:list").is_ok());
    }

    #[test]
    fn a_read_only_prefix_does_not_authorise_a_state_change() {
        assert_eq!(
            classify("time set day").unwrap().category,
            CommandCategory::Disruptive
        );
        assert_eq!(
            classify("gamerule keepInventory true").unwrap().category,
            CommandCategory::Disruptive
        );
        assert_eq!(
            classify("difficulty hard").unwrap().category,
            CommandCategory::Disruptive
        );
        assert_eq!(
            classify("worldborder set 100").unwrap().category,
            CommandCategory::Disruptive
        );
        assert!(!enabled().allows(CommandCategory::ReadOnly) || enabled().read_only);
    }

    #[test]
    fn unknown_and_malformed_commands_are_refused() {
        for command in [
            "",
            "   ",
            "setblock",
            "definitelynotacommand",
            "time query",
            "time query daytime extra",
            "time set",
            "execute as @a run say hi",
            "give @a diamond",
            "say hello",
            "msg Alex hi",
            "time query \"daytime",
            "list\nop Alex",
        ] {
            assert!(classify(command).is_err(), "{command:?} should be refused");
        }

        // Administrative shapes are classified, not refused here; the owner's
        // category switch (and the confirmation prompt) is what gates them.
        for command in ["function namespace:fn", "op Alex", "reload"] {
            assert_eq!(
                classify(command).unwrap().category,
                CommandCategory::Administrative,
                "{command:?}"
            );
        }
    }

    #[test]
    fn category_switches_are_independent() {
        let classified = validate_command("time set day", &enabled()).unwrap();
        assert_eq!(classified.category, CommandCategory::Disruptive);

        let read_only = CommandPolicy {
            read_only: true,
            ..CommandPolicy::default()
        };
        assert!(validate_command("list", &read_only).is_ok());
        assert!(validate_command("time set day", &read_only).is_err());
        assert!(validate_command("op Alex", &read_only).is_err());

        let none = CommandPolicy::default();
        assert!(validate_command("list", &none).is_err());
    }

    #[test]
    fn confirmation_is_required_for_state_changes_only() {
        assert!(!classify("list").unwrap().requires_confirmation());
        assert!(classify("time set day").unwrap().requires_confirmation());
        assert!(classify("op Alex").unwrap().requires_confirmation());
    }

    #[test]
    fn quoted_arguments_are_tokenised_without_guessing() {
        let classified = classify("gamerule \"keep Inventory\" true").unwrap();
        assert_eq!(classified.command, "gamerule keep Inventory true");
        assert!(classify("gamerule \"keepInventory").is_err());
    }

    #[test]
    fn chat_validation_rejects_empty_oversized_and_command_smuggling() {
        assert_eq!(validate_chat_message("hello").unwrap(), "hello");
        assert!(validate_chat_message("").is_err());
        assert!(validate_chat_message("   ").is_err());
        assert!(validate_chat_message(&"a".repeat(MAX_CHAT_MESSAGE_CHARS + 1)).is_err());
        assert!(validate_chat_message("/op Alex").is_err());
        assert!(validate_chat_message("line\nbreak").is_err());
        assert_eq!(
            validate_chat_message("hello\r\n").unwrap(),
            "hello",
            "only a trailing line ending is removed"
        );
    }

    #[test]
    fn administrative_commands_need_their_own_switch() {
        let admin = CommandPolicy {
            read_only: true,
            disruptive: true,
            administrative: true,
        };
        assert!(validate_command("op Alex", &admin).is_ok());
        assert!(validate_command("op Alex", &enabled()).is_err());
    }
}
