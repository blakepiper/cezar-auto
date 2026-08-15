use async_trait::async_trait;
use coducktor_contract::{
    ApiRun, ConfigResponse, CreateRunInput, CreateRunResponse, GithubData, HealthResponse,
    ProjectsResponse, ProviderStatusResponse, Runner, RunnerModelCatalogResponse, Skill,
    WorkflowsResponse, WorkspaceConfigResponse, WorkspaceUsageResponse,
};
use futures_core::stream::BoxStream;

use crate::error::EngineError;
use crate::http::HttpEngine;
use crate::scope::Scope;
use crate::ws::EngineEvent;

/// Input accepted by the engine's start-run seam.
pub type StartRunInput = CreateRunInput;

/// Demand-driven WebSocket topics exposed to screens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Topic {
    Health,
    Todos,
    Run { id: String },
    Named(String),
}

/// The only backend seam the terminal UI is allowed to import.
#[async_trait]
pub trait Engine: Send + Sync {
    async fn health(&self) -> Result<HealthResponse, EngineError>;
    async fn list_runs(&self, scope: &Scope) -> Result<Vec<ApiRun>, EngineError>;
    async fn start_run(
        &self,
        scope: &Scope,
        input: StartRunInput,
    ) -> Result<CreateRunResponse, EngineError>;
    async fn get_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError>;
    async fn workflows(&self, scope: &Scope) -> Result<WorkflowsResponse, EngineError>;
    async fn skills(&self, scope: &Scope) -> Result<Vec<Skill>, EngineError>;
    async fn projects(&self) -> Result<ProjectsResponse, EngineError>;
    async fn workspace_config(&self) -> Result<WorkspaceConfigResponse, EngineError>;
    async fn workspace_usage(&self) -> Result<WorkspaceUsageResponse, EngineError>;
    async fn config(&self, scope: &Scope) -> Result<ConfigResponse, EngineError>;
    async fn provider_status(&self) -> Result<ProviderStatusResponse, EngineError>;
    async fn models(&self, runner: Runner) -> Result<RunnerModelCatalogResponse, EngineError>;
    async fn github(&self, scope: &Scope) -> Result<GithubData, EngineError>;
    fn subscribe(&self, topic: Topic) -> BoxStream<'static, EngineEvent>;
}

#[async_trait]
impl Engine for HttpEngine {
    async fn health(&self) -> Result<HealthResponse, EngineError> {
        HttpEngine::health(self).await
    }

    async fn list_runs(&self, scope: &Scope) -> Result<Vec<ApiRun>, EngineError> {
        HttpEngine::list_runs(self, scope).await
    }

    async fn start_run(
        &self,
        scope: &Scope,
        input: StartRunInput,
    ) -> Result<CreateRunResponse, EngineError> {
        HttpEngine::start_run(self, scope, &input).await
    }

    async fn get_run(&self, scope: &Scope, run_id: &str) -> Result<ApiRun, EngineError> {
        HttpEngine::get_run(self, scope, run_id).await
    }

    async fn workflows(&self, scope: &Scope) -> Result<WorkflowsResponse, EngineError> {
        HttpEngine::workflows(self, scope).await
    }

    async fn skills(&self, scope: &Scope) -> Result<Vec<Skill>, EngineError> {
        HttpEngine::skills(self, scope).await
    }

    async fn projects(&self) -> Result<ProjectsResponse, EngineError> {
        HttpEngine::projects(self).await
    }

    async fn workspace_config(&self) -> Result<WorkspaceConfigResponse, EngineError> {
        HttpEngine::workspace_config(self).await
    }

    async fn workspace_usage(&self) -> Result<WorkspaceUsageResponse, EngineError> {
        HttpEngine::workspace_usage(self).await
    }

    async fn config(&self, scope: &Scope) -> Result<ConfigResponse, EngineError> {
        HttpEngine::config(self, scope).await
    }

    async fn provider_status(&self) -> Result<ProviderStatusResponse, EngineError> {
        HttpEngine::provider_status(self).await
    }

    async fn models(&self, runner: Runner) -> Result<RunnerModelCatalogResponse, EngineError> {
        HttpEngine::models(self, runner).await
    }

    async fn github(&self, scope: &Scope) -> Result<GithubData, EngineError> {
        HttpEngine::github(self, scope).await
    }

    fn subscribe(&self, topic: Topic) -> BoxStream<'static, EngineEvent> {
        let topic = match topic {
            Topic::Health => "health".to_owned(),
            Topic::Todos => "todos".to_owned(),
            Topic::Run { id } => format!("run:{id}"),
            Topic::Named(topic) => topic,
        };
        self.subscribe_topic(topic)
    }
}
