use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coducktor_contract::{
    ChecksGlyph, GithubChangeStatus, GithubCheckState, GithubComment, GithubCommentsData,
    GithubData, GithubItem, GithubItemKind, GithubMergeEligibility, GithubMergeMethod,
    GithubMergeable, GithubPrCheck, GithubPrMergeState, GithubPrState, GithubReviewDecision,
    ReferenceStatus,
};
use coducktor_core::git::worktree::{AutosaveReason, AutosaveResult, autosave_commit};
use serde_json::{Value, json};

use crate::graphql::{GraphqlVariables, fetch_commit_checks, fetch_pr_checks, fetch_ref_statuses};
use crate::model::{
    DraftPrInput, DraftPrOutcome, ForgeAvailability, ForgeDriver, ForgeKind, ForgeMergeInput,
    ForgeMergeResult, ForgePrDiffResult, ForgePrMergeStateResult, ForgePrStatus, ForgeRefKind,
    GithubPrChange, GithubRepoRef,
};
use crate::normalize::{
    CACHE_MS, CheckRun, GH_CHECKS_MAX, GH_MAX_LIMIT, GH_PR_DIFF_FILE_CAP, GH_PR_DIFF_JSON_CAP,
    GH_PR_PATCH_CAP, GH_REF_STATUS_MAX, REF_STATUS_RETRY_MS, THREAD_ENTRY_CAP, TIMELINE_BUDGET_MS,
    TIMELINE_EVENT_CAP, TIMELINE_MAX_PAGES, TIMELINE_MIN_PAGE_MS, batch_recheck_after, cap_body,
    derive_pr_reference_status, fetch_comment_counts, fetch_timeline_pages, first_line,
    has_resolved_repository, map_check_state, merge_thread, normalize_comments, normalize_events,
    normalize_reviews, parse_owner_name, ref_status_ttl, rollup_to_checks, sanitize_ref_numbers,
    tail,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub not_found: bool,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, binary: &str, cwd: &Path, args: &[String], timeout: Duration) -> CommandOutput;
}

#[derive(Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, binary: &str, cwd: &Path, args: &[String], _timeout: Duration) -> CommandOutput {
        let executable = if binary == "gh" {
            std::env::var("DUCK_GH_BIN")
                .ok()
                .filter(|path| !path.trim().is_empty())
                .unwrap_or_else(|| binary.to_owned())
        } else {
            binary.to_owned()
        };
        match Command::new(executable)
            .current_dir(cwd)
            .args(args)
            .output()
        {
            Ok(output) => CommandOutput {
                ok: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                not_found: false,
            },
            Err(error) => CommandOutput {
                ok: false,
                stdout: String::new(),
                stderr: error.to_string(),
                not_found: error.kind() == std::io::ErrorKind::NotFound,
            },
        }
    }
}

#[derive(Debug, Clone)]
struct CacheEntry<T> {
    at: u64,
    value: T,
}

#[derive(Debug, Clone)]
struct ListCache {
    at: u64,
    limit: usize,
    data: GithubData,
}

#[derive(Debug, Clone)]
struct RefCacheEntry {
    at: u64,
    resolved: Option<crate::model::ResolvedReference>,
}

type RepoHandleCache = Option<Option<(String, String)>>;

#[derive(Debug, Clone)]
struct MergePolicy {
    allow_merge_commit: bool,
    allow_squash_merge: bool,
    allow_rebase_merge: bool,
    merge_commit_title: Option<String>,
    squash_merge_commit_title: Option<String>,
}

#[derive(Debug, Clone)]
struct MergeRawCheck {
    name: String,
    state: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    details_url: Option<String>,
}

#[derive(Debug, Clone)]
struct MergeRawPr {
    number: u64,
    title: String,
    url: String,
    state: String,
    is_draft: bool,
    head_ref: String,
    base_ref: String,
    head_sha: String,
    mergeable: Option<String>,
    merge_state_status: Option<String>,
    review_decision: Option<String>,
    checks: Vec<MergeRawCheck>,
}

#[derive(Clone)]
pub struct GithubDriver {
    repo_root: PathBuf,
    repo_ref: Option<GithubRepoRef>,
    runner: Arc<dyn CommandRunner>,
    list_cache: Arc<Mutex<Option<ListCache>>>,
    comments_cache: Arc<Mutex<HashMap<String, CacheEntry<GithubCommentsData>>>>,
    checks_cache: Arc<Mutex<HashMap<u64, CacheEntry<Option<ChecksGlyph>>>>>,
    ref_cache: Arc<Mutex<HashMap<u64, RefCacheEntry>>>,
    repo_handle_cache: Arc<Mutex<RepoHandleCache>>,
    detect_cache: Arc<Mutex<Option<CacheEntry<ForgeAvailability>>>>,
    merge_cache: Arc<Mutex<HashMap<u64, CacheEntry<ForgePrMergeStateResult>>>>,
    merge_inflight: Arc<Mutex<std::collections::BTreeSet<u64>>>,
}

impl GithubDriver {
    pub fn new(repo_root: impl Into<PathBuf>, repo_ref: Option<GithubRepoRef>) -> Self {
        Self::with_runner(repo_root, repo_ref, Arc::new(SystemCommandRunner))
    }

    pub fn with_runner(
        repo_root: impl Into<PathBuf>,
        repo_ref: Option<GithubRepoRef>,
        runner: Arc<dyn CommandRunner>,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            repo_ref,
            runner,
            list_cache: Arc::new(Mutex::new(None)),
            comments_cache: Arc::new(Mutex::new(HashMap::new())),
            checks_cache: Arc::new(Mutex::new(HashMap::new())),
            ref_cache: Arc::new(Mutex::new(HashMap::new())),
            repo_handle_cache: Arc::new(Mutex::new(None)),
            detect_cache: Arc::new(Mutex::new(None)),
            merge_cache: Arc::new(Mutex::new(HashMap::new())),
            merge_inflight: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn repo_ref(&self) -> Option<&GithubRepoRef> {
        self.repo_ref.as_ref()
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64)
    }

    fn dry_run() -> bool {
        std::env::var("DUCK_DRY_RUN").ok().as_deref() == Some("1")
    }

    fn run(&self, binary: &str, args: &[impl AsRef<str>], timeout: Duration) -> CommandOutput {
        self.run_at(binary, &self.repo_root, args, timeout)
    }

    fn run_at(
        &self,
        binary: &str,
        cwd: &Path,
        args: &[impl AsRef<str>],
        timeout: Duration,
    ) -> CommandOutput {
        let args: Vec<String> = args.iter().map(|arg| arg.as_ref().to_owned()).collect();
        self.runner.run(binary, cwd, &args, timeout)
    }

    fn run_gh(&self, args: &[impl AsRef<str>], timeout: Duration) -> Result<String, String> {
        let result = self.run("gh", args, timeout);
        if result.ok {
            Ok(result.stdout)
        } else {
            Err(if result.not_found {
                "gh CLI not found — install it and run `gh auth login`".to_owned()
            } else {
                first_line(if result.stderr.is_empty() {
                    &result.stdout
                } else {
                    &result.stderr
                })
            })
        }
    }

    fn graphql(
        &self,
        query: &str,
        variables: &GraphqlVariables,
        timeout: Duration,
    ) -> Result<String, String> {
        let mut args = vec![
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            format!("query={query}"),
        ];
        for (key, value) in variables {
            args.push("-f".to_owned());
            args.push(format!("{key}={value}"));
        }
        let result = self.run("gh", &args, timeout);
        if result.ok {
            return Ok(result.stdout);
        }
        if has_resolved_repository(&result.stdout) {
            return Ok(result.stdout);
        }
        Err(if result.not_found {
            "gh CLI not found — install it and run `gh auth login`".to_owned()
        } else {
            first_line(if result.stderr.is_empty() {
                &result.stdout
            } else {
                &result.stderr
            })
        })
    }

    pub fn detect(&self) -> ForgeAvailability {
        if Self::dry_run() {
            return ForgeAvailability::available();
        }
        let now = Self::now_ms();
        if let Some(entry) = self
            .detect_cache
            .lock()
            .expect("detect cache poisoned")
            .as_ref()
            && now.saturating_sub(entry.at) < CACHE_MS
        {
            return entry.value.clone();
        }
        let value = match self.run(
            "gh",
            &["repo", "view", "--json", "nameWithOwner"],
            Duration::from_secs(5),
        ) {
            result if result.ok => ForgeAvailability::available(),
            result if result.not_found => ForgeAvailability::unavailable(
                "gh CLI not found — install it and run `gh auth login`",
            ),
            result => ForgeAvailability::unavailable(first_line(if result.stderr.is_empty() {
                &result.stdout
            } else {
                &result.stderr
            })),
        };
        *self.detect_cache.lock().expect("detect cache poisoned") = Some(CacheEntry {
            at: now,
            value: value.clone(),
        });
        value
    }

    pub fn detect_cached(&self) -> Option<ForgeAvailability> {
        if Self::dry_run() {
            return Some(ForgeAvailability::available());
        }
        let cached = self
            .detect_cache
            .lock()
            .expect("detect cache poisoned")
            .clone();
        if cached
            .as_ref()
            .is_none_or(|entry| Self::now_ms().saturating_sub(entry.at) >= CACHE_MS)
        {
            let driver = self.clone();
            std::thread::spawn(move || {
                let _ = driver.detect();
            });
        }
        cached.map(|entry| entry.value)
    }

    pub fn list(&self, refresh: bool, limit: usize) -> GithubData {
        if Self::dry_run() {
            return mock_github();
        }
        let limit = limit.clamp(1, GH_MAX_LIMIT);
        let now = Self::now_ms();
        if !refresh
            && let Some(cache) = self
                .list_cache
                .lock()
                .expect("list cache poisoned")
                .as_ref()
            && now.saturating_sub(cache.at) < CACHE_MS
            && cache.limit >= limit
        {
            return cache.data.clone();
        }
        let result = self.list_uncached(limit);
        if result.available {
            *self.list_cache.lock().expect("list cache poisoned") = Some(ListCache {
                at: Self::now_ms(),
                limit,
                data: result.clone(),
            });
        }
        result
    }

    fn list_uncached(&self, limit: usize) -> GithubData {
        let repo_output = match self.run_gh(
            &[
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "--jq",
                ".nameWithOwner",
            ],
            if limit > 100 {
                Duration::from_secs(30)
            } else {
                Duration::from_secs(15)
            },
        ) {
            Ok(output) => output,
            Err(reason) => return unavailable_data(reason),
        };
        let repo = parse_owner_name(&repo_output);
        let limit_text = limit.to_string();
        let issue = self.run(
            "gh",
            &[
                "issue",
                "list",
                "--limit",
                &limit_text,
                "--json",
                "number,title,author,createdAt,labels,body,url",
            ],
            Duration::from_secs(15),
        );
        let pr = self.run(
            "gh",
            &[
                "pr",
                "list",
                "--limit",
                &limit_text,
                "--json",
                "number,title,author,createdAt,labels,body,url,isDraft,additions,deletions",
            ],
            Duration::from_secs(15),
        );
        if !issue.ok && !pr.ok {
            return unavailable_data(first_line(if issue.stderr.is_empty() {
                &pr.stderr
            } else {
                &issue.stderr
            }));
        }
        let (issue_counts, pr_counts) = repo.as_ref().map_or_else(
            || (BTreeMap::new(), BTreeMap::new()),
            |(owner, name)| {
                fetch_comment_counts(
                    |query, variables| self.graphql(query, variables, Duration::from_secs(15)),
                    owner,
                    name,
                    ((limit as f64 / 100.0).ceil() as usize).clamp(1, 10),
                )
            },
        );
        let mut label_colors = BTreeMap::new();
        let issues = if issue.ok {
            parse_items(
                &issue.stdout,
                GithubItemKind::Issue,
                &issue_counts,
                &mut label_colors,
            )
            .unwrap_or_default()
        } else {
            Vec::new()
        };
        let prs = if pr.ok {
            parse_items(
                &pr.stdout,
                GithubItemKind::Pr,
                &pr_counts,
                &mut label_colors,
            )
            .unwrap_or_default()
        } else {
            Vec::new()
        };
        GithubData {
            available: true,
            reason: None,
            repo: Some(repo_output.trim().to_owned()),
            synced_at: Some(now_iso()),
            issues,
            prs,
            label_colors: (!label_colors.is_empty()).then_some(label_colors),
        }
    }

    pub fn list_issues(&self, refresh: bool, limit: usize) -> Vec<GithubItem> {
        self.list(refresh, limit).issues
    }

    pub fn list_prs(&self, refresh: bool, limit: usize) -> Vec<GithubItem> {
        self.list(refresh, limit).prs
    }

    pub fn view_url(&self, kind: ForgeRefKind, reference: impl AsRef<str>) -> Option<String> {
        let repo = self.repo_ref.as_ref()?;
        let base = format!("https://github.com/{}/{}", repo.owner, repo.repo);
        let path = reference
            .as_ref()
            .split('/')
            .map(url_encode_segment)
            .collect::<Vec<_>>()
            .join("/");
        Some(match kind {
            ForgeRefKind::Repo => base,
            ForgeRefKind::Issue => format!("{base}/issues/{path}"),
            ForgeRefKind::Pr => format!("{base}/pull/{path}"),
            ForgeRefKind::Branch => format!("{base}/tree/{path}"),
            ForgeRefKind::Commit => format!("{base}/commit/{path}"),
        })
    }

    pub fn resolve_repo_handle(&self) -> Option<(String, String)> {
        if let Some(cached) = self
            .repo_handle_cache
            .lock()
            .expect("repo cache poisoned")
            .clone()
        {
            return cached;
        }
        let Ok(value) = self.run_gh(
            &[
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "--jq",
                ".nameWithOwner",
            ],
            Duration::from_secs(15),
        ) else {
            return None;
        };
        let handle = parse_owner_name(&value);
        *self.repo_handle_cache.lock().expect("repo cache poisoned") = Some(handle.clone());
        handle
    }

    fn resolve_repo_handle_strict(&self) -> Result<Option<(String, String)>, String> {
        if let Some(cached) = self
            .repo_handle_cache
            .lock()
            .expect("repo cache poisoned")
            .clone()
        {
            return Ok(cached);
        }
        let value = self.run_gh(
            &[
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "--jq",
                ".nameWithOwner",
            ],
            Duration::from_secs(15),
        )?;
        let handle = parse_owner_name(&value);
        *self.repo_handle_cache.lock().expect("repo cache poisoned") = Some(handle.clone());
        Ok(handle)
    }

    pub fn clear_caches(&self) {
        *self.list_cache.lock().expect("list cache poisoned") = None;
        self.comments_cache
            .lock()
            .expect("comments cache poisoned")
            .clear();
        self.checks_cache
            .lock()
            .expect("checks cache poisoned")
            .clear();
        self.ref_cache.lock().expect("ref cache poisoned").clear();
        self.merge_cache
            .lock()
            .expect("merge cache poisoned")
            .clear();
    }

    pub fn comments(&self, kind: GithubItemKind, number: u64, refresh: bool) -> GithubCommentsData {
        if Self::dry_run() {
            return mock_comments(kind);
        }
        let key = format!(
            "{}\0{}#{number}",
            self.repo_root.display(),
            if kind == GithubItemKind::Pr {
                "pr"
            } else {
                "issue"
            }
        );
        let now = Self::now_ms();
        if !refresh
            && let Some(entry) = self
                .comments_cache
                .lock()
                .expect("comments cache poisoned")
                .get(&key)
            && now.saturating_sub(entry.at) < CACHE_MS
        {
            return entry.value.clone();
        }
        let timeline = fetch_timeline_pages(
            |page, remaining| {
                self.run_gh(
                    &[
                        "api",
                        "-H",
                        "Accept: application/vnd.github+json",
                        &format!("repos/{{owner}}/{{repo}}/issues/{number}/timeline?per_page=100&page={page}"),
                    ],
                    Duration::from_millis(remaining),
                )
            },
            TIMELINE_MAX_PAGES,
            TIMELINE_BUDGET_MS,
            TIMELINE_MIN_PAGE_MS,
            Self::now_ms,
        );
        let (comment_rows, events, events_truncated, stopped_short) = match timeline {
            Ok(pages) => {
                let comments = pages
                    .rows
                    .iter()
                    .filter(|row| row.get("event").and_then(Value::as_str) == Some("commented"))
                    .cloned()
                    .collect::<Vec<_>>();
                let (events, truncated) =
                    normalize_events(&Value::Array(pages.rows), TIMELINE_EVENT_CAP)
                        .unwrap_or_default();
                (comments, events, truncated, pages.stopped_short)
            }
            Err(_) => {
                let raw = match self.run_gh(
                    &[
                        "api",
                        &format!("repos/{{owner}}/{{repo}}/issues/{number}/comments"),
                        "--paginate",
                    ],
                    Duration::from_secs(15),
                ) {
                    Ok(raw) => raw,
                    Err(error) => return unavailable_comments(error),
                };
                let rows = match serde_json::from_str::<Value>(&raw) {
                    Ok(value) => value.as_array().cloned().unwrap_or_default(),
                    Err(error) => return unavailable_comments(error.to_string()),
                };
                (rows, Vec::new(), false, false)
            }
        };
        let mut comments = match normalize_comments(&Value::Array(comment_rows.clone())) {
            Ok(comments) => comments,
            Err(error) => return unavailable_comments(error),
        };
        if stopped_short
            && comment_rows.len() < THREAD_ENTRY_CAP
            && let Ok(raw) = self.run_gh(
                &[
                    "api",
                    &format!("repos/{{owner}}/{{repo}}/issues/{number}/comments"),
                    "--paginate",
                ],
                Duration::from_secs(15),
            )
            && let Ok(value) = serde_json::from_str::<Value>(&raw)
            && let Some(rows) = value.as_array()
        {
            comments = normalize_comments(&Value::Array(rows.clone())).unwrap_or(comments);
        }
        let mut events = events;
        let shas = events
            .iter()
            .filter_map(|event| event.sha.clone())
            .collect::<Vec<_>>();
        if !shas.is_empty()
            && let Some((owner, repo)) = self.resolve_repo_handle()
        {
            let checks = fetch_commit_checks(
                |query, variables| self.graphql(query, variables, Duration::from_secs(15)),
                &owner,
                &repo,
                &shas,
                crate::normalize::COMMIT_CHECKS_CHUNK,
            );
            for event in &mut events {
                if let Some(sha) = &event.sha
                    && let Some(checks) = checks.get(sha)
                {
                    event.checks = Some(*checks);
                }
            }
        }
        if kind == GithubItemKind::Pr {
            let raw = match self.run_gh(
                &[
                    "api",
                    &format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews"),
                    "--paginate",
                ],
                Duration::from_secs(15),
            ) {
                Ok(raw) => raw,
                Err(error) => return unavailable_comments(error),
            };
            if let Ok(value) = serde_json::from_str::<Value>(&raw)
                && let Ok(reviews) = normalize_reviews(&value)
            {
                let (merged, review_truncated) =
                    merge_thread(vec![comments, reviews], THREAD_ENTRY_CAP);
                comments = merged;
                let data = GithubCommentsData {
                    available: true,
                    reason: None,
                    comments,
                    truncated: (review_truncated || events_truncated || stopped_short)
                        .then_some(true),
                    events: Some(events),
                };
                self.store_comments(key, data.clone());
                return data;
            }
            return unavailable_comments("GitHub returned malformed reviews".to_owned());
        }
        let (comments, comment_truncated) = merge_thread(vec![comments], THREAD_ENTRY_CAP);
        let data = GithubCommentsData {
            available: true,
            reason: None,
            comments,
            truncated: (comment_truncated || events_truncated || stopped_short).then_some(true),
            events: Some(events),
        };
        self.store_comments(key, data.clone());
        data
    }

    fn store_comments(&self, key: String, data: GithubCommentsData) {
        let mut cache = self.comments_cache.lock().expect("comments cache poisoned");
        cache.insert(
            key,
            CacheEntry {
                at: Self::now_ms(),
                value: data,
            },
        );
        while cache.len() > 50 {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                cache.remove(&key);
            } else {
                break;
            }
        }
    }

    pub fn checks(&self, numbers: &[u64]) -> Result<BTreeMap<u64, Option<ChecksGlyph>>, String> {
        if Self::dry_run() {
            let data = mock_github();
            let mut output = BTreeMap::new();
            for number in sanitize_ref_numbers(numbers, GH_CHECKS_MAX) {
                output.insert(
                    number,
                    data.prs
                        .iter()
                        .find(|item| item.number == number)
                        .and_then(|item| item.checks.flatten()),
                );
            }
            return Ok(output);
        }
        let wanted = sanitize_ref_numbers(numbers, GH_CHECKS_MAX);
        let now = Self::now_ms();
        let mut output = BTreeMap::new();
        let mut misses = Vec::new();
        {
            let cache = self.checks_cache.lock().expect("checks cache poisoned");
            for number in wanted {
                if let Some(entry) = cache.get(&number)
                    && now.saturating_sub(entry.at) < CACHE_MS
                {
                    output.insert(number, entry.value);
                } else {
                    misses.push(number);
                }
            }
        }
        if misses.is_empty() {
            return Ok(output);
        }
        let Some((owner, repo)) = self.resolve_repo_handle() else {
            return Err("repository handle unavailable".to_owned());
        };
        let fetched = fetch_pr_checks(
            |query, variables| self.graphql(query, variables, Duration::from_secs(15)),
            &owner,
            &repo,
            &misses,
            GH_CHECKS_MAX,
        );
        let mut cache = self.checks_cache.lock().expect("checks cache poisoned");
        for number in misses {
            let value = fetched.get(&number).copied().unwrap_or(None);
            output.insert(number, value);
            cache.insert(number, CacheEntry { at: now, value });
        }
        while cache.len() > 500 {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(number, _)| *number);
            if let Some(number) = oldest {
                cache.remove(&number);
            } else {
                break;
            }
        }
        Ok(output)
    }

    pub fn ref_status(&self, prs: &[u64], issues: &[u64]) -> crate::model::GithubRefStatusData {
        if Self::dry_run() {
            return mock_ref_status(prs, issues);
        }
        let wanted = sanitize_ref_numbers(
            &prs.iter().chain(issues).copied().collect::<Vec<_>>(),
            GH_REF_STATUS_MAX,
        );
        let now = Self::now_ms();
        let mut resolved = BTreeMap::new();
        let mut entries = Vec::new();
        let mut misses = Vec::new();
        {
            let cache = self.ref_cache.lock().expect("ref cache poisoned");
            for number in &wanted {
                if let Some(entry) = cache.get(number)
                    && now.saturating_sub(entry.at) < ref_status_ttl(entry.resolved.as_ref())
                {
                    entries.push(entry.resolved.clone());
                    if let Some(value) = &entry.resolved {
                        resolved.insert(*number, value.clone());
                    }
                } else {
                    misses.push(*number);
                }
            }
        }
        if misses.is_empty() {
            return ref_data_from_resolved(&resolved, batch_recheck_after(&entries));
        }
        let (owner, repo) = match self.resolve_repo_handle_strict() {
            Ok(Some(value)) => value,
            Ok(None) => {
                return crate::model::GithubRefStatusData {
                    available: false,
                    reason: Some("repository handle unavailable".to_owned()),
                    prs: BTreeMap::new(),
                    issues: BTreeMap::new(),
                    recheck_after_ms: Some(REF_STATUS_RETRY_MS),
                };
            }
            Err(reason) => {
                return crate::model::GithubRefStatusData {
                    available: false,
                    reason: Some(reason),
                    prs: BTreeMap::new(),
                    issues: BTreeMap::new(),
                    recheck_after_ms: Some(REF_STATUS_RETRY_MS),
                };
            }
        };
        let batch = fetch_ref_statuses(
            |query, variables| self.graphql(query, variables, Duration::from_secs(15)),
            &owner,
            &repo,
            &misses,
            GH_REF_STATUS_MAX,
        );
        let failed = batch
            .failed
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let stored_at = Self::now_ms();
        let mut cache = self.ref_cache.lock().expect("ref cache poisoned");
        for number in misses {
            if failed.contains(&number) {
                continue;
            }
            let value = batch.resolved.get(&number).cloned();
            entries.push(value.clone());
            if let Some(value) = &value {
                resolved.insert(number, value.clone());
            }
            cache.insert(
                number,
                RefCacheEntry {
                    at: stored_at,
                    resolved: value,
                },
            );
        }
        while cache.len() > 500 {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(number, _)| *number);
            if let Some(number) = oldest {
                cache.remove(&number);
            } else {
                break;
            }
        }
        if !batch.failed.is_empty() {
            return crate::model::GithubRefStatusData {
                available: false,
                reason: batch.reason,
                prs: BTreeMap::new(),
                issues: BTreeMap::new(),
                recheck_after_ms: Some(REF_STATUS_RETRY_MS),
            };
        }
        ref_data_from_resolved(&resolved, batch_recheck_after(&entries))
    }

    pub fn forget_ref_status(&self, number: u64) {
        self.ref_cache
            .lock()
            .expect("ref cache poisoned")
            .remove(&number);
    }

    pub fn read_cached_ref_statuses(
        &self,
        numbers: &[u64],
    ) -> (
        BTreeMap<u64, ReferenceStatus>,
        BTreeMap<u64, ReferenceStatus>,
    ) {
        let now = Self::now_ms();
        let cache = self.ref_cache.lock().expect("ref cache poisoned");
        let mut prs = BTreeMap::new();
        let mut issues = BTreeMap::new();
        for number in numbers.iter().copied() {
            let Some(entry) = cache.get(&number) else {
                continue;
            };
            if now.saturating_sub(entry.at) >= ref_status_ttl(entry.resolved.as_ref()) {
                continue;
            }
            let Some(value) = &entry.resolved else {
                continue;
            };
            match value.kind {
                crate::model::ReferenceKind::Pr => {
                    prs.insert(number, value.status);
                }
                crate::model::ReferenceKind::Issue => {
                    issues.insert(number, value.status);
                }
            }
        }
        (prs, issues)
    }

    pub fn pr_status(&self, branch: &str) -> Option<ForgePrStatus> {
        if Self::dry_run() {
            return None;
        }
        let raw = self
            .run_gh(
                &[
                    "pr",
                    "view",
                    branch,
                    "--json",
                    "number,url,state,isDraft,statusCheckRollup",
                ],
                Duration::from_secs(15),
            )
            .ok()?;
        let value: Value = serde_json::from_str(&raw).ok()?;
        let checks = parse_check_runs(value.get("statusCheckRollup")).ok()?;
        Some(ForgePrStatus {
            number: value.get("number")?.as_u64()?,
            url: value.get("url")?.as_str()?.to_owned(),
            state: match value
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("OPEN")
                .to_ascii_uppercase()
                .as_str()
            {
                "MERGED" => GithubPrState::Merged,
                "CLOSED" => GithubPrState::Closed,
                _ => GithubPrState::Open,
            },
            is_draft: value
                .get("isDraft")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            checks: rollup_to_checks(Some(&checks)),
        })
    }

    pub fn pr_diff(&self, number: u64, _refresh: bool) -> ForgePrDiffResult {
        if Self::dry_run() {
            return ForgePrDiffResult::Available {
                number,
                head_sha: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                files: vec![GithubPrChange {
                    path: "src/session.ts".to_owned(),
                    previous_path: None,
                    status: GithubChangeStatus::Modified,
                    additions: 8,
                    deletions: 3,
                    patch: Some("@@ -1 +1 @@\n-old\n+new".to_owned()),
                    patch_unavailable_reason: None,
                    truncated: false,
                }],
                additions: 8,
                deletions: 3,
                truncated: false,
                reason: None,
            };
        }
        let number_text = number.to_string();
        let head_raw = match self.run_gh(
            &["pr", "view", &number_text, "--json", "headRefOid"],
            Duration::from_secs(15),
        ) {
            Ok(raw) => raw,
            Err(reason) => return ForgePrDiffResult::Unavailable { reason },
        };
        let head_sha = match serde_json::from_str::<Value>(&head_raw)
            .ok()
            .and_then(|value| {
                value
                    .get("headRefOid")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }) {
            Some(sha) => sha,
            None => {
                return ForgePrDiffResult::Unavailable {
                    reason: "GitHub returned no pull request head".to_owned(),
                };
            }
        };
        let mut rows = Vec::new();
        for page in 1..=3 {
            let raw = match self.run_gh(
                &[
                    "api",
                    &format!(
                        "repos/{{owner}}/{{repo}}/pulls/{number}/files?per_page=100&page={page}"
                    ),
                ],
                Duration::from_secs(30),
            ) {
                Ok(raw) => raw,
                Err(reason) => return ForgePrDiffResult::Unavailable { reason },
            };
            let page_rows = match serde_json::from_str::<Vec<Value>>(&raw) {
                Ok(rows) => rows,
                Err(error) => {
                    return ForgePrDiffResult::Unavailable {
                        reason: error.to_string(),
                    };
                }
            };
            let short = page_rows.len() < 100;
            rows.extend(page_rows);
            if short {
                break;
            }
        }
        let response_truncated = rows.len() >= GH_PR_DIFF_FILE_CAP;
        let mut reasons = Vec::new();
        if response_truncated {
            reasons.push(format!(
                "Only the first {GH_PR_DIFF_FILE_CAP} files are shown."
            ));
        }
        let mut files = Vec::new();
        let mut size_truncated = false;
        for row in rows.iter().take(GH_PR_DIFF_FILE_CAP) {
            let Some(path) = row.get("filename").and_then(Value::as_str) else {
                continue;
            };
            let status = match row
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("changed")
            {
                "added" => GithubChangeStatus::Added,
                "modified" => GithubChangeStatus::Modified,
                "removed" => GithubChangeStatus::Removed,
                "renamed" => GithubChangeStatus::Renamed,
                "copied" => GithubChangeStatus::Copied,
                _ => GithubChangeStatus::Changed,
            };
            let additions = row.get("additions").and_then(Value::as_u64).unwrap_or(0);
            let deletions = row.get("deletions").and_then(Value::as_u64).unwrap_or(0);
            let mut patch = row.get("patch").and_then(Value::as_str).map(str::to_owned);
            let mut patch_unavailable_reason = None;
            let mut truncated = false;
            if patch
                .as_ref()
                .is_some_and(|patch| patch.len() > GH_PR_PATCH_CAP)
            {
                patch = None;
                patch_unavailable_reason =
                    Some(coducktor_contract::GithubPatchUnavailableReason::TooLarge);
                truncated = true;
                reasons.push("One or more patches exceeded the per-file limit.".to_owned());
            } else if patch.is_none() {
                patch_unavailable_reason = Some(if additions == 0 && deletions == 0 {
                    coducktor_contract::GithubPatchUnavailableReason::Binary
                } else {
                    coducktor_contract::GithubPatchUnavailableReason::NotProvided
                });
            }
            files.push(GithubPrChange {
                path: path.to_owned(),
                previous_path: row
                    .get("previous_filename")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                status,
                additions,
                deletions,
                patch,
                patch_unavailable_reason,
                truncated,
            });
        }
        let additions = rows
            .iter()
            .map(|row| row.get("additions").and_then(Value::as_u64).unwrap_or(0))
            .sum();
        let deletions = rows
            .iter()
            .map(|row| row.get("deletions").and_then(Value::as_u64).unwrap_or(0))
            .sum();
        while serde_json::to_vec(&files).map_or(true, |bytes| bytes.len() > GH_PR_DIFF_JSON_CAP)
            && !files.is_empty()
        {
            files.pop();
            size_truncated = true;
        }
        if size_truncated {
            reasons.push("The response size limit omitted some files.".to_owned());
        }
        let file_truncated = files.iter().any(|file| file.truncated);
        ForgePrDiffResult::Available {
            number,
            head_sha,
            files,
            additions,
            deletions,
            truncated: response_truncated || size_truncated || file_truncated,
            reason: (!reasons.is_empty()).then(|| reasons.join(" ")),
        }
    }

    pub fn pr_merge_state(&self, number: u64, refresh: bool) -> ForgePrMergeStateResult {
        if Self::dry_run() {
            return normalize_merge_state(
                &json!({
                    "number": number,
                    "title": "Dry-run pull request",
                    "url": format!("https://github.com/mock/repo/pull/{number}"),
                    "state": "OPEN",
                    "isDraft": false,
                    "headRefName": "feat/dry-run",
                    "baseRefName": "main",
                    "headRefOid": "0123456789abcdef0123456789abcdef01234567",
                    "mergeable": "MERGEABLE",
                    "mergeStateStatus": "CLEAN",
                    "reviewDecision": "APPROVED",
                    "statusCheckRollup": [{"name":"test","conclusion":"SUCCESS","detailsUrl":"https://github.com/mock/repo/actions"}]
                }),
                &json!({"allow_merge_commit":true,"allow_squash_merge":true,"allow_rebase_merge":true}),
                true,
                &["test".to_owned()],
            )
            .map_or_else(|error| ForgePrMergeStateResult::Unavailable { reason: error }, ForgePrMergeStateResult::Available);
        }
        let now = Self::now_ms();
        if !refresh
            && let Some(entry) = self
                .merge_cache
                .lock()
                .expect("merge cache poisoned")
                .get(&number)
            && now.saturating_sub(entry.at) < 15_000
        {
            return entry.value.clone();
        }
        let value = (|| {
            let number_text = number.to_string();
            let raw = self.run_gh(&["pr", "view", &number_text, "--json", "number,title,url,state,isDraft,headRefName,baseRefName,headRefOid,mergeable,mergeStateStatus,reviewDecision,statusCheckRollup"], Duration::from_secs(15))?;
            let pr: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
            let repo = self
                .repo_ref
                .as_ref()
                .ok_or_else(|| "GitHub remote could not be resolved".to_owned())?;
            let policy_raw = self.run_gh(
                &["api", &format!("repos/{}/{}", repo.owner, repo.repo)],
                Duration::from_secs(15),
            )?;
            let required_raw = self.run_gh(
                &[
                    "api",
                    &format!(
                        "repos/{}/{}/branches/{}/protection/required_status_checks",
                        repo.owner,
                        repo.repo,
                        url_encode_segment(
                            pr.get("baseRefName")
                                .and_then(Value::as_str)
                                .unwrap_or("main")
                        )
                    ),
                    "--jq",
                    "[.contexts[]?, .checks[]?.context] | unique",
                ],
                Duration::from_secs(15),
            );
            let (readable, required) = required_raw
                .ok()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
                .map_or((false, Vec::new()), |required| (true, required));
            normalize_merge_state(
                &pr,
                &serde_json::from_str(&policy_raw).map_err(|error| error.to_string())?,
                readable,
                &required,
            )
        })();
        let result = match value {
            Ok(value) => ForgePrMergeStateResult::Available(value),
            Err(reason) => ForgePrMergeStateResult::Unavailable {
                reason: first_line(&reason),
            },
        };
        self.merge_cache
            .lock()
            .expect("merge cache poisoned")
            .insert(
                number,
                CacheEntry {
                    at: Self::now_ms(),
                    value: result.clone(),
                },
            );
        result
    }

    pub fn merge_pr(&self, number: u64, input: &ForgeMergeInput) -> ForgeMergeResult {
        {
            let mut inflight = self.merge_inflight.lock().expect("merge lock poisoned");
            if !inflight.insert(number) {
                return ForgeMergeResult::Rejected {
                    status: 409,
                    error: "A merge is already in progress.".to_owned(),
                    code: Some("concurrent".to_owned()),
                    current: None,
                };
            }
        }
        let outcome = self.merge_pr_inner(number, input);
        self.merge_inflight
            .lock()
            .expect("merge lock poisoned")
            .remove(&number);
        outcome
    }

    fn merge_pr_inner(&self, number: u64, input: &ForgeMergeInput) -> ForgeMergeResult {
        let state = match self.pr_merge_state(number, true) {
            ForgePrMergeStateResult::Available(state) => state,
            ForgePrMergeStateResult::Unavailable { reason } => {
                return ForgeMergeResult::Rejected {
                    status: 502,
                    error: reason,
                    code: None,
                    current: None,
                };
            }
        };
        if state.head_sha != input.expected_head_sha {
            return ForgeMergeResult::Rejected {
                status: 409,
                error: "The pull request head changed. Review the new commits before merging."
                    .to_owned(),
                code: Some("stale-head".to_owned()),
                current: Some(state),
            };
        }
        if !state.methods.contains(&input.method) {
            return ForgeMergeResult::Rejected {
                status: 409,
                error: "That merge method is no longer enabled.".to_owned(),
                code: Some("disabled-method".to_owned()),
                current: Some(state),
            };
        }
        if !merge_preflight_allowed(&state, input.override_rules) {
            return ForgeMergeResult::Rejected {
                status: 409,
                error: state.blockers.first().map_or_else(
                    || "The pull request is not eligible to merge.".to_owned(),
                    |blocker| blocker.message.clone(),
                ),
                code: Some(format!("{:?}", state.eligibility).to_ascii_lowercase()),
                current: Some(state),
            };
        }
        if Self::dry_run() {
            self.clear_caches();
            return ForgeMergeResult::Merged {
                number,
                url: state.url,
                method: input.method,
                merge_commit_sha: Some("abcdef0123456789abcdef0123456789abcdef01".to_owned()),
            };
        }
        let Some(repo) = self.repo_ref.as_ref() else {
            return ForgeMergeResult::Rejected {
                status: 404,
                error: "GitHub repository not found.".to_owned(),
                code: None,
                current: Some(state),
            };
        };
        let raw = self.run_gh(
            &[
                "api",
                "--method",
                "PUT",
                &format!("repos/{}/{}/pulls/{number}/merge", repo.owner, repo.repo),
                "-f",
                &format!("merge_method={}", input.method),
                "-f",
                &format!("sha={}", input.expected_head_sha),
            ],
            Duration::from_secs(15),
        );
        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                return ForgeMergeResult::Rejected {
                    status: if error.to_ascii_lowercase().contains("forbidden") {
                        403
                    } else if error.to_ascii_lowercase().contains("not found") {
                        404
                    } else {
                        502
                    },
                    error,
                    code: None,
                    current: Some(state),
                };
            }
        };
        let response: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(error) => {
                return ForgeMergeResult::Rejected {
                    status: 502,
                    error: error.to_string(),
                    code: None,
                    current: Some(state),
                };
            }
        };
        if !response
            .get("merged")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return ForgeMergeResult::Rejected {
                status: 409,
                error: response
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("GitHub refused the merge.")
                    .to_owned(),
                code: Some("github-blocked".to_owned()),
                current: Some(state),
            };
        }
        self.clear_caches();
        ForgeMergeResult::Merged {
            number,
            url: state.url,
            method: input.method,
            merge_commit_sha: response
                .get("sha")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }

    pub fn create_draft_pr(&self, input: &DraftPrInput) -> DraftPrOutcome {
        let Some(worktree) = input.run.worktree_path.as_deref() else {
            return DraftPrOutcome::Failed {
                error: "this task has no worktree/branch to publish".to_owned(),
            };
        };
        let Some(branch) = input.run.branch.as_deref() else {
            return DraftPrOutcome::Failed {
                error: "this task has no worktree/branch to publish".to_owned(),
            };
        };
        match autosave_commit(Path::new(worktree), AutosaveReason::PrePr) {
            AutosaveResult::Refused => {
                return DraftPrOutcome::Failed {
                    error:
                        "worktree has unresolved merge conflicts — resolve them, then publish again"
                            .to_owned(),
                };
            }
            AutosaveResult::Failed => {
                return DraftPrOutcome::Failed {
                    error: "could not commit the final changes — check git status in the worktree"
                        .to_owned(),
                };
            }
            AutosaveResult::Committed | AutosaveResult::NothingToDo => {}
        }
        if Self::dry_run() {
            return DraftPrOutcome::Created {
                url: "https://github.com/open-mercato/demo/pull/777".to_owned(),
                dry_run: true,
            };
        }
        let remote = self.run_at(
            "git",
            Path::new(worktree),
            &["remote", "get-url", "origin"],
            Duration::from_secs(30),
        );
        if !remote.ok || remote.stdout.trim().is_empty() {
            return DraftPrOutcome::Failed { error: "no git remote — add one (git remote add origin <url>) or merge the branch locally".to_owned() };
        }
        let push = self.run_at(
            "git",
            Path::new(worktree),
            &["push", "-u", "origin", branch],
            Duration::from_secs(60),
        );
        if !push.ok {
            return DraftPrOutcome::Failed {
                error: format!("git push failed — {}", tail(&push.stderr)),
            };
        }
        let mut args = vec![
            "pr".to_owned(),
            "create".to_owned(),
            "--draft".to_owned(),
            "--head".to_owned(),
            branch.to_owned(),
        ];
        if let Some(base) = input
            .run
            .base_branch
            .as_deref()
            .filter(|base| !is_sha(base))
        {
            args.push("--base".to_owned());
            args.push(base.strip_prefix("origin/").unwrap_or(base).to_owned());
        }
        args.extend([
            "--title".to_owned(),
            input.run.title.clone(),
            "--body".to_owned(),
            build_pr_body(&input.handoff_text, &input.run.task),
        ]);
        let pr = self.run_at("gh", Path::new(worktree), &args, Duration::from_secs(60));
        if !pr.ok {
            if pr.not_found {
                return DraftPrOutcome::Failed { error: "gh not found — install the GitHub CLI and run `gh auth login`, or merge the branch locally".to_owned() };
            }
            let hint = if pr.stderr.to_ascii_lowercase().contains("auth")
                || pr.stderr.to_ascii_lowercase().contains("login")
                || pr.stderr.to_ascii_lowercase().contains("credential")
            {
                " (try `gh auth login`)"
            } else {
                ""
            };
            return DraftPrOutcome::Failed {
                error: format!("gh pr create failed — {}{}", tail(&pr.stderr), hint),
            };
        }
        extract_pr_url(&format!("{}\n{}", pr.stdout, pr.stderr)).map_or_else(
            || DraftPrOutcome::Failed {
                error: "gh pr create returned no PR URL — check `gh pr list` manually".to_owned(),
            },
            |url| DraftPrOutcome::Created {
                url,
                dry_run: false,
            },
        )
    }
}

fn unavailable_data(reason: String) -> GithubData {
    GithubData {
        available: false,
        reason: Some(reason),
        repo: None,
        synced_at: None,
        issues: Vec::new(),
        prs: Vec::new(),
        label_colors: None,
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn url_encode_segment(value: &str) -> String {
    value.bytes().fold(String::new(), |mut output, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
        output
    })
}

fn mock_github() -> GithubData {
    let item = |kind: GithubItemKind, number: u64, title: &str, body: &str| GithubItem {
        kind,
        number,
        title: title.to_owned(),
        author: "mock".to_owned(),
        created_at: now_iso(),
        labels: Vec::new(),
        body: body.to_owned(),
        url: format!(
            "https://github.com/mock/repo/{}/{number}",
            if kind == GithubItemKind::Pr {
                "pull"
            } else {
                "issues"
            }
        ),
        comments: 0,
        is_draft: (kind == GithubItemKind::Pr).then_some(false),
        additions: (kind == GithubItemKind::Pr).then_some(15.0),
        deletions: (kind == GithubItemKind::Pr).then_some(4.0),
        checks: (kind == GithubItemKind::Pr).then_some(Some(ChecksGlyph::Passing)),
    };
    GithubData {
        available: true,
        reason: None,
        repo: Some("mock/repo".to_owned()),
        synced_at: Some(now_iso()),
        issues: vec![item(
            GithubItemKind::Issue,
            142,
            "Login form drops session on refresh",
            "Repro: reload after login.",
        )],
        prs: vec![item(
            GithubItemKind::Pr,
            128,
            "Fix flaky auth test in CI",
            "Loosens the timing assertion.",
        )],
        label_colors: None,
    }
}

fn parse_items(
    raw: &str,
    kind: GithubItemKind,
    counts: &BTreeMap<u64, u64>,
    colors: &mut BTreeMap<String, String>,
) -> Result<Vec<GithubItem>, String> {
    let rows = serde_json::from_str::<Vec<Value>>(raw).map_err(|err| err.to_string())?;
    rows.into_iter()
        .map(|row| {
            let number = row
                .get("number")
                .and_then(Value::as_u64)
                .ok_or_else(|| "GitHub item has no number".to_owned())?;
            let labels = row
                .get("labels")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut label_names = Vec::new();
            for label in labels {
                if let Some(name) = label.get("name").and_then(Value::as_str) {
                    label_names.push(name.to_owned());
                    if let Some(color) = label
                        .get("color")
                        .and_then(Value::as_str)
                        .filter(|color| !color.is_empty())
                    {
                        colors
                            .entry(name.to_owned())
                            .or_insert_with(|| color.to_owned());
                    }
                }
            }
            let item = GithubItem {
                kind,
                number,
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                author: row
                    .pointer("/author/login")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_owned(),
                created_at: row
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                labels: label_names,
                body: cap_body(row.get("body").and_then(Value::as_str)),
                url: row
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                comments: counts.get(&number).copied().unwrap_or(0),
                is_draft: (kind == GithubItemKind::Pr)
                    .then(|| row.get("isDraft").and_then(Value::as_bool).unwrap_or(false)),
                additions: (kind == GithubItemKind::Pr)
                    .then(|| row.get("additions").and_then(Value::as_f64).unwrap_or(0.0)),
                deletions: (kind == GithubItemKind::Pr)
                    .then(|| row.get("deletions").and_then(Value::as_f64).unwrap_or(0.0)),
                checks: (kind == GithubItemKind::Pr).then_some(None),
            };
            Ok(item)
        })
        .collect()
}

fn parse_check_runs(value: Option<&Value>) -> Result<Vec<CheckRun>, String> {
    let Some(rows) = value else {
        return Ok(Vec::new());
    };
    let rows = rows
        .as_array()
        .ok_or_else(|| "statusCheckRollup is not an array".to_owned())?;
    Ok(rows
        .iter()
        .map(|row| CheckRun {
            state: row.get("state").and_then(Value::as_str).map(str::to_owned),
            status: row.get("status").and_then(Value::as_str).map(str::to_owned),
            conclusion: row
                .get("conclusion")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .collect())
}

fn unavailable_comments(reason: String) -> GithubCommentsData {
    GithubCommentsData {
        available: false,
        reason: Some(reason),
        comments: Vec::new(),
        truncated: None,
        events: None,
    }
}

fn ref_data_from_resolved(
    resolved: &BTreeMap<u64, crate::model::ResolvedReference>,
    recheck_after_ms: Option<u64>,
) -> crate::model::GithubRefStatusData {
    let mut prs = BTreeMap::new();
    let mut issues = BTreeMap::new();
    for (number, value) in resolved {
        match value.kind {
            crate::model::ReferenceKind::Pr => {
                prs.insert(*number, value.status);
            }
            crate::model::ReferenceKind::Issue => {
                issues.insert(*number, value.status);
            }
        }
    }
    crate::model::GithubRefStatusData {
        available: true,
        reason: None,
        prs,
        issues,
        recheck_after_ms,
    }
}

fn mock_comments(kind: GithubItemKind) -> GithubCommentsData {
    let now = now_iso();
    let mut comments = vec![GithubComment {
        id: 1,
        author: "ada".to_owned(),
        avatar_url: None,
        created_at: now.clone(),
        body: "Thanks for the report — I can reproduce.".to_owned(),
        kind: coducktor_contract::GithubCommentKind::Comment,
        review_state: None,
        url: "https://github.com/mock/repo/issues/1#issuecomment-1".to_owned(),
    }];
    if kind == GithubItemKind::Pr {
        comments.push(GithubComment {
            id: 2,
            author: "grace".to_owned(),
            avatar_url: None,
            created_at: now,
            body: "Please add a regression test.".to_owned(),
            kind: coducktor_contract::GithubCommentKind::Review,
            review_state: Some(coducktor_contract::GithubReviewState::ChangesRequested),
            url: "https://github.com/mock/repo/pull/1#pullrequestreview-2".to_owned(),
        });
    }
    GithubCommentsData {
        available: true,
        reason: None,
        comments,
        truncated: None,
        events: Some(Vec::new()),
    }
}

fn mock_ref_status(prs: &[u64], issues: &[u64]) -> crate::model::GithubRefStatusData {
    let catalog = mock_github();
    let mut pr_out = BTreeMap::new();
    let mut issue_out = BTreeMap::new();
    for number in prs {
        if let Some(item) = catalog.prs.iter().find(|item| item.number == *number) {
            let checks = item.checks.flatten();
            pr_out.insert(
                *number,
                derive_pr_reference_status(
                    "OPEN",
                    item.is_draft.unwrap_or(false),
                    None,
                    checks,
                    None,
                    None,
                    false,
                ),
            );
        }
    }
    for number in issues {
        if catalog.issues.iter().any(|item| item.number == *number) {
            issue_out.insert(*number, ReferenceStatus::Open);
        }
    }
    crate::model::GithubRefStatusData {
        available: true,
        reason: None,
        prs: pr_out,
        issues: issue_out,
        recheck_after_ms: Some(CACHE_MS),
    }
}

fn is_sha(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn extract_pr_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| {
            token.trim_matches(|char: char| {
                !char.is_ascii() || matches!(char, ')' | ']' | '}' | ',')
            })
        })
        .find(|token| {
            let Some(rest) = token.strip_prefix("https://github.com/") else {
                return false;
            };
            let pieces: Vec<&str> = rest.split('/').collect();
            pieces.len() >= 4 && pieces[2] == "pull" && pieces[3].parse::<u64>().is_ok()
        })
        .map(str::to_owned)
}

fn section(text: &str, header: &str) -> String {
    let Some(start) = text.find(&format!("{header}\n")) else {
        return String::new();
    };
    let rest = &text[start + header.len() + 1..];
    rest.find("\n## ").map_or_else(
        || rest.trim().to_owned(),
        |end| rest[..end].trim().to_owned(),
    )
}

pub fn build_pr_body(handoff_text: &str, task: &str) -> String {
    let goal = {
        let goal = section(handoff_text, "## Goal");
        if goal.is_empty() {
            task.trim().to_owned()
        } else {
            goal
        }
    };
    let progress = section(handoff_text, "## Progress log")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(10)
        .collect::<Vec<_>>()
        .join("\n");
    let mut parts = vec!["## Goal".to_owned(), String::new(), goal];
    if !progress.is_empty() {
        parts.extend([
            String::new(),
            "## Progress log".to_owned(),
            String::new(),
            progress,
        ]);
    }
    parts.extend([
        String::new(),
        "---".to_owned(),
        String::new(),
        "🤖 made with coducktor".to_owned(),
    ]);
    parts.join("\n")
}

fn parse_merge_raw(value: &Value) -> Result<MergeRawPr, String> {
    let checks = value
        .get("statusCheckRollup")
        .map(parse_merge_checks)
        .transpose()?
        .unwrap_or_default();
    Ok(MergeRawPr {
        number: value
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| "pull request has no number".to_owned())?,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        url: value
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        state: value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("OPEN")
            .to_owned(),
        is_draft: value
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        head_ref: value
            .get("headRefName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        base_ref: value
            .get("baseRefName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        head_sha: value
            .get("headRefOid")
            .and_then(Value::as_str)
            .ok_or_else(|| "pull request has no head sha".to_owned())?
            .to_owned(),
        mergeable: value
            .get("mergeable")
            .and_then(Value::as_str)
            .map(str::to_owned),
        merge_state_status: value
            .get("mergeStateStatus")
            .and_then(Value::as_str)
            .map(str::to_owned),
        review_decision: value
            .get("reviewDecision")
            .and_then(Value::as_str)
            .map(str::to_owned),
        checks,
    })
}

fn parse_merge_checks(value: &Value) -> Result<Vec<MergeRawCheck>, String> {
    let rows = value
        .as_array()
        .ok_or_else(|| "statusCheckRollup is not an array".to_owned())?;
    Ok(rows
        .iter()
        .map(|row| MergeRawCheck {
            name: row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Check")
                .to_owned(),
            state: row.get("state").and_then(Value::as_str).map(str::to_owned),
            status: row.get("status").and_then(Value::as_str).map(str::to_owned),
            conclusion: row
                .get("conclusion")
                .and_then(Value::as_str)
                .map(str::to_owned),
            details_url: row
                .get("detailsUrl")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .collect())
}

fn parse_merge_policy(value: &Value) -> MergePolicy {
    MergePolicy {
        allow_merge_commit: value
            .get("allow_merge_commit")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        allow_squash_merge: value
            .get("allow_squash_merge")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        allow_rebase_merge: value
            .get("allow_rebase_merge")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        merge_commit_title: value
            .get("merge_commit_title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        squash_merge_commit_title: value
            .get("squash_merge_commit_title")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

pub fn normalize_merge_state(
    raw: &Value,
    policy_raw: &Value,
    requirements_readable: bool,
    required_checks: &[String],
) -> Result<GithubPrMergeState, String> {
    let pr = parse_merge_raw(raw)?;
    let policy = parse_merge_policy(policy_raw);
    let state = match pr.state.to_ascii_uppercase().as_str() {
        "MERGED" => GithubPrState::Merged,
        "CLOSED" => GithubPrState::Closed,
        _ => GithubPrState::Open,
    };
    let mergeable = match pr
        .mergeable
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase()
        .as_str()
    {
        "MERGEABLE" => GithubMergeable::Mergeable,
        "CONFLICTING" => GithubMergeable::Conflicting,
        _ => GithubMergeable::Unknown,
    };
    let review_decision = match pr
        .review_decision
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase()
        .as_str()
    {
        "APPROVED" => GithubReviewDecision::Approved,
        "CHANGES_REQUESTED" => GithubReviewDecision::ChangesRequested,
        "REVIEW_REQUIRED" => GithubReviewDecision::ReviewRequired,
        _ => GithubReviewDecision::Unknown,
    };
    let checks = pr
        .checks
        .iter()
        .map(|check| GithubPrCheck {
            name: check.name.clone(),
            state: map_check_state(
                check
                    .conclusion
                    .as_deref()
                    .or(check.state.as_deref())
                    .or(check.status.as_deref()),
            ),
            required: requirements_readable.then(|| required_checks.contains(&check.name)),
            url: check
                .details_url
                .as_deref()
                .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                .map(str::to_owned),
        })
        .collect::<Vec<_>>();
    let mut methods = Vec::new();
    if policy.allow_squash_merge {
        methods.push(GithubMergeMethod::Squash);
    }
    if policy.allow_merge_commit {
        methods.push(GithubMergeMethod::Merge);
    }
    if policy.allow_rebase_merge {
        methods.push(GithubMergeMethod::Rebase);
    }
    let default_method = if policy.squash_merge_commit_title.is_some()
        && methods.contains(&GithubMergeMethod::Squash)
    {
        Some(GithubMergeMethod::Squash)
    } else if policy.merge_commit_title.is_some() && methods.contains(&GithubMergeMethod::Merge) {
        Some(GithubMergeMethod::Merge)
    } else {
        methods.first().copied()
    };
    let mut blockers = Vec::new();
    let eligibility;
    if state != GithubPrState::Open {
        eligibility = GithubMergeEligibility::Terminal;
        blockers.push(coducktor_contract::GithubBlocker {
            code: "terminal".to_owned(),
            message: if state == GithubPrState::Merged {
                "This pull request is merged."
            } else {
                "This pull request is closed."
            }
            .to_owned(),
        });
    } else if pr.is_draft {
        eligibility = GithubMergeEligibility::Blocked;
        blockers.push(coducktor_contract::GithubBlocker {
            code: "draft".to_owned(),
            message: "Mark the pull request ready for review before merging.".to_owned(),
        });
    } else if mergeable == GithubMergeable::Conflicting {
        eligibility = GithubMergeEligibility::Blocked;
        blockers.push(coducktor_contract::GithubBlocker {
            code: "conflicts".to_owned(),
            message: "Conflicts must be resolved before merging.".to_owned(),
        });
    } else if checks
        .iter()
        .any(|check| check.state == GithubCheckState::Failing)
    {
        eligibility = GithubMergeEligibility::Blocked;
        blockers.push(coducktor_contract::GithubBlocker {
            code: "checks-failing".to_owned(),
            message: "One or more checks are failing.".to_owned(),
        });
    } else if matches!(
        review_decision,
        GithubReviewDecision::ChangesRequested | GithubReviewDecision::ReviewRequired
    ) {
        eligibility = GithubMergeEligibility::Blocked;
        blockers.push(coducktor_contract::GithubBlocker {
            code: "reviews".to_owned(),
            message: if review_decision == GithubReviewDecision::ChangesRequested {
                "Changes were requested."
            } else {
                "A required review is missing."
            }
            .to_owned(),
        });
    } else if review_decision == GithubReviewDecision::Unknown || !requirements_readable {
        eligibility = GithubMergeEligibility::Unknown;
        blockers.push(coducktor_contract::GithubBlocker {
            code: "rules-unknown".to_owned(),
            message: "GitHub could not confirm review and branch-protection requirements."
                .to_owned(),
        });
    } else if checks
        .iter()
        .any(|check| check.state == GithubCheckState::Pending)
        || pr
            .merge_state_status
            .as_deref()
            .unwrap_or_default()
            .eq_ignore_ascii_case("UNSTABLE")
    {
        eligibility = GithubMergeEligibility::Pending;
        blockers.push(coducktor_contract::GithubBlocker {
            code: "pending".to_owned(),
            message: "Checks or GitHub mergeability are still pending.".to_owned(),
        });
    } else if mergeable != GithubMergeable::Mergeable
        || !matches!(
            pr.merge_state_status
                .as_deref()
                .unwrap_or_default()
                .to_ascii_uppercase()
                .as_str(),
            "CLEAN" | "HAS_HOOKS"
        )
        || methods.is_empty()
    {
        eligibility = GithubMergeEligibility::Unknown;
        blockers.push(coducktor_contract::GithubBlocker {
            code: "unknown".to_owned(),
            message: "GitHub could not confirm every merge requirement.".to_owned(),
        });
    } else {
        eligibility = GithubMergeEligibility::Ready;
    }
    let can_merge = eligibility == GithubMergeEligibility::Ready;
    let can_override = !can_merge
        && state == GithubPrState::Open
        && !pr.is_draft
        && mergeable != GithubMergeable::Conflicting
        && !methods.is_empty();
    Ok(GithubPrMergeState {
        number: pr.number,
        title: pr.title,
        url: pr.url,
        state,
        is_draft: pr.is_draft,
        head_ref: pr.head_ref,
        base_ref: pr.base_ref,
        head_sha: pr.head_sha,
        mergeable,
        review_decision,
        checks,
        methods,
        default_method,
        eligibility,
        blockers,
        can_merge,
        can_override,
    })
}

pub fn merge_preflight_allowed(state: &GithubPrMergeState, override_rules: bool) -> bool {
    state.can_merge || (override_rules && state.can_override)
}

impl ForgeDriver for GithubDriver {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Github
    }

    fn detect(&self) -> ForgeAvailability {
        GithubDriver::detect(self)
    }

    fn detect_cached(&self) -> Option<ForgeAvailability> {
        GithubDriver::detect_cached(self)
    }

    fn list_issues(&self, refresh: bool, limit: usize) -> Vec<GithubItem> {
        GithubDriver::list_issues(self, refresh, limit)
    }

    fn list_prs(&self, refresh: bool, limit: usize) -> Vec<GithubItem> {
        GithubDriver::list_prs(self, refresh, limit)
    }

    fn create_pr(&self, input: &DraftPrInput) -> DraftPrOutcome {
        GithubDriver::create_draft_pr(self, input)
    }

    fn pr_status(&self, branch: &str) -> Option<ForgePrStatus> {
        GithubDriver::pr_status(self, branch)
    }

    fn pr_merge_state(&self, number: u64, refresh: bool) -> ForgePrMergeStateResult {
        GithubDriver::pr_merge_state(self, number, refresh)
    }

    fn merge_pr(&self, number: u64, input: &ForgeMergeInput) -> ForgeMergeResult {
        GithubDriver::merge_pr(self, number, input)
    }

    fn pr_diff(&self, number: u64, refresh: bool) -> ForgePrDiffResult {
        GithubDriver::pr_diff(self, number, refresh)
    }

    fn comments(&self, kind: GithubItemKind, number: u64, refresh: bool) -> GithubCommentsData {
        GithubDriver::comments(self, kind, number, refresh)
    }

    fn view_url(&self, kind: ForgeRefKind, reference: &str) -> Option<String> {
        GithubDriver::view_url(self, kind, reference)
    }

    fn list(&self, refresh: bool, limit: usize) -> GithubData {
        GithubDriver::list(self, refresh, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct FixtureRunner {
        calls: AtomicUsize,
    }

    impl CommandRunner for FixtureRunner {
        fn run(
            &self,
            binary: &str,
            _cwd: &Path,
            args: &[String],
            _timeout: Duration,
        ) -> CommandOutput {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if binary != "gh" {
                return CommandOutput {
                    ok: true,
                    stdout: String::new(),
                    stderr: String::new(),
                    not_found: false,
                };
            }
            match args.first().map(String::as_str) {
                Some("repo") => CommandOutput { ok: true, stdout: "owner/demo\n".to_owned(), stderr: String::new(), not_found: false },
                Some("issue") => CommandOutput { ok: true, stdout: r#"[{"number":7,"title":"Issue","author":{"login":"ada"},"createdAt":"2026-08-01T00:00:00Z","labels":[{"name":"bug","color":"d73a4a"}],"body":"body","url":"https://github.com/owner/demo/issues/7"}]"#.to_owned(), stderr: String::new(), not_found: false },
                Some("pr") if args.get(1).map(String::as_str) == Some("list") => CommandOutput { ok: true, stdout: r#"[{"number":8,"title":"PR","author":{"login":"lin"},"createdAt":"2026-08-01T00:00:00Z","labels":[],"body":"body","url":"https://github.com/owner/demo/pull/8","isDraft":false,"additions":2,"deletions":1}]"#.to_owned(), stderr: String::new(), not_found: false },
                Some("pr") => CommandOutput { ok: true, stdout: r#"{"number":8,"url":"https://github.com/owner/demo/pull/8","state":"OPEN","isDraft":false,"statusCheckRollup":[{"conclusion":"SUCCESS"}]}"#.to_owned(), stderr: String::new(), not_found: false },
                Some("api") => CommandOutput { ok: true, stdout: "{}".to_owned(), stderr: String::new(), not_found: false },
                _ => CommandOutput { ok: false, stdout: String::new(), stderr: "unsupported fixture".to_owned(), not_found: false },
            }
        }
    }

    #[test]
    fn list_is_cached_per_driver_and_keeps_label_colors() {
        let runner = Arc::new(FixtureRunner::default());
        let driver = GithubDriver::with_runner(
            "/repo/a",
            Some(GithubRepoRef {
                owner: "owner".into(),
                repo: "demo".into(),
            }),
            runner.clone(),
        );
        let first = driver.list(false, 30);
        let calls = runner.calls.load(Ordering::Relaxed);
        let second = driver.list(false, 30);
        assert_eq!(first, second);
        assert_eq!(runner.calls.load(Ordering::Relaxed), calls);
        assert_eq!(first.issues[0].comments, 0);
        assert_eq!(
            first
                .label_colors
                .as_ref()
                .and_then(|colors| colors.get("bug")),
            Some(&"d73a4a".to_owned())
        );
    }

    #[test]
    fn list_degrades_one_disabled_capability_without_hiding_the_other() {
        struct PullRequestOnly;
        impl CommandRunner for PullRequestOnly {
            fn run(
                &self,
                binary: &str,
                _cwd: &Path,
                args: &[String],
                _timeout: Duration,
            ) -> CommandOutput {
                if binary != "gh" {
                    return CommandOutput {
                        ok: true,
                        stdout: String::new(),
                        stderr: String::new(),
                        not_found: false,
                    };
                }
                if args.first().map(String::as_str) == Some("repo") {
                    return CommandOutput {
                        ok: true,
                        stdout: "owner/demo\n".into(),
                        stderr: String::new(),
                        not_found: false,
                    };
                }
                if args.first().map(String::as_str) == Some("issue") {
                    return CommandOutput {
                        ok: false,
                        stdout: String::new(),
                        stderr: "issues are disabled".into(),
                        not_found: false,
                    };
                }
                if args.first().map(String::as_str) == Some("pr") {
                    return CommandOutput { ok: true, stdout: r#"[{"number":8,"title":"PR","author":null,"createdAt":"t","labels":[],"body":null,"url":"u","isDraft":false,"additions":1,"deletions":0}]"#.into(), stderr: String::new(), not_found: false };
                }
                CommandOutput {
                    ok: true,
                    stdout: "{}".into(),
                    stderr: String::new(),
                    not_found: false,
                }
            }
        }
        let driver = GithubDriver::with_runner("/repo", None, Arc::new(PullRequestOnly));
        let data = driver.list(true, 30);
        assert!(data.available);
        assert!(data.issues.is_empty());
        assert_eq!(data.prs[0].number, 8);
    }

    #[test]
    fn missing_gh_is_a_quiet_availability_result() {
        struct Missing;
        impl CommandRunner for Missing {
            fn run(
                &self,
                _binary: &str,
                _cwd: &Path,
                _args: &[String],
                _timeout: Duration,
            ) -> CommandOutput {
                CommandOutput {
                    ok: false,
                    stdout: String::new(),
                    stderr: "spawn gh".into(),
                    not_found: true,
                }
            }
        }
        let driver = GithubDriver::with_runner("/repo", None, Arc::new(Missing));
        let result = driver.detect();
        assert!(!result.available);
        assert!(result.reason.unwrap().contains("gh CLI not found"));
    }

    #[test]
    fn pr_status_collapses_a_rollup_to_one_glyph() {
        let driver = GithubDriver::with_runner("/repo", None, Arc::new(FixtureRunner::default()));
        let status = driver.pr_status("feature").expect("fixture PR");
        assert_eq!(status.number, 8);
        assert_eq!(status.checks, Some(ChecksGlyph::Passing));
    }

    #[test]
    fn merge_policy_requires_authoritative_rules_for_ready() {
        let raw = json!({
            "number":128,"title":"Ready","url":"https://github.com/o/r/pull/128","state":"OPEN","isDraft":false,
            "headRefName":"feat","baseRefName":"main","headRefOid":"0123456789abcdef0123456789abcdef01234567",
            "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","reviewDecision":"APPROVED",
            "statusCheckRollup":[{"name":"test","conclusion":"SUCCESS","detailsUrl":"https://example.test"}]
        });
        let state = normalize_merge_state(
            &raw,
            &json!({"allow_squash_merge":true,"squash_merge_commit_title":"PR_TITLE"}),
            true,
            &["test".into()],
        )
        .unwrap();
        assert_eq!(state.eligibility, GithubMergeEligibility::Ready);
        assert_eq!(state.methods, vec![GithubMergeMethod::Squash]);
        assert!(state.can_merge);
        assert_eq!(state.checks[0].state, GithubCheckState::Passing);
        let unknown =
            normalize_merge_state(&raw, &json!({"allow_squash_merge":true}), false, &[]).unwrap();
        assert_eq!(unknown.eligibility, GithubMergeEligibility::Unknown);
        assert!(unknown.can_override);
    }
}
