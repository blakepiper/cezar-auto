use std::time::Duration;

use coducktor_client::{Engine, EngineError, HttpEngine, Scope, Topic};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sse_resume_uses_after_seq_and_deduplicates_replayed_frames() {
    let server = MockServer::start().await;
    let body = concat!(
        "id: 2\n",
        "event: run-event\n",
        "data: {\"seq\":2,\"ts\":\"now\",\"type\":\"old\"}\n\n",
        "id: 3\n",
        "event: ui-event\n",
        "data: {\"seq\":3,\"ts\":\"now\",\"type\":\"new\"}\n\n",
    );
    Mock::given(method("GET"))
        .and(path("/api/v1/p/shop/runs/run-1/events"))
        .and(query_param("afterSeq", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let mut events = engine
        .run_events(&Scope::Project("shop".into()), "run-1", None, Some(2.0))
        .await
        .unwrap();
    let event = events.next().await.unwrap().unwrap();

    assert_eq!(event.seq, 3.0);
    assert_eq!(event.channel.as_deref(), Some("ui-event"));
    assert_eq!(event.event.event_type, "new");
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn websocket_reconnect_resubscribes_held_topics() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let Some(Ok(Message::Text(frame))) = socket.next().await else {
                panic!("client did not subscribe");
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&frame).unwrap(),
                json!({"type":"subscribe","topic":"health"})
            );
            socket
                .send(Message::Text(
                    json!({
                        "type":"event",
                        "topic":"health",
                        "data":{"attempt":attempt}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            if attempt == 0 {
                socket.close(None).await.unwrap();
            }
        }
    });

    let engine = HttpEngine::new(format!("http://{address}")).unwrap();
    let mut events = engine.subscribe(Topic::Health);
    let first = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(3), events.next())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(first.data, json!({"attempt": 0}));
    assert_eq!(second.data, json!({"attempt": 1}));
    drop(events);
    server.await.unwrap();
}

#[tokio::test]
async fn http_statuses_become_domain_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/runs/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/runs/conflict"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error":"run is active"})))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    assert_eq!(
        engine.get_run(&Scope::Workspace, "missing").await,
        Err(EngineError::NotFound)
    );
    assert_eq!(
        engine.get_run(&Scope::Workspace, "conflict").await,
        Err(EngineError::Conflict {
            reason: "run is active".into()
        })
    );
}

#[tokio::test]
async fn run_mutations_hit_the_versioned_routes() {
    let server = MockServer::start().await;
    let run = json!({"id":"run-1","title":"Ship","workflow":"quick-task","task":"ship","status":"done","createdAt":"now","tokensUsed":0,"archived":false,"steps":[]});
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/run-1/archive"))
        .and(|request: &wiremock::Request| request.body.as_slice() == br#"{"archived":true}"#)
        .respond_with(ResponseTemplate::new(200).set_body_json(run.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/run-1/read"))
        .respond_with(ResponseTemplate::new(200).set_body_json(run.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/run-1/unread"))
        .respond_with(ResponseTemplate::new(200).set_body_json(run.clone()))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/p/shop/runs/run-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"deleted":true})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/archive-finished"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"archived":3})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/runs/read-all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"read":2})))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let scope = Scope::Project("shop".into());
    let archived = engine.archive_run(&scope, "run-1", true).await.unwrap();
    assert_eq!(archived.record.id, "run-1");
    assert!(engine.read_run(&scope, "run-1").await.is_ok());
    assert!(engine.unread_run(&scope, "run-1").await.is_ok());
    assert!(engine.delete_run(&scope, "run-1").await.unwrap().deleted);
    assert_eq!(engine.archive_finished(&scope).await.unwrap().archived, 3.0);
    assert_eq!(engine.mark_all_read(&scope).await.unwrap().read, 2.0);
}

#[tokio::test]
async fn ide_directory_file_and_save_round_trip_through_the_versioned_routes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/p/shop/ide/tree"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "path": "",
            "entries": [
                {"name": "src", "path": "src", "type": "dir"},
                {"name": "README.md", "path": "README.md", "type": "file", "size": 42}
            ],
            "truncated": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/p/shop/ide/file"))
        .and(query_param("path", "src/lib.rs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "path": "src/lib.rs",
            "content": "fn main() {}\n",
            "size": 13
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/p/shop/ide/file"))
        .and(|request: &wiremock::Request| {
            // serde_json's `Value` map serializes keys sorted, so compare JSON semantics,
            // not byte order.
            serde_json::from_slice::<serde_json::Value>(&request.body)
                .map(|body| {
                    body == json!({
                        "path": "src/lib.rs",
                        "content": "fn main() { println!(\"hi\"); }\n",
                    })
                })
                .unwrap_or(false)
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "path": "src/lib.rs",
            "content": "fn main() { println!(\"hi\"); }\n",
            "size": 35
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/p/shop/ide/file"))
        .and(query_param("path", "huge.bin"))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(json!({"error":"file is too large to edit"})),
        )
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let scope = Scope::Project("shop".into());
    let tree = engine.ide_tree(&scope, None).await.unwrap();
    assert_eq!(tree.entries.len(), 2);
    assert_eq!(tree.entries[0].name, "src");
    assert!(!tree.truncated);

    let file = engine.ide_file(&scope, "src/lib.rs").await.unwrap();
    assert_eq!(file.content, "fn main() {}\n");
    assert_eq!(file.size, 13);

    // The A10 accept criterion's edit-and-save round-trip: the PUT carries the draft bytes
    // verbatim and the engine returns the stored file's metadata.
    let saved = engine
        .ide_save(&scope, "src/lib.rs", "fn main() { println!(\"hi\"); }\n")
        .await
        .unwrap();
    assert_eq!(saved.size, 35);

    assert_eq!(
        engine.ide_file(&scope, "huge.bin").await,
        Err(EngineError::Conflict {
            reason: "file is too large to edit".into()
        })
    );
}

#[tokio::test]
async fn workflow_save_parse_and_delete_round_trip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/workflows"))
        .and(|request: &wiremock::Request| {
            serde_json::from_slice::<serde_json::Value>(&request.body)
                .map(|body| {
                    // The portable compact form: skills only, never both keys (spec 012).
                    body == json!({
                        "name": "my-chain",
                        "skills": ["om-fix", "omarchy"],
                        "overwrite": true,
                    })
                })
                .unwrap_or(false)
        })
        .respond_with(ResponseTemplate::new(201).set_body_json(
            json!({"path": "/repo/.ai/cezar/workflows/my-chain.yaml", "name": "my-chain"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/workflows/parse"))
        .and(|request: &wiremock::Request| {
            serde_json::from_slice::<serde_json::Value>(&request.body)
                .map(|body| body == json!({"yaml": "name: qa\nskills: [om-fix]"}))
                .unwrap_or(false)
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "qa",
            "steps": [{"id": "om-fix", "name": "om-fix", "skill": "om-fix", "prompt": "{{task}}"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/p/shop/workflows/my-chain"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"ok": true, "path": "/repo/.ai/cezar/workflows/my-chain.yaml"}),
            ),
        )
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let scope = Scope::Project("shop".into());
    let saved = engine
        .save_workflow(
            &scope,
            &coducktor_contract::SaveWorkflowInput {
                name: "my-chain".to_owned(),
                description: None,
                steps: None,
                skills: Some(vec!["om-fix".to_owned(), "omarchy".to_owned()]),
                overwrite: Some(true),
            },
        )
        .await
        .unwrap();
    assert!(saved.path.ends_with("my-chain.yaml"));

    let parsed = engine
        .parse_workflow(&scope, "name: qa\nskills: [om-fix]")
        .await
        .unwrap();
    assert_eq!(parsed.name, "qa");
    assert_eq!(parsed.steps.len(), 1);

    let deleted = engine.delete_workflow(&scope, "my-chain").await.unwrap();
    assert!(deleted.ok);
}

#[tokio::test]
async fn runs_index_is_workspace_level() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workspace/runs-index"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "runs": [{
                "projectId":"coducktor",
                "id":"run-1",
                "title":"Ship",
                "status":"running",
                "createdAt":"now",
                "archived":false,
                "workflow":"quick-task"
            }],
            "perProjectLimit":200,
            "truncated":["coducktor"],
            "referenceStatuses":{}
        })))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let index = engine.runs_index().await.unwrap();
    assert_eq!(index.runs.len(), 1);
    assert_eq!(index.runs[0].project_id, "coducktor");
    assert_eq!(index.per_project_limit, 200);
    assert_eq!(index.truncated, vec!["coducktor".to_owned()]);
}

// ---- Settings (spec §8.14, A12) — route-shape coverage for every new Engine method --------

#[tokio::test]
async fn workspace_config_and_ui_state_write_to_the_unscoped_workspace_routes() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workspace/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "browseRoot": "/home",
            "projectsDir": "/home/user/projects",
            "effectiveSkillsAutoUpdate": false,
            "composerDefaults": {"inheritedAutonomous": "source-dependent", "inheritedWorktree": false},
            "resources": {
                "maxParallel": 4, "maxMonitoringSessions": 2,
                "autoResumeOnUsageLimit": false, "intelligentContextRefresh": false,
                "worktreeRetentionDefault": 5
            },
            "agentDefaults": {}
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workspace/ui-state"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"notifications": {"enabled": true}})),
        )
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let config = engine
        .put_workspace_config(&coducktor_contract::SetWorkspaceConfigInput {
            projects_dir: Some("/home/user/projects".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(config.projects_dir, "/home/user/projects");

    let state = engine
        .put_workspace_ui_state(&coducktor_contract::WorkspaceUiState::default())
        .await
        .unwrap();
    assert_eq!(state.notifications.unwrap().enabled, Some(true));
}

#[tokio::test]
async fn agent_config_routes_are_project_scoped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/p/shop/agent-config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "editable": true, "files": [], "userMcp": null
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/p/shop/agent-config/claude-md"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "claude-md", "path": "CLAUDE.md", "exists": true, "content": "hi", "version": "v1"
        })))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let scope = Scope::Project("shop".into());
    let listing = engine.agent_config(&scope).await.unwrap();
    assert!(listing.editable);

    let saved = engine
        .put_agent_config_file(
            &scope,
            "claude-md",
            &coducktor_contract::SetAgentConfigInput {
                content: "hi".to_owned(),
                version: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.content, "hi");
}

#[tokio::test]
async fn agent_profiles_write_routes_are_workspace_level() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/workspace/agent-profiles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "profile": {
                "id": "acct-1", "provider": "claude", "label": "Work", "configDir": "/tmp/x",
                "path": "/tmp/x", "exists": true, "looksValid": true, "isDefault": false, "files": []
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workspace/agent-profiles/acct-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"removed": true, "id": "acct-1"})),
        )
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let created = engine
        .create_agent_profile(&coducktor_contract::CreateAgentProfileInput {
            provider: coducktor_contract::Runner::Claude,
            label: None,
            config_dir: "/tmp/x".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(created.profile.id, "acct-1");

    let removed = engine.remove_agent_profile("acct-1").await.unwrap();
    assert!(removed.removed);
}

#[tokio::test]
async fn worktrees_and_open_in_routes_are_project_scoped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/p/shop/worktrees"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "worktrees": [], "totalBytes": 0, "keep": 5
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/p/shop/open-targets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"targets": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/p/shop/open-in"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"opened": true, "path": "/repo"})),
        )
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let scope = Scope::Project("shop".into());
    let worktrees = engine.worktrees(&scope).await.unwrap();
    assert_eq!(worktrees.keep, 5);
    let targets = engine.open_targets(&scope).await.unwrap();
    assert!(targets.targets.is_empty());
    let opened = engine.open_project_in(&scope, "vscode").await.unwrap();
    assert!(opened.opened);
}

#[tokio::test]
async fn remove_and_update_project_hit_the_workspace_projects_route() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/projects/shop"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"removed": true, "id": "shop"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/projects/shop"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": {
                "id": "shop", "name": "shop", "root": "/repo", "addedAt": "now",
                "lastOpenedAt": "now", "source": "local", "status": "ok", "maxParallel": 3
            }
        })))
        .mount(&server)
        .await;

    let engine = HttpEngine::new(server.uri()).unwrap();
    let removed = engine.remove_project("shop").await.unwrap();
    assert!(removed.removed);
    let updated = engine
        .update_project(
            "shop",
            &coducktor_contract::UpdateProjectInput {
                max_parallel: Some(Some(3)),
                tags: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.project.max_parallel, Some(3.0));
}
