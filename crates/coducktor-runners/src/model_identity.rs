//! Canonical `provider/model` identity parsing shared by the OpenCode and Pi runners. The run
//! wiring resolves the selected model before a backend receives it.

/// A backend-agnostic model identity. Serialized as `provider/model`. Treated as an identifier,
/// never display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelIdentity {
    /// Canonical provider id, lowercased — e.g. `anthropic`, `openai`.
    pub provider: String,
    /// Provider-native model id — e.g. `claude-opus-4-8`, `gpt-5.1-codex`.
    pub model: String,
}

/// Parse a `provider/model` string into a canonical identity. `None` for anything not in
/// explicit provider/model form (empty, bare, a leading or trailing slash). Keeps only the FIRST
/// slash as the separator — a model id itself may contain slashes (e.g. an OpenRouter-qualified
/// model).
pub fn parse_model_identity(raw: Option<&str>) -> Option<ModelIdentity> {
    let raw = raw?;
    let trimmed = raw.trim();
    let index = trimmed.find('/')?;
    // index == 0 rejects "/model"; index at the end rejects "provider/".
    if index == 0 || index >= trimmed.len() - 1 {
        return None;
    }
    Some(ModelIdentity {
        provider: trimmed[..index].to_lowercase(),
        model: trimmed[index + 1..].trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_an_explicit_provider_model_lowercasing_the_provider() {
        assert_eq!(
            parse_model_identity(Some("anthropic/claude-opus-4-8")),
            Some(ModelIdentity {
                provider: "anthropic".to_owned(),
                model: "claude-opus-4-8".to_owned(),
            })
        );
        assert_eq!(
            parse_model_identity(Some("OpenAI/gpt-5.1")),
            Some(ModelIdentity {
                provider: "openai".to_owned(),
                model: "gpt-5.1".to_owned(),
            })
        );
    }

    #[test]
    fn keeps_only_the_first_slash_as_the_separator() {
        assert_eq!(
            parse_model_identity(Some("openrouter/anthropic/claude-3.5")),
            Some(ModelIdentity {
                provider: "openrouter".to_owned(),
                model: "anthropic/claude-3.5".to_owned(),
            })
        );
    }

    #[test]
    fn returns_none_for_anything_not_in_provider_model_form() {
        for raw in ["", "   ", "sonnet", "/sonnet", "anthropic/"] {
            assert_eq!(parse_model_identity(Some(raw)), None, "raw = {raw:?}");
        }
        assert_eq!(parse_model_identity(None), None);
    }

    #[test]
    fn format_and_parse_round_trip() {
        let id = ModelIdentity {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-5".to_owned(),
        };
        let formatted = format!("{}/{}", id.provider, id.model);
        assert_eq!(formatted, "anthropic/claude-sonnet-5");
        assert_eq!(parse_model_identity(Some(&formatted)), Some(id));
    }
}
