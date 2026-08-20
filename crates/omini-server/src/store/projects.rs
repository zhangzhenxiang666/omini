use super::*;

impl Database {
    pub async fn create_project(&self, project: &Project) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO project(
                    id,
                    name,
                    path,
                    storage_key,
                    created_at,
                    updated_at,
                    last_opened_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&project.id)
        .bind(&project.name)
        .bind(&project.path)
        .bind(&project.storage_key)
        .bind(project.created_at)
        .bind(project.updated_at)
        .bind(project.last_opened_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_project(&self, id: &str) -> Result<Option<Project>, StoreError> {
        Ok(
            sqlx::query_as::<_, Project>("SELECT * FROM project WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn get_project_by_path(&self, path: &str) -> Result<Option<Project>, StoreError> {
        Ok(
            sqlx::query_as::<_, Project>("SELECT * FROM project WHERE path = ?")
                .bind(path)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, StoreError> {
        Ok(sqlx::query_as::<_, Project>(
            "SELECT *
                FROM project
                ORDER BY last_opened_at IS NULL, last_opened_at DESC, created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn update_project(&self, project: &Project) -> Result<(), StoreError> {
        sqlx::query("UPDATE project SET name = ?, path = ?, updated_at = ? WHERE id = ?")
            .bind(&project.name)
            .bind(&project.path)
            .bind(project.updated_at)
            .bind(&project.id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_project_opened(&self, id: &str) -> Result<(), StoreError> {
        let now = Utc::now();
        sqlx::query("UPDATE project SET last_opened_at = ? WHERE id = ?")
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
