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

pub const PUBLIC_MESSAGE: &str = "Request blocked by policy.";

// Older hosts use different policy wording. Only normalize this plugin's own 403s.
#[derive(Default)]
pub struct PendingRequests(Mutex<HashMap<String, bool>>);

impl PendingRequests {
    fn entries(&self) -> MutexGuard<'_, HashMap<String, bool>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn track(&self, id: String) -> RequestScope<'_> {
        self.entries().insert(id.clone(), false);
        RequestScope { pending: self, id }
    }

    pub fn reject(&self, id: &str) {
        if let Some(entry) = self.entries().get_mut(id) {
            *entry = true;
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
        if response.status == 403 && scope.rejected() {
            if is_websocket {
                websocket.insert(&call.context, &scope.id);
                return Ok(response);
            }
            response.remove_header("content-length");
            response.body = response.body.map_frames(|mut frame| {
                if let Ok(mut body) = serde_json::from_slice(&frame.payload)
                    && normalize(&mut body)
                {
                    frame.payload = body.to_string().into_bytes();
                }
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
    fn rejected(&self) -> bool {
        self.pending.entries().get(&self.id) == Some(&true)
    }
}

impl Drop for RequestScope<'_> {
    fn drop(&mut self) {
        self.pending.entries().remove(&self.id);
    }
}

pub fn normalize(body: &mut Value) -> bool {
    if body.pointer("/error/code").and_then(Value::as_str) == Some("policy_denied")
        && let Some(message) = body.pointer_mut("/error/message")
        && message.as_str() != Some(PUBLIC_MESSAGE)
    {
        *message = Value::String(PUBLIC_MESSAGE.into());
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_our_active_rejection_gets_the_generic_response() {
        let requests = PendingRequests::default();
        let first = requests.track("req_a".into());
        let second = requests.track("req_b".into());
        requests.reject("req_a");
        assert!(first.rejected());
        assert!(!second.rejected());
        drop(first);
        requests.reject("req_a");
        assert_eq!(requests.entries().len(), 1);

        let mut unrelated = json!({"error":{"code":"another_denial","message":"other"}});
        assert!(!normalize(&mut unrelated));
        assert_eq!(unrelated["error"]["message"], "other");
        let mut legacy = json!({"request_id":"req_a","error":{"code":"policy_denied","message":"request policy rejected the request"}});
        assert!(normalize(&mut legacy));
        assert_eq!(legacy["request_id"], "req_a");
        assert_eq!(legacy["error"]["message"], PUBLIC_MESSAGE);
        assert!(!normalize(&mut legacy));
    }
}
