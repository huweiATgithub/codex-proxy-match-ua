use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
};

use gateway_plugin_sdk::{
    PluginFault,
    call::middleware::MiddlewareTransport,
    client::{MiddlewareResponse, RequestCall},
};
use serde_json::Value;

use crate::platform::Rejection;

// The host creates a unique model request ID for every request, including each WS turn.
// Its outbound policy error loses the plugin message before reaching the client.
#[derive(Default)]
pub struct PendingRequests(Mutex<HashMap<String, Option<Rejection>>>);

impl PendingRequests {
    fn entries(&self) -> MutexGuard<'_, HashMap<String, Option<Rejection>>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn track(&self, id: String) -> RequestScope<'_> {
        self.entries().insert(id.clone(), None);
        RequestScope { pending: self, id }
    }

    pub fn reject(&self, id: &str, reason: Rejection) {
        if let Some(entry) = self.entries().get_mut(id) {
            *entry = Some(reason);
        }
    }

    pub async fn format_response(
        &self,
        call: RequestCall,
        websocket: &crate::websocket::ErrorMessages,
    ) -> Result<MiddlewareResponse, PluginFault> {
        let is_websocket = call.request.head.transport == MiddlewareTransport::WebSocket;
        let scope = self.track(call.request.head.request_id.clone());
        let mut response = call.next.run(call.request).await?;
        if response.status == 403
            && let Some(reason) = scope.reason()
        {
            if is_websocket {
                websocket.insert(&call.context, &scope.id, reason);
                return Ok(response);
            }
            response.remove_header("content-length");
            response.body = response.body.map_frames(move |mut frame| {
                replace_message(&mut frame.payload, reason);
                Ok(vec![frame])
            })?;
        }
        Ok(response)
    }
}

// Dropping a cancelled request removes the slot, including when no response was received.
struct RequestScope<'a> {
    pending: &'a PendingRequests,
    id: String,
}

impl RequestScope<'_> {
    fn reason(&self) -> Option<Rejection> {
        self.pending.entries().get(&self.id).copied().flatten()
    }
}

impl Drop for RequestScope<'_> {
    fn drop(&mut self) {
        self.pending.entries().remove(&self.id);
    }
}

fn replace_message(payload: &mut Vec<u8>, reason: Rejection) {
    let Ok(mut body) = serde_json::from_slice::<Value>(payload) else {
        return;
    };
    if body.pointer("/error/code").and_then(Value::as_str) == Some("policy_denied")
        && let Some(message) = body.pointer_mut("/error/message")
    {
        *message = Value::String(reason.to_string());
        *payload = body.to_string().into_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Platform;

    #[test]
    fn only_the_rejected_request_gets_details_and_cancellation_releases_it() {
        let requests = PendingRequests::default();
        let first = requests.track("req_a".into());
        let second = requests.track("req_b".into());
        let reason = Rejection {
            client: Ok(Platform::Linux),
            effective: Ok(Platform::Windows),
        };
        requests.reject("req_a", reason);
        assert!(first.reason().is_some());
        assert!(second.reason().is_none());
        drop(first);
        requests.reject("req_a", reason);
        assert_eq!(requests.entries().len(), 1);
        let mut body = br#"{"error":{"code":"another_denial","message":"other"}}"#.to_vec();
        let original = body.clone();
        replace_message(&mut body, reason);
        assert_eq!(body, original);
    }
}
