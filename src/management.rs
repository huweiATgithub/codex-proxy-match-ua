mod history;

use crate::diagnostics::Diagnostics;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::management::{
        ManagementPage, ManagementRegistration, ManagementRequest, ManagementResource,
        ManagementResponse, ManagementRoute,
    },
    client::TypedReply,
};
use history::{Page, Query};
use serde::Serialize;
use serde_json::json;

const PAGE: &str = "ui/rejections.html";
const ROUTE: &str = "api/rejections";

pub fn registration() -> ManagementRegistration {
    ManagementRegistration {
        pages: vec![ManagementPage {
            id: "rejections".into(),
            title: "UA rejections".into(),
            description: Some("Rejected requests and their client and key User-Agents.".into()),
            entry: PAGE.into(),
            icon: None,
        }],
        resources: vec![ManagementResource {
            path: PAGE.into(),
            public: false,
        }],
        routes: vec![ManagementRoute {
            method: "GET".into(),
            path: ROUTE.into(),
            request_content_types: vec![],
            response_content_types: vec!["application/json".into()],
        }],
        callbacks: vec![],
    }
}

pub async fn handle(
    diagnostics: &Diagnostics,
    request: ManagementRequest,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    if request.path != ROUTE || request.method != "GET" {
        return reply(404, &json!({"error": "Unknown diagnostics route."}));
    }
    let query = match Query::parse(&request.query) {
        Ok(query) => query,
        Err(message) => return reply(400, &json!({"error": message})),
    };
    if let Some(path) = diagnostics.database_path().map(std::path::Path::to_owned) {
        let result = tokio::task::spawn_blocking(move || Page::read(&path, &query)).await;
        match result {
            Ok(Ok(page)) => return reply(200, &page),
            Ok(Err(error)) => eprintln!("Could not read UA diagnostics: {error}"),
            Err(error) => eprintln!("UA diagnostics reader failed: {error}"),
        }
    }
    reply(
        503,
        &json!({"error": "Rejection records are temporarily unavailable."}),
    )
}

fn reply(
    status: u16,
    body: &impl Serialize,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    let payload = serde_json::to_vec(body)
        .map_err(|_| PluginFault::new(ErrorCode::Fault, "Could not encode UA diagnostics."))?;
    Ok(TypedReply::new(ManagementResponse {
        status,
        content_type: "application/json".into(),
        headers: vec![],
    })
    .with_payload(payload))
}
