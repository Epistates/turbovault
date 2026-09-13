use std::collections::BTreeMap;

/// What a completion request is asking a plugin to complete.
///
/// Both variants name the plugin-local identifier, not the published one: a
/// plugin never spells its own namespace here any more than it does when
/// declaring a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompletionTarget {
    /// An argument of a prompt, by local name.
    Prompt(String),
    /// An expression in a resource template, by local URI template.
    ResourceTemplate(String),
}

/// A request to suggest values for an argument a user is part-way through.
///
/// Completion is what makes a resource template usable: without it a client can
/// only show `note/{path}` and hope the user types a real path. It is called
/// while the user types, so answer from state already in memory rather than
/// scanning the vault on each keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompletionRequest {
    /// The prompt or resource template being filled in.
    pub target: CompletionTarget,
    /// Name of the argument or template expression being completed.
    pub argument: String,
    /// What the user has typed so far. Often empty on the first request.
    pub value: String,
    /// Arguments of the same target the client has already resolved.
    ///
    /// A template with more than one expression can use these to narrow later
    /// suggestions to what is reachable given the earlier choices.
    pub resolved: BTreeMap<String, String>,
}

impl CompletionRequest {
    /// Construct a request for `argument` with the partial input `value`.
    pub fn new(
        target: CompletionTarget,
        argument: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            target,
            argument: argument.into(),
            value: value.into(),
            resolved: BTreeMap::new(),
        }
    }

    /// Attach the arguments the client has already resolved.
    pub fn with_resolved(mut self, resolved: BTreeMap<String, String>) -> Self {
        self.resolved = resolved;
        self
    }
}

/// Suggested values for one argument.
///
/// The host truncates [`Self::values`] to the limit MCP places on a single
/// completion response and marks the result as having more, so a plugin may
/// return everything it knows without tracking the protocol's cap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Completion {
    /// Suggestions, best first.
    pub values: Vec<String>,
    /// How many exist in total, when the plugin knows and it exceeds what it
    /// returned.
    pub total: Option<u32>,
    /// Whether more exist beyond [`Self::values`].
    ///
    /// Leave unset unless the plugin itself truncated; the host sets it when
    /// its own truncation drops values.
    pub has_more: Option<bool>,
}

impl Completion {
    /// Suggest nothing. A valid answer, and the default.
    pub fn none() -> Self {
        Self::default()
    }

    /// Suggest `values`, best first.
    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        Self {
            values: values.into_iter().collect(),
            ..Self::default()
        }
    }

    /// Report how many suggestions exist in total.
    pub fn with_total(mut self, total: u32) -> Self {
        self.total = Some(total);
        self
    }

    /// Report that the plugin itself returned only part of what it knows.
    pub fn with_has_more(mut self, has_more: bool) -> Self {
        self.has_more = Some(has_more);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_default_to_suggesting_nothing() {
        let empty = Completion::none();
        assert!(empty.values.is_empty());
        assert_eq!(empty.total, None);
        assert_eq!(empty.has_more, None);
        assert_eq!(empty, Completion::default());
    }

    #[test]
    fn requests_carry_previously_resolved_arguments() {
        let request = CompletionRequest::new(
            CompletionTarget::ResourceTemplate("note/{folder}/{name}".to_string()),
            "name",
            "dai",
        )
        .with_resolved(BTreeMap::from([(
            "folder".to_string(),
            "journal".to_string(),
        )]));
        assert_eq!(
            request.resolved.get("folder").map(String::as_str),
            Some("journal")
        );
        assert_eq!(request.value, "dai");
    }
}
