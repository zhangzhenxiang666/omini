use crate::client;
use crate::features::setup::state::ConfigurationForm;
use std::time::Duration;
pub async fn bootstrap_configuration(
    connection: &client::ConfigurationConnection,
    form: &ConfigurationForm,
) -> Result<client::ProjectConnection, String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("build configuration client: {error}"))?;
    let base = format!(
        "http://{}/v1/projects/{}",
        connection.addr, connection.project_id
    );
    let request = omini_protocol::BootstrapProjectConfigurationRequest {
        provider_id: form.provider_id.clone(),
        protocol: form.protocol,
        base_url: form.base_url.clone(),
        model_id: form.model_id.clone(),
        environment_variable: (!form.api_key.trim().is_empty())
            .then(|| form.environment_variable.clone()),
        api_key: (!form.api_key.trim().is_empty()).then(|| form.api_key.clone()),
    };
    let response = http
        .post(format!("{base}/configuration"))
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("save configuration: {error}"))?;
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "save failed".to_string()));
    }
    let status: omini_protocol::ProjectConfigurationResponse = response
        .json()
        .await
        .map_err(|error| format!("read configuration result: {error}"))?;
    if status.state != omini_protocol::ProjectConfigurationState::Ready {
        return Err(status
            .message
            .unwrap_or_else(|| "configuration is still incomplete".to_string()));
    }
    let open = http
        .post(format!("{base}/open"))
        .send()
        .await
        .map_err(|error| format!("open configured project: {error}"))?;
    if !open.status().is_success() {
        return Err(open
            .text()
            .await
            .unwrap_or_else(|_| "project open failed".to_string()));
    }
    let open = open
        .json()
        .await
        .map_err(|error| format!("read configured project: {error}"))?;
    Ok(client::ProjectConnection {
        addr: connection.addr,
        project_id: connection.project_id.clone(),
        client_id: connection.client_id.clone(),
        open,
    })
}
