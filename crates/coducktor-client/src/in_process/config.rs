// ---- agent-config helpers -----------------------------------------------------------------
// catalog and its supporting functions --------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum AgentConfigPath {
    ClaudeUserSettings,
    ClaudeProjectSettings,
    ClaudeLocalSettings,
    ClaudeProjectMcp,
    ClaudeUserMemory,
    ClaudeProjectMemory,
    ClaudeLocalMemory,
    CodexUserConfig,
    CodexProjectConfig,
    CodexUserMemory,
    OpenCodeUserConfig,
    OpenCodeProjectConfig,
    OpenCodeUserMemory,
    ProjectAgents,
}

#[derive(Debug, Clone, Copy)]
struct AgentConfigDefinition {
    id: &'static str,
    runners: &'static [Runner],
    kind: AgentConfigKind,
    scope: AgentConfigScope,
    label: &'static str,
    format: AgentConfigFormat,
    tracked: AgentConfigTracked,
    seeded: bool,
    holds_mcp: bool,
    path: AgentConfigPath,
    docs_url: &'static str,
}

const CLAUDE_RUNNER: &[Runner] = &[Runner::Claude];
const CODEX_RUNNER: &[Runner] = &[Runner::Codex];
const OPENCODE_RUNNER: &[Runner] = &[Runner::OpenCode];
const CODEX_OPENCODE_RUNNERS: &[Runner] = &[Runner::Codex, Runner::OpenCode];

const AGENT_CONFIG_DEFINITIONS: &[AgentConfigDefinition] = &[
    AgentConfigDefinition {
        id: "claude.user.settings",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::User,
        label: "~/.claude/settings.json",
        format: AgentConfigFormat::Json,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeUserSettings,
        docs_url: "https://code.claude.com/docs/en/settings",
    },
    AgentConfigDefinition {
        id: "claude.project.settings",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::Project,
        label: ".claude/settings.json",
        format: AgentConfigFormat::Json,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeProjectSettings,
        docs_url: "https://code.claude.com/docs/en/settings",
    },
    AgentConfigDefinition {
        id: "claude.local.settings",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::Local,
        label: ".claude/settings.local.json",
        format: AgentConfigFormat::Json,
        tracked: AgentConfigTracked::Gitignored,
        seeded: true,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeLocalSettings,
        docs_url: "https://code.claude.com/docs/en/settings",
    },
    AgentConfigDefinition {
        id: "claude.project.mcp",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Mcp,
        scope: AgentConfigScope::Project,
        label: ".mcp.json",
        format: AgentConfigFormat::Json,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::ClaudeProjectMcp,
        docs_url: "https://code.claude.com/docs/en/mcp",
    },
    AgentConfigDefinition {
        id: "claude.user.memory",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::User,
        label: "~/.claude/CLAUDE.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeUserMemory,
        docs_url: "https://code.claude.com/docs/en/memory",
    },
    AgentConfigDefinition {
        id: "claude.project.memory",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::Project,
        label: "CLAUDE.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeProjectMemory,
        docs_url: "https://code.claude.com/docs/en/memory",
    },
    AgentConfigDefinition {
        id: "claude.local.memory",
        runners: CLAUDE_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::Local,
        label: "CLAUDE.local.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::Gitignored,
        seeded: true,
        holds_mcp: false,
        path: AgentConfigPath::ClaudeLocalMemory,
        docs_url: "https://code.claude.com/docs/en/memory",
    },
    AgentConfigDefinition {
        id: "codex.user.config",
        runners: CODEX_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::User,
        label: "~/.codex/config.toml",
        format: AgentConfigFormat::Toml,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::CodexUserConfig,
        docs_url: "https://developers.openai.com/codex/config-reference",
    },
    AgentConfigDefinition {
        id: "codex.project.config",
        runners: CODEX_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::Project,
        label: ".codex/config.toml",
        format: AgentConfigFormat::Toml,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::CodexProjectConfig,
        docs_url: "https://developers.openai.com/codex/config-reference",
    },
    AgentConfigDefinition {
        id: "codex.user.memory",
        runners: CODEX_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::User,
        label: "~/.codex/AGENTS.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::CodexUserMemory,
        docs_url: "https://developers.openai.com/codex/guides/agents-md",
    },
    AgentConfigDefinition {
        id: "opencode.user.config",
        runners: OPENCODE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::User,
        label: "~/.config/opencode/opencode.json",
        format: AgentConfigFormat::JsonC,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::OpenCodeUserConfig,
        docs_url: "https://opencode.ai/docs/config/",
    },
    AgentConfigDefinition {
        id: "opencode.project.config",
        runners: OPENCODE_RUNNER,
        kind: AgentConfigKind::Settings,
        scope: AgentConfigScope::Project,
        label: "opencode.json",
        format: AgentConfigFormat::JsonC,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: true,
        path: AgentConfigPath::OpenCodeProjectConfig,
        docs_url: "https://opencode.ai/docs/config/",
    },
    AgentConfigDefinition {
        id: "opencode.user.memory",
        runners: OPENCODE_RUNNER,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::User,
        label: "~/.config/opencode/AGENTS.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::OutsideRepo,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::OpenCodeUserMemory,
        docs_url: "https://opencode.ai/docs/rules/",
    },
    AgentConfigDefinition {
        id: "project.agents",
        runners: CODEX_OPENCODE_RUNNERS,
        kind: AgentConfigKind::Memory,
        scope: AgentConfigScope::Project,
        label: "AGENTS.md",
        format: AgentConfigFormat::Markdown,
        tracked: AgentConfigTracked::Tracked,
        seeded: false,
        holds_mcp: false,
        path: AgentConfigPath::ProjectAgents,
        docs_url: "https://opencode.ai/docs/rules/",
    },
];

fn agent_config_definition(id: &str) -> Option<&'static AgentConfigDefinition> {
    AGENT_CONFIG_DEFINITIONS
        .iter()
        .find(|definition| definition.id == id)
}

fn resolve_agent_config_path(definition: &AgentConfigDefinition, repo_root: &Path) -> PathBuf {
    let homes = agent_home_paths(&ProcessEnv);
    match definition.path {
        AgentConfigPath::ClaudeUserSettings => homes.claude.join("settings.json"),
        AgentConfigPath::ClaudeProjectSettings => repo_root.join(".claude/settings.json"),
        AgentConfigPath::ClaudeLocalSettings => repo_root.join(".claude/settings.local.json"),
        AgentConfigPath::ClaudeProjectMcp => repo_root.join(".mcp.json"),
        AgentConfigPath::ClaudeUserMemory => homes.claude.join("CLAUDE.md"),
        AgentConfigPath::ClaudeProjectMemory => repo_root.join("CLAUDE.md"),
        AgentConfigPath::ClaudeLocalMemory => repo_root.join("CLAUDE.local.md"),
        AgentConfigPath::CodexUserConfig => homes.codex.join("config.toml"),
        AgentConfigPath::CodexProjectConfig => repo_root.join(".codex/config.toml"),
        AgentConfigPath::CodexUserMemory => homes.codex.join("AGENTS.md"),
        AgentConfigPath::OpenCodeUserConfig => homes.opencode_config.join("opencode.json"),
        AgentConfigPath::OpenCodeProjectConfig => repo_root.join("opencode.json"),
        AgentConfigPath::OpenCodeUserMemory => homes.opencode_config.join("AGENTS.md"),
        AgentConfigPath::ProjectAgents => repo_root.join("AGENTS.md"),
    }
}

fn config_hash(content: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(content);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn agent_config_content(
    definition: &AgentConfigDefinition,
    repo_root: &Path,
) -> Result<AgentConfigFileContent, String> {
    let path = resolve_agent_config_path(definition, repo_root);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let content = String::from_utf8(bytes.clone()).map_err(|error| error.to_string())?;
            Ok(AgentConfigFileContent {
                id: definition.id.to_owned(),
                path: path.to_string_lossy().into_owned(),
                exists: true,
                content,
                version: Some(config_hash(&bytes)),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AgentConfigFileContent {
            id: definition.id.to_owned(),
            path: path.to_string_lossy().into_owned(),
            exists: false,
            content: String::new(),
            version: None,
        }),
        Err(error) => Err(error.to_string()),
    }
}

fn jsonc_without_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if byte >= 0x80
                && let Some(character) = input[index..].chars().next()
            {
                output.push(character);
                index += character.len_utf8();
                continue;
            }
            output.push(byte as char);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            if index < bytes.len() {
                output.push('\n');
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            output.push(' ');
            output.push(' ');
            index += 2;
            while index < bytes.len()
                && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
            {
                output.push(if bytes[index] == b'\n' { '\n' } else { ' ' });
                index += 1;
            }
            if index < bytes.len() {
                output.push(' ');
                output.push(' ');
                index += 2;
            }
            continue;
        }
        if byte >= 0x80
            && let Some(character) = input[index..].chars().next()
        {
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        output.push(byte as char);
        index += 1;
    }
    output
}

fn validate_agent_config(content: &str, format: AgentConfigFormat) -> Result<(), String> {
    if content.trim().is_empty() || matches!(format, AgentConfigFormat::Markdown) {
        return Ok(());
    }
    match format {
        AgentConfigFormat::Json => serde_json::from_str::<Value>(content)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        AgentConfigFormat::JsonC => serde_json::from_str::<Value>(&jsonc_without_comments(content))
            .map(|_| ())
            .map_err(|error| error.to_string()),
        AgentConfigFormat::Toml => toml::from_str::<toml::Value>(content)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        AgentConfigFormat::Markdown => Ok(()),
    }
}

fn claude_state_path() -> PathBuf {
    let homes = agent_home_paths(&ProcessEnv);
    let default_home = real_home_dir(&ProcessEnv).join(".claude");
    if std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
        || homes.claude != default_home
    {
        homes.claude.join(".claude.json")
    } else {
        homes.claude.parent().map_or_else(
            || PathBuf::from(".claude.json"),
            |parent| parent.join(".claude.json"),
        )
    }
}

fn user_mcp_listing() -> UserMcpListing {
    let path = claude_state_path();
    let path_string = path.to_string_lossy().into_owned();
    let Ok(metadata) = std::fs::metadata(&path) else {
        return UserMcpListing {
            path: path_string,
            servers: Vec::new(),
            readable: true,
        };
    };
    if metadata.len() > 2 * 1024 * 1024 {
        return UserMcpListing {
            path: path_string,
            servers: Vec::new(),
            readable: false,
        };
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return UserMcpListing {
            path: path_string,
            servers: Vec::new(),
            readable: false,
        };
    };
    let servers = serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.get("mcpServers").cloned())
        .and_then(|value| value.as_object().cloned())
        .map(|servers| servers.into_iter().map(|(name, _)| name).collect())
        .unwrap_or_default();
    UserMcpListing {
        path: path_string,
        servers,
        readable: true,
    }
}

fn agent_config_listing(repo_root: &Path) -> AgentConfigListing {
    let files = AGENT_CONFIG_DEFINITIONS
        .iter()
        .map(|definition| {
            let path = resolve_agent_config_path(definition, repo_root);
            let metadata = std::fs::metadata(&path).ok();
            let (exists, size, version) = match metadata {
                Some(metadata) => {
                    let version = std::fs::read(&path).ok().map(|bytes| config_hash(&bytes));
                    (true, metadata.len() as f64, version)
                }
                None => (false, 0.0, None),
            };
            AgentConfigFile {
                id: definition.id.to_owned(),
                runners: definition.runners.to_vec(),
                kind: definition.kind,
                scope: definition.scope,
                label: definition.label.to_owned(),
                path: path.to_string_lossy().into_owned(),
                format: definition.format,
                tracked: definition.tracked,
                seeded: definition.seeded,
                holds_mcp: definition.holds_mcp,
                precedence: "Vendor-documented precedence; coducktor writes the file verbatim."
                    .to_owned(),
                hot_reload: None,
                docs_url: definition.docs_url.to_owned(),
                exists,
                size,
                version,
                writable: true,
                read_only_reason: None,
            }
        })
        .collect();
    AgentConfigListing {
        editable: true,
        files,
        user_mcp: Some(user_mcp_listing()),
    }
}

fn write_agent_config(
    definition: &AgentConfigDefinition,
    repo_root: &Path,
    input: SetAgentConfigInput,
) -> Result<AgentConfigFileContent, EngineError> {
    if let Err(error) = validate_agent_config(&input.content, definition.format) {
        return Err(EngineError::Conflict {
            reason: format!("Invalid {:?}: {error}", definition.format).to_lowercase(),
        });
    }
    let path = resolve_agent_config_path(definition, repo_root);
    let current = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_err(error)),
    };
    if input.content.trim().is_empty()
        && current
            .as_ref()
            .is_some_and(|bytes| !String::from_utf8_lossy(bytes).trim().is_empty())
    {
        return Err(EngineError::Conflict {
            reason: "refusing to overwrite a non-empty config file with empty content — delete the file manually if you mean to remove it"
                .to_owned(),
        });
    }
    let current_version = current.as_deref().map(config_hash);
    if current_version != input.version {
        return Err(EngineError::Conflict {
            reason: if current_version.is_none() {
                "the file no longer exists on disk — reload before saving".to_owned()
            } else {
                "the file changed on disk since you opened it — reload before saving".to_owned()
            },
        });
    }
    let target = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if let Some(parent) = target.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return Err(io_err(error));
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = PathBuf::from(format!(
        "{}.duck-tmp-{}-{nonce}",
        target.display(),
        std::process::id()
    ));
    if let Err(error) = std::fs::write(&temporary, input.content.as_bytes()) {
        return Err(io_err(error));
    }
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(io_err(error));
    }
    agent_config_content(definition, repo_root).map_err(EngineError::Transport)
}

