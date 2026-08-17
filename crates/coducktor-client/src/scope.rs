/// Whether an engine request targets the workspace or one registered project.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Workspace,
    Project(String),
}
