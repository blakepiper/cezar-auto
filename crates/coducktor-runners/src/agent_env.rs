//! Least-privilege environment for spawned agent backends (#427).
//!
//! Every backend used to inherit the FULL parent environment, handing `GITHUB_TOKEN`,
//! `ANTHROPIC_API_KEY`, `AWS_*` and any other host secret to a process an attacker-controlled
//! prompt can drive. Instead [`build_child_env`] builds an explicit, curated child env: a base
//! allowlist of the non-secret vars a shell / dev toolchain genuinely needs, plus the specific
//! auth vars the chosen backend requires, plus coducktor's own `DUCK_*` namespace and the per-run
//! `extra_env`. Everything else — notably arbitrary secrets — is dropped by default.
//!
//! Zero-config: the safe env is the default and needs no configuration. Two opt-in escape
//! hatches (both read from the host env, both off by default): `DUCK_ENV_PASSTHROUGH=A,B,C`
//! forwards those extra named vars; `DUCK_AGENT_ENV_FULL=1` restores the legacy full-env
//! behavior.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use coducktor_contract::Runner;
use regex::Regex;

/// The single source of truth for "is this env var name a credential?" in the child-env filter.
static SECRET_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|_KEY$|_KEY_|APIKEY|API_KEY|PRIVATE_KEY|ACCESS_KEY|_AUTH$|_AUTH_|SESSION|COOKIE|PASSPHRASE)")
        .expect("SECRET_NAME_RE is a fixed, valid pattern")
});

pub fn looks_secret(name: &str) -> bool {
    SECRET_NAME_RE.is_match(name)
}

fn upper_set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|name| name.to_uppercase()).collect()
}

/// Exact var names always forwarded — non-secret, needed by shells/tools. Var name matching is
/// case-INSENSITIVE throughout this module (#427 review): Windows spells its essentials `Path`,
/// `SystemRoot`, `ComSpec`, `windir`, so an exact-case match drops every one of them and the
/// spawned binary cannot even be resolved. The child env still carries each var under the
/// parent's ORIGINAL spelling — Windows tooling expects `Path`, and normalizing the key would
/// break it just as badly.
static BASE_ALLOW_NAMES: LazyLock<HashSet<String>> = LazyLock::new(|| {
    upper_set(&[
        "PATH",
        "HOME",
        "SHELL",
        "USER",
        "LOGNAME",
        "LNAME",
        "PWD",
        "OLDPWD",
        "SHLVL",
        "HOSTNAME",
        "HOST",
        "HOSTTYPE",
        "MACHTYPE",
        "OSTYPE",
        "TMPDIR",
        "TEMP",
        "TMP",
        "TZ",
        "LANG",
        "LANGUAGE",
        "TERM",
        "TERMINFO",
        "COLORTERM",
        "COLUMNS",
        "LINES",
        "CLICOLOR",
        "FORCE_COLOR",
        "NO_COLOR",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
        "EDITOR",
        "VISUAL",
        "PAGER",
        "LESS",
        "MANPATH",
        "INFOPATH",
        "GIT_PAGER",
        "GIT_EDITOR",
        "GIT_TERMINAL_PROMPT",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_EXEC_PATH",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "FTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "ALLUSERSPROFILE",
        "PUBLIC",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMW6432",
        "COMMONPROGRAMFILES",
        "COMMONPROGRAMFILES(X86)",
        "COMMONPROGRAMW6432",
        "PSMODULEPATH",
        "USERNAME",
        "USERDOMAIN",
        "COMPUTERNAME",
        "LOGONSERVER",
        "SESSIONNAME",
        "OS",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        "PROCESSOR_IDENTIFIER",
        "PROCESSOR_LEVEL",
        "PROCESSOR_REVISION",
    ])
});

/// Safe prefix families — dev toolchains that agents drive (node, python, rust, go, java, …). A
/// name here still drops if it matches a secret pattern (e.g. `NODE_AUTH_TOKEN`).
const BASE_ALLOW_PREFIXES: &[&str] = &[
    "LC_",
    "XDG_",
    "LESS_",
    "NODE_",
    "NPM_CONFIG_",
    "NVM_",
    "PNPM_",
    "YARN_",
    "BUN_",
    "DENO_",
    "VOLTA_",
    "FNM_",
    "COREPACK_",
    "PYENV",
    "PYTHON",
    "PIP_",
    "PIPENV_",
    "POETRY_",
    "VIRTUAL_ENV",
    "CONDA_",
    "UV_",
    "RBENV",
    "RUBY",
    "GEM_",
    "BUNDLE_",
    "RUSTUP_",
    "CARGO_",
    "RUST_",
    "GOPATH",
    "GOROOT",
    "GOBIN",
    "GOCACHE",
    "GOMODCACHE",
    "GOFLAGS",
    "GOPROXY",
    "GOPRIVATE",
    "GOTOOLCHAIN",
    "GOOS",
    "GOARCH",
    "GOENV",
    "GOWORK",
    "GO111MODULE",
    "JAVA",
    "JDK_",
    "MAVEN_",
    "GRADLE_",
    "KOTLIN_",
    "ANDROID_",
    "SDKMAN_",
    "DOTNET_",
    "NUGET_",
    "SWIFT",
    "XCODE",
    "HOMEBREW_",
    "ASDF_",
    "MISE_",
    "COMPOSER_",
];

/// The provider credentials a backend that selects models as `provider/model` may need — it can
/// be pointed at any of them, so none can be pruned without breaking a legitimate model id.
const MULTI_PROVIDER_PREFIXES: &[&str] = &[
    "OPENAI_",
    "ANTHROPIC_",
    "AZURE_OPENAI_",
    "OPENROUTER_",
    "GROQ_",
    "MISTRAL_",
    "GEMINI_",
    "GOOGLE_GENERATIVE_AI_",
    "DEEPSEEK_",
    "XAI_",
    "PERPLEXITY_",
    "TOGETHER_",
    "FIREWORKS_",
];

/// Per-backend auth/config the runner genuinely needs, by prefix. `pi` selects models as
/// `provider/model` (#387), so it needs both its own config and any provider a configured model
/// id can name — the same set OpenCode gets, for the same reason. Deliberately NOT `CLAUDE_`: pi
/// is not Claude Code and reads none of its variables.
fn backend_allow_prefixes(backend: Runner) -> Vec<&'static str> {
    match backend {
        Runner::Claude => vec!["ANTHROPIC_", "CLAUDE_"],
        Runner::Codex => vec!["OPENAI_", "CODEX_", "AZURE_OPENAI_"],
        Runner::OpenCode => MULTI_PROVIDER_PREFIXES.to_vec(),
        Runner::Pi => {
            let mut prefixes = vec!["PI_"];
            prefixes.extend_from_slice(MULTI_PROVIDER_PREFIXES);
            prefixes
        }
    }
}

/// `gh` handoff (draft PRs) works in every backend — the one credential the project
/// deliberately keeps in the environment (`GITHUB_TOKEN` stays in env, never on disk).
static GH_ALLOW_NAMES: LazyLock<HashSet<String>> = LazyLock::new(|| {
    upper_set(&[
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "GH_HOST",
        "GH_ENTERPRISE_TOKEN",
        "GH_CONFIG_DIR",
    ])
});

/// Claude Code can be pointed at Bedrock or Vertex instead of the Anthropic API by
/// `CLAUDE_CODE_USE_BEDROCK=1` / `CLAUDE_CODE_USE_VERTEX=1`. Those toggles ride in on the
/// `CLAUDE_` prefix — but the cloud credentials they switch the SDK over to do not, so each
/// toggle unlocks exactly the credentials its own path needs, and only while it is on: with no
/// toggle set (the default, direct-API posture) `AWS_*` / `GOOGLE_*` stay dropped.
const BEDROCK_TOGGLE: &str = "CLAUDE_CODE_USE_BEDROCK";
const VERTEX_TOGGLE: &str = "CLAUDE_CODE_USE_VERTEX";
const BEDROCK_ALLOW_PREFIXES: &[&str] = &["AWS_"];
static VERTEX_ALLOW_NAMES: LazyLock<HashSet<String>> =
    LazyLock::new(|| upper_set(&["GOOGLE_APPLICATION_CREDENTIALS", "CLOUD_ML_REGION"]));
const VERTEX_ALLOW_PREFIXES: &[&str] = &["GOOGLE_CLOUD_"];

fn matches_prefix(name: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| name.starts_with(prefix))
}

/// Case-insensitive read of `source` — a win32 fixture may spell a name `Path`.
fn read_var<'a>(source: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    if let Some(value) = source.get(name) {
        return Some(value.as_str());
    }
    let upper = name.to_uppercase();
    source
        .iter()
        .find(|(key, _)| key.to_uppercase() == upper)
        .map(|(_, value)| value.as_str())
}

/// An opt-in toggle is on unless it is absent/empty/`0`/`false`.
fn is_truthy(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(raw) => {
            let value = raw.trim().to_lowercase();
            value != "0" && value != "false" && !value.is_empty()
        }
    }
}

pub struct BuildChildEnvOptions<'a> {
    pub backend: Runner,
    /// Per-run env (`DUCK_HANDOFF_FILE` etc.) — always applied, wins over the host.
    pub extra_env: &'a BTreeMap<String, String>,
    pub source: &'a BTreeMap<String, String>,
}

/// Build the curated child environment for a spawned backend. `extra_env` (the runner spec's own
/// env) is applied last so per-run vars always win — and, matching #785, a per-run var
/// case-insensitively REPLACES a host var under any spelling rather than merely shadowing it (a
/// surviving `Temp` beside a curated `TEMP` would hand the backend the host's exhausted temp
/// directory under another spelling).
pub fn build_child_env(opts: BuildChildEnvOptions<'_>) -> BTreeMap<String, String> {
    let source = opts.source;
    let extra = opts.extra_env;
    let overridden: HashSet<String> = extra.keys().map(|key| key.to_uppercase()).collect();

    if is_truthy(read_var(source, "DUCK_AGENT_ENV_FULL")) {
        let mut full: BTreeMap<String, String> = source
            .iter()
            .filter(|(key, _)| !overridden.contains(&key.to_uppercase()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for (key, value) in extra {
            full.insert(key.clone(), value.clone());
        }
        return full;
    }

    let backend_prefixes = backend_allow_prefixes(opts.backend);
    let passthrough: HashSet<String> = read_var(source, "DUCK_ENV_PASSTHROUGH")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_uppercase)
        .collect();

    // Cloud auth is unlocked only by the toggle that needs it, and only for a backend that is
    // actually given the toggle (the `CLAUDE_` prefix) to read.
    let claude_cloud = backend_prefixes.contains(&"CLAUDE_");
    let mut cloud_prefixes: Vec<&str> = Vec::new();
    let mut cloud_names: HashSet<String> = HashSet::new();
    if claude_cloud && is_truthy(read_var(source, BEDROCK_TOGGLE)) {
        cloud_prefixes.extend_from_slice(BEDROCK_ALLOW_PREFIXES);
    }
    if claude_cloud && is_truthy(read_var(source, VERTEX_TOGGLE)) {
        cloud_prefixes.extend_from_slice(VERTEX_ALLOW_PREFIXES);
        cloud_names.extend(VERTEX_ALLOW_NAMES.iter().cloned());
    }

    let allow = |name: &str| -> bool {
        let key = name.to_uppercase();
        if key.starts_with("DUCK_") {
            return true;
        }
        if GH_ALLOW_NAMES.contains(&key) {
            return true;
        }
        if matches_prefix(&key, &backend_prefixes) {
            return true;
        }
        if cloud_names.contains(&key) || matches_prefix(&key, &cloud_prefixes) {
            return true;
        }
        if passthrough.contains(&key) {
            return true;
        }
        if BASE_ALLOW_NAMES.contains(&key) {
            return true;
        }
        if matches_prefix(&key, BASE_ALLOW_PREFIXES) && !looks_secret(&key) {
            return true;
        }
        false
    };

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in source {
        if overridden.contains(&name.to_uppercase()) {
            continue;
        }
        if allow(name) {
            out.insert(name.clone(), value.clone());
        }
    }
    for (name, value) in extra {
        out.insert(name.clone(), value.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn host() -> BTreeMap<String, String> {
        map(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/dev"),
            ("LANG", "en_US.UTF-8"),
            ("TERM", "xterm"),
            ("SSH_AUTH_SOCK", "/tmp/ssh-abc/agent.1"),
            ("NODE_OPTIONS", "--max-old-space-size=4096"),
            ("CARGO_HOME", "/home/dev/.cargo"),
            ("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE"),
            (
                "AWS_SECRET_ACCESS_KEY",
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            ),
            ("STRIPE_SECRET_KEY", "sk_live_supersecretxxxxxxxx"),
            ("NODE_AUTH_TOKEN", "npm_tokenshouldnotleak"),
            ("RANDOM_PASSWORD", "hunter2hunter2"),
        ])
    }

    fn build(backend: Runner, source: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        build_child_env(BuildChildEnvOptions {
            backend,
            extra_env: &BTreeMap::new(),
            source,
        })
    }

    fn build_with_extra(
        backend: Runner,
        source: &BTreeMap<String, String>,
        extra: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        build_child_env(BuildChildEnvOptions {
            backend,
            extra_env: extra,
            source,
        })
    }

    #[test]
    fn forwards_safe_base_and_toolchain_vars() {
        let env = build(Runner::Claude, &host());
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/home/dev"));
        assert_eq!(env.get("LANG").map(String::as_str), Some("en_US.UTF-8"));
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm"));
        assert_eq!(
            env.get("SSH_AUTH_SOCK").map(String::as_str),
            Some("/tmp/ssh-abc/agent.1")
        );
        assert_eq!(
            env.get("NODE_OPTIONS").map(String::as_str),
            Some("--max-old-space-size=4096")
        );
        assert_eq!(
            env.get("CARGO_HOME").map(String::as_str),
            Some("/home/dev/.cargo")
        );
    }

    #[test]
    fn drops_arbitrary_host_secrets() {
        let env = build(Runner::Claude, &host());
        assert!(!env.contains_key("AWS_ACCESS_KEY_ID"));
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!env.contains_key("STRIPE_SECRET_KEY"));
        assert!(!env.contains_key("NODE_AUTH_TOKEN")); // secret name beats the NODE_ prefix
        assert!(!env.contains_key("RANDOM_PASSWORD"));
    }

    #[test]
    fn forwards_the_backend_auth_it_needs_but_not_another_backends() {
        let mut src = host();
        src.insert("ANTHROPIC_API_KEY".to_owned(), "sk-ant-xyz".to_owned());
        src.insert("OPENAI_API_KEY".to_owned(), "sk-openai-xyz".to_owned());

        let claude = build(Runner::Claude, &src);
        assert_eq!(
            claude.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-ant-xyz")
        );
        assert!(!claude.contains_key("OPENAI_API_KEY"));

        let codex = build(Runner::Codex, &src);
        assert_eq!(
            codex.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-openai-xyz")
        );
        assert!(!codex.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn keeps_github_token_and_the_duck_namespace() {
        let mut src = host();
        src.insert("GITHUB_TOKEN".to_owned(), "gho_token".to_owned());
        src.insert("DUCK_DRY_RUN".to_owned(), "1".to_owned());
        src.insert("DUCK_MOCK_ARGS_FILE".to_owned(), "/tmp/args".to_owned());

        let env = build(Runner::Claude, &src);
        assert_eq!(
            env.get("GITHUB_TOKEN").map(String::as_str),
            Some("gho_token")
        );
        assert_eq!(env.get("DUCK_DRY_RUN").map(String::as_str), Some("1"));
        assert_eq!(
            env.get("DUCK_MOCK_ARGS_FILE").map(String::as_str),
            Some("/tmp/args")
        );
    }

    #[test]
    fn applies_extra_env_last_so_per_run_vars_win() {
        let mut src = host();
        src.insert("PATH".to_owned(), "/host".to_owned());
        let extra = map(&[("DUCK_HANDOFF_FILE", "/runs/x.md"), ("PATH", "/override")]);
        let env = build_with_extra(Runner::Claude, &src, &extra);
        assert_eq!(
            env.get("DUCK_HANDOFF_FILE").map(String::as_str),
            Some("/runs/x.md")
        );
        assert_eq!(env.get("PATH").map(String::as_str), Some("/override"));
    }

    #[test]
    fn per_run_temp_directory_overrides_all_three_spellings() {
        let mut src = host();
        src.insert("TMPDIR".to_owned(), "/tmp".to_owned());
        src.insert("TEMP".to_owned(), "/tmp".to_owned());
        src.insert("TMP".to_owned(), "/tmp".to_owned());
        let extra = map(&[
            ("TMPDIR", "/data/tmp/run-1"),
            ("TEMP", "/data/tmp/run-1"),
            ("TMP", "/data/tmp/run-1"),
        ]);
        let env = build_with_extra(Runner::Claude, &src, &extra);
        assert_eq!(
            env.get("TMPDIR").map(String::as_str),
            Some("/data/tmp/run-1")
        );
        assert_eq!(env.get("TEMP").map(String::as_str), Some("/data/tmp/run-1"));
        assert_eq!(env.get("TMP").map(String::as_str), Some("/data/tmp/run-1"));
    }

    #[test]
    fn per_run_temp_directory_leaves_no_host_cased_duplicate() {
        let mut src = host();
        src.insert("Temp".to_owned(), "C:\\Windows\\Temp".to_owned());
        src.insert("Tmp".to_owned(), "C:\\Windows\\Temp".to_owned());
        let extra = map(&[
            ("TMPDIR", "D:\\run-1"),
            ("TEMP", "D:\\run-1"),
            ("TMP", "D:\\run-1"),
        ]);
        let env = build_with_extra(Runner::Claude, &src, &extra);
        let tempish: Vec<&str> = env
            .iter()
            .filter(|(k, _)| matches!(k.to_lowercase().as_str(), "tmpdir" | "temp" | "tmp"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(tempish, vec!["D:\\run-1", "D:\\run-1", "D:\\run-1"]);
    }

    #[test]
    fn per_run_temp_directory_untouched_when_the_run_overrides_nothing() {
        let mut src = host();
        src.insert("TMPDIR".to_owned(), "/tmp".to_owned());
        let env = build(Runner::Claude, &src);
        assert_eq!(env.get("TMPDIR").map(String::as_str), Some("/tmp"));
    }

    #[test]
    fn the_escape_hatch_does_not_resurrect_the_host_value_either() {
        let mut src = host();
        src.insert("DUCK_AGENT_ENV_FULL".to_owned(), "1".to_owned());
        src.insert("Temp".to_owned(), "C:\\Windows\\Temp".to_owned());
        let extra = map(&[("TEMP", "D:\\run-1")]);
        let env = build_with_extra(Runner::Claude, &src, &extra);
        assert!(!env.contains_key("Temp"));
        assert_eq!(env.get("TEMP").map(String::as_str), Some("D:\\run-1"));
    }

    #[test]
    fn opt_in_passthrough_forwards_named_extras() {
        let mut src = host();
        src.insert("MY_TOOLCHAIN_DIR".to_owned(), "/opt/tc".to_owned());
        src.insert(
            "DUCK_ENV_PASSTHROUGH".to_owned(),
            "MY_TOOLCHAIN_DIR".to_owned(),
        );
        let env = build(Runner::Claude, &src);
        assert_eq!(
            env.get("MY_TOOLCHAIN_DIR").map(String::as_str),
            Some("/opt/tc")
        );
    }

    #[test]
    fn opt_in_full_inheritance_restores_the_legacy_escape_hatch() {
        for value in ["1", "true", "yes"] {
            let mut src = host();
            src.insert("DUCK_AGENT_ENV_FULL".to_owned(), value.to_owned());
            let env = build(Runner::Claude, &src);
            assert_eq!(
                env.get("AWS_SECRET_ACCESS_KEY"),
                src.get("AWS_SECRET_ACCESS_KEY"),
                "value {value} should enable the hatch"
            );
        }
        for value in ["0", "false", ""] {
            let mut src = host();
            src.insert("DUCK_AGENT_ENV_FULL".to_owned(), value.to_owned());
            let env = build(Runner::Claude, &src);
            assert!(
                !env.contains_key("AWS_SECRET_ACCESS_KEY"),
                "value {value} should stay hardened"
            );
        }
    }

    #[test]
    fn windows_shaped_env_keeps_original_casing() {
        let win = map(&[
            ("Path", "C:\\Windows\\system32;C:\\Program Files\\nodejs"),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
            ("SystemRoot", "C:\\Windows"),
            ("SystemDrive", "C:"),
            ("ComSpec", "C:\\Windows\\system32\\cmd.exe"),
            ("windir", "C:\\Windows"),
            ("USERPROFILE", "C:\\Users\\dev"),
            ("APPDATA", "C:\\Users\\dev\\AppData\\Roaming"),
            ("LOCALAPPDATA", "C:\\Users\\dev\\AppData\\Local"),
            ("TEMP", "C:\\Users\\dev\\AppData\\Local\\Temp"),
            ("TMP", "C:\\Users\\dev\\AppData\\Local\\Temp"),
            ("ProgramFiles", "C:\\Program Files"),
        ]);
        let env = build(Runner::Claude, &win);
        assert_eq!(
            env.get("Path").map(String::as_str),
            Some("C:\\Windows\\system32;C:\\Program Files\\nodejs")
        );
        assert_eq!(
            env.get("SystemRoot").map(String::as_str),
            Some("C:\\Windows")
        );
        assert_eq!(
            env.get("ComSpec").map(String::as_str),
            Some("C:\\Windows\\system32\\cmd.exe")
        );
        assert_eq!(env.get("windir").map(String::as_str), Some("C:\\Windows"));
        assert!(env.contains_key("Path"));
        assert!(!env.contains_key("PATH"));
    }

    #[test]
    fn windows_shaped_env_still_drops_secrets_whatever_the_casing() {
        let mut win = map(&[("Path", "C:\\Windows\\system32")]);
        win.insert("Node_Auth_Token".to_owned(), "npm_shouldnotleak".to_owned());
        win.insert("Stripe_Secret_Key".to_owned(), "sk_live_x".to_owned());
        let env = build(Runner::Claude, &win);
        assert!(!env.contains_key("Node_Auth_Token"));
        assert!(!env.contains_key("Stripe_Secret_Key"));
    }

    #[test]
    fn honors_lowercased_proxy_and_passthrough_case_insensitively() {
        let win = map(&[
            ("Path", "C:\\Windows\\system32"),
            ("http_proxy", "http://p:3128"),
            ("DUCK_ENV_PASSTHROUGH", "my_tool_dir"),
            ("my_tool_dir", "C:\\tc"),
        ]);
        let env = build(Runner::Claude, &win);
        assert_eq!(
            env.get("http_proxy").map(String::as_str),
            Some("http://p:3128")
        );
        assert_eq!(env.get("my_tool_dir").map(String::as_str), Some("C:\\tc"));
    }

    #[test]
    fn bedrock_toggle_forwards_itself_and_the_aws_creds_it_needs() {
        let src = map(&[
            ("PATH", "/usr/bin"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE"),
            (
                "AWS_SECRET_ACCESS_KEY",
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            ),
            ("AWS_SESSION_TOKEN", "FwoGZXIvYXdzEExample"),
            ("AWS_REGION", "us-east-1"),
        ]);
        let env = build(Runner::Claude, &src);
        assert_eq!(
            env.get("CLAUDE_CODE_USE_BEDROCK").map(String::as_str),
            Some("1")
        );
        for key in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_REGION",
        ] {
            assert_eq!(env.get(key), src.get(key));
        }
    }

    #[test]
    fn without_the_toggle_the_same_aws_creds_are_dropped() {
        let src = map(&[
            ("PATH", "/usr/bin"),
            ("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE"),
            (
                "AWS_SECRET_ACCESS_KEY",
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            ),
        ]);
        let env = build(Runner::Claude, &src);
        assert!(!env.contains_key("AWS_ACCESS_KEY_ID"));
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn bedrock_toggle_zero_is_not_a_toggle() {
        let src = map(&[
            ("PATH", "/usr/bin"),
            ("CLAUDE_CODE_USE_BEDROCK", "0"),
            (
                "AWS_SECRET_ACCESS_KEY",
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            ),
        ]);
        let env = build(Runner::Claude, &src);
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn vertex_toggle_forwards_gcp_config_but_not_aws() {
        let src = map(&[
            ("PATH", "/usr/bin"),
            ("CLAUDE_CODE_USE_VERTEX", "true"),
            ("GOOGLE_APPLICATION_CREDENTIALS", "/home/dev/gcp.json"),
            ("CLOUD_ML_REGION", "us-east5"),
            ("ANTHROPIC_VERTEX_PROJECT_ID", "my-project"),
            ("GOOGLE_CLOUD_PROJECT", "my-project"),
            (
                "AWS_SECRET_ACCESS_KEY",
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            ),
        ]);
        let env = build(Runner::Claude, &src);
        assert_eq!(
            env.get("GOOGLE_APPLICATION_CREDENTIALS")
                .map(String::as_str),
            Some("/home/dev/gcp.json")
        );
        assert_eq!(
            env.get("CLOUD_ML_REGION").map(String::as_str),
            Some("us-east5")
        );
        assert_eq!(
            env.get("ANTHROPIC_VERTEX_PROJECT_ID").map(String::as_str),
            Some("my-project")
        );
        assert_eq!(
            env.get("GOOGLE_CLOUD_PROJECT").map(String::as_str),
            Some("my-project")
        );
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn a_backend_that_never_sees_the_toggle_never_gets_the_creds_either() {
        let src = map(&[
            ("PATH", "/usr/bin"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            (
                "AWS_SECRET_ACCESS_KEY",
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            ),
        ]);
        let env = build(Runner::Codex, &src);
        assert!(!env.contains_key("CLAUDE_CODE_USE_BEDROCK"));
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn agent_profile_config_dirs_reach_the_child() {
        let src = map(&[("PATH", "/usr/bin")]);
        let extra = map(&[("CLAUDE_CONFIG_DIR", "/home/u/.claude-klaudiusz")]);
        let env = build_with_extra(Runner::Claude, &src, &extra);
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/u/.claude-klaudiusz")
        );

        let mut host_src = src.clone();
        host_src.insert(
            "CLAUDE_CONFIG_DIR".to_owned(),
            "/home/u/.claude-host".to_owned(),
        );
        let env = build(Runner::Claude, &host_src);
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/u/.claude-host")
        );
    }

    #[test]
    fn codex_home_survives_for_codex() {
        let src = map(&[("PATH", "/usr/bin")]);
        let extra = map(&[("CODEX_HOME", "/home/u/.codex-klaudiusz")]);
        let env = build_with_extra(Runner::Codex, &src, &extra);
        assert_eq!(
            env.get("CODEX_HOME").map(String::as_str),
            Some("/home/u/.codex-klaudiusz")
        );
    }

    #[test]
    fn per_run_account_wins_under_the_full_inheritance_hatch_too() {
        let src = map(&[
            ("PATH", "/usr/bin"),
            ("DUCK_AGENT_ENV_FULL", "1"),
            ("CLAUDE_CONFIG_DIR", "/home/u/.claude"),
        ]);
        let extra = map(&[("CLAUDE_CONFIG_DIR", "/home/u/.claude-klaudiusz")]);
        let env = build_with_extra(Runner::Claude, &src, &extra);
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/u/.claude-klaudiusz")
        );
    }

    #[test]
    fn looks_secret_flags_credential_shaped_names() {
        for name in [
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "FOO_API_KEY",
            "DB_PASSWORD",
            "NODE_AUTH_TOKEN",
        ] {
            assert!(looks_secret(name), "{name} should look secret");
        }
    }

    #[test]
    fn looks_secret_does_not_flag_ordinary_names() {
        for name in ["PATH", "HOME", "NODE_OPTIONS", "CARGO_HOME", "LANG"] {
            assert!(!looks_secret(name), "{name} should not look secret");
        }
    }
}
