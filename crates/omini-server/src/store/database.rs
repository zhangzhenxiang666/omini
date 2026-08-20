use super::*;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("base64 error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid persisted data: {0}")]
    InvalidData(String),
    #[error("LLM context version conflict: expected {expected}, found {actual}")]
    ContextVersionConflict { expected: i64, actual: i64 },
}

pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await?;

        let db = Self { pool };
        db.initialize().await?;
        Ok(db)
    }
}
