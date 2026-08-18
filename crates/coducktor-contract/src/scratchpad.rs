use serde::{Deserialize, Serialize};

/// Per-project notes stored in the user's Coducktor home, outside the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Scratchpad {
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetScratchpadInput {
    pub content: String,
}
