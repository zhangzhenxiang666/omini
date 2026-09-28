use super::Store;
use crate::store::StoreError;
use jiff::Timestamp;
use omini_entity::Project;

impl Store {
    pub async fn create_project(&self, project: &Project) -> Result<(), StoreError> {
        let mut db = self.conn();
        toasty::create!(Project {
            id: project.id.clone(),
            name: project.name.clone(),
            path: project.path.clone(),
            storage_key: project.storage_key.clone(),
            created_at: project.created_at,
            updated_at: project.updated_at,
            last_opened_at: project.last_opened_at,
        })
        .exec(&mut db)
        .await?;
        Ok(())
    }

    pub async fn get_project(&self, id: &str) -> Result<Option<Project>, StoreError> {
        let mut db = self.conn();
        Ok(Project::filter_by_id(id).first().exec(&mut db).await?)
    }

    pub async fn get_project_by_path(&self, path: &str) -> Result<Option<Project>, StoreError> {
        let mut db = self.conn();
        Ok(Project::filter_by_path(path).first().exec(&mut db).await?)
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, StoreError> {
        let mut db = self.conn();
        let mut projects: Vec<Project> = Project::all().exec(&mut db).await?;
        // SQL 侧的 `ORDER BY last_opened_at IS NULL, last_opened_at DESC, created_at DESC`
        // 在应用层等价表达:Option 的 None < Some,逆序比较即 None 殿后。
        projects.sort_by(|a, b| {
            b.last_opened_at
                .cmp(&a.last_opened_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        Ok(projects)
    }

    pub async fn update_project(&self, project: &Project) -> Result<(), StoreError> {
        let mut db = self.conn();
        // 缺行时静默无操作,与原 UPDATE 未命中即无操作的行为一致。
        if let Some(mut row) = Project::filter_by_id(&project.id)
            .first()
            .exec(&mut db)
            .await?
        {
            toasty::update!(row {
                name: project.name.clone(),
                path: project.path.clone(),
                updated_at: project.updated_at,
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    pub async fn mark_project_opened(&self, id: &str) -> Result<(), StoreError> {
        let mut db = self.conn();
        if let Some(mut row) = Project::filter_by_id(id).first().exec(&mut db).await? {
            toasty::update!(row {
                last_opened_at: Some(Timestamp::now())
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }
}
