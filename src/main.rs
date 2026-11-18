mod diagnostics;
mod management;
mod platform;
mod response;
mod websocket;

use diagnostics::{Diagnostics, Record};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    client::{
        Empty, MiddlewareCall, MiddlewareResult, OutboundCall, OutboundRequest, PluginBuilder,
        PluginSession, SessionConfig, TypedReply, methods,
    },
};
use platform::{Rejection, UserAgent};
use response::PendingRequests;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let diagnostics = Arc::new(Diagnostics::start(
        &std::env::current_dir()?,
        &session.handshake().instance_id,
    ));
    let maintenance = Arc::clone(&diagnostics);
    let names = Arc::new(diagnostics::names::NameRefresh::default());
    let viewer = Arc::clone(&diagnostics);
    let pending = Arc::new(PendingRequests::default());
    let ws = Arc::new(websocket::ErrorMessages::default());
    let ws_maintenance = Arc::clone(&ws);
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware(move |call: MiddlewareCall| {
            let diagnostics = Arc::clone(&diagnostics);
            let pending = Arc::clone(&pending);
            let ws = Arc::clone(&ws);
            async move {
                match call {
                    MiddlewareCall::Request(call) => pending
                        .format_response(*call, &ws)
                        .await
                        .map(MiddlewareResult::Request),
                    MiddlewareCall::Outbound(call) => guard(*call, &pending, &diagnostics)
                        .await
                        .map(MiddlewareResult::Outbound),
                    MiddlewareCall::WebSocket(call) => {
                        ws.format(*call).await.map(MiddlewareResult::WebSocket)
                    }
                    other => other.forward().await,
                }
            }
        })?
        .management(management::registration(), move |call| {
            let viewer = Arc::clone(&viewer);
            async move { management::handle(&viewer, call.request).await }
        })?
        .on(methods::RECONCILE, move |call| {
            let maintenance = Arc::clone(&maintenance);
            let names = Arc::clone(&names);
            ws_maintenance.expire();
            async move {
                maintenance.prune(call.host.remaining_budget()).await?;
                if let Some(path) = maintenance.database_path() {
                    names.refresh(&call.host, path).await?;
                }
                Ok(TypedReply::new(Empty {}))
            }
        })?
        .build()?;
    session.run(plugin).await?;
    Ok(())
}

async fn guard(
    call: OutboundCall,
    pending: &PendingRequests,
    diagnostics: &Diagnostics,
) -> Result<OutboundRequest, PluginFault> {
    if call.metadata.provider != "openai" {
        return call.next.run(call.request).await;
    }
    let prepared = call.next.run(call.request).await?;
    let incoming = call
        .context
        .request
        .as_ref()
        .map_or(&[][..], |req| req.headers.as_slice());
    let client = UserAgent::read(incoming);
    let effective = UserAgent::read(&prepared.head.headers);
    if let Err(reason) = Rejection::compare(&client, &effective) {
        pending.reject(&call.metadata.request_id, reason);
        diagnostics.submit(Record::new(
            &call.metadata,
            &prepared.head.target,
            &client,
            &effective,
            reason,
        ));
        return Err(PluginFault::new(ErrorCode::Rejected, reason.to_string()));
    }
    Ok(prepared)
}
