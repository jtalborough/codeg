//! Per-channel default agent binding, parsed from the channel `config_json`.
//!
//! A channel can behave like "one agent you talk to": `working_dir` and
//! `agent_type` are the defaults used to spawn a session when the sender hasn't
//! overridden them with `/folder` or `/agent`. Both are optional — a channel
//! with neither falls back to the old explicit `/folder` + `/agent` flow.

use serde_json::Value;

/// Default folder + agent bound to a channel.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChannelDefaults {
    /// Absolute path of the default working folder, if configured.
    pub working_dir: Option<String>,
    /// Default agent type string (serde form, e.g. `"claude_code"`), if configured.
    pub agent_type: Option<String>,
    /// Auto-approve the agent's tool-call permission requests for this channel.
    /// For a trusted persona reading/working in its own workspace, per-file
    /// permission prompts are pure friction. Defaults to `false` (prompt).
    pub auto_approve: bool,
}

/// Parse `working_dir` / `agent_type` / `auto_approve` from a channel `config_json`
/// blob. Missing keys, blanks, and parse failures yield the field default.
pub fn parse(config_json: &str) -> ChannelDefaults {
    let value: Value = serde_json::from_str(config_json).unwrap_or(Value::Null);
    let field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    ChannelDefaults {
        working_dir: field("working_dir"),
        agent_type: field("agent_type"),
        auto_approve: value
            .get("auto_approve")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_missing_yields_none() {
        assert_eq!(parse("{}"), ChannelDefaults::default());
        assert_eq!(parse("not json"), ChannelDefaults::default());
        assert_eq!(
            parse(r#"{"chat_id":"1","allowed_senders":["7"]}"#),
            ChannelDefaults::default()
        );
    }

    #[test]
    fn parses_both_fields_and_trims() {
        let cfg =
            r#"{"chat_id":"1","working_dir":"  /home/rai/src/codeg  ","agent_type":"claude_code"}"#;
        let d = parse(cfg);
        assert_eq!(d.working_dir.as_deref(), Some("/home/rai/src/codeg"));
        assert_eq!(d.agent_type.as_deref(), Some("claude_code"));
        assert!(!d.auto_approve);
    }

    #[test]
    fn blanks_are_none() {
        let d = parse(r#"{"working_dir":"   ","agent_type":""}"#);
        assert_eq!(d, ChannelDefaults::default());
    }

    #[test]
    fn one_field_present() {
        let d = parse(r#"{"agent_type":"codex"}"#);
        assert_eq!(d.working_dir, None);
        assert_eq!(d.agent_type.as_deref(), Some("codex"));
    }

    #[test]
    fn auto_approve_parsed() {
        assert!(parse(r#"{"auto_approve":true}"#).auto_approve);
        assert!(!parse(r#"{"auto_approve":false}"#).auto_approve);
        assert!(!parse(r#"{"auto_approve":"yes"}"#).auto_approve); // non-bool ignored
    }
}
