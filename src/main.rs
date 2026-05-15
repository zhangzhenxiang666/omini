use omini::config::settings::OminiRoot;
use omini::db::Database;
use std::error::Error;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {e}");
        let mut source = e.source();
        while let Some(s) = source {
            eprintln!("  cause: {s}");
            source = s.source();
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let root = OminiRoot::init()?;

    let config = root.load_config()?;
    config.validate()?;

    let cwd = std::env::current_dir()?;
    let project = root.init_project(&cwd, &config)?;

    let project_state = project.load_state()?;
    let settings = config.to_settings(
        project_state.default_provider.as_deref(),
        project_state.default_model.as_deref(),
        project_state.thinking_effort,
    )?;

    Database::open(&root.db_path())
        .await
        .map(omini::db::init_global)?;

    omini::tui::run_ui(settings, project).await?;

    Ok(())
}
