#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMcpServerStatus {
    Disabled,
    Connecting,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMcpToolSnapshot {
    pub name: String,
    pub registered_name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMcpServerSnapshot {
    pub name: String,
    pub status: RuntimeMcpServerStatus,
    pub last_error: Option<String>,
    pub tools: Vec<RuntimeMcpToolSnapshot>,
}
