use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cached_tokens: usize,
}

impl Usage {
    pub fn total_tokens(self) -> usize {
        self.prompt_tokens + self.completion_tokens
    }
}
