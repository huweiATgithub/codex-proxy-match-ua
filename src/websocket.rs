use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use gateway_plugin_sdk::{
    CallContext, PluginFault,
    client::{WebSocketCall, WebSocketDirection, WebSocketKind, WebSocketMessage},
};
use serde_json::Value;

use crate::response;

// A native WS policy failure is a standalone error event. Editing it as a model stream
// frame triggers Responses lifecycle validation. Format the actual outgoing WS message.
type Turns = HashMap<String, Instant>;

#[derive(Default)]
pub struct ErrorMessages(Mutex<HashMap<Vec<u8>, Turns>>);

fn handshake_key(context: &CallContext) -> &[u8] {
    context
        .request
        .as_ref()
        .and_then(|request| {
            request
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("sec-websocket-key"))
        })
        .map_or(&[], |h| h.value.as_slice())
}

impl ErrorMessages {
    fn connections(&self) -> MutexGuard<'_, HashMap<Vec<u8>, Turns>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn insert(&self, context: &CallContext, request_id: &str) {
        self.connections()
            .entry(handshake_key(context).to_vec())
            .or_default()
            .insert(request_id.into(), Instant::now());
    }

    pub fn expire(&self) {
        // A client may disconnect before its rejection is delivered.
        self.connections().retain(|_, turns| {
            turns.retain(|_, at| at.elapsed() < Duration::from_secs(300));
            !turns.is_empty()
        });
    }

    fn take(&self, key: &[u8], request_id: &str) -> bool {
        let mut connections = self.connections();
        let Some(turns) = connections.get_mut(key) else {
            return false;
        };
        let found = turns.remove(request_id).is_some();
        if turns.is_empty() {
            connections.remove(key);
        }
        found
    }

    pub async fn format(
        &self,
        call: WebSocketCall,
    ) -> Result<Option<WebSocketMessage>, PluginFault> {
        let message = call.next.run(call.message).await?;
        let Some(message) = message else {
            return Ok(None);
        };
        let key = handshake_key(&call.context);
        if call.direction != WebSocketDirection::Outgoing
            || message.kind != WebSocketKind::Text
            || !self.connections().contains_key(key)
        {
            return Ok(Some(message));
        }
        // Normal connections never read or copy payloads here. On a rejected turn the host
        // may send response.metadata before the error. Only its model request ID is authoritative.
        let mut bytes = message.payload.collect().await?;
        if let Ok(mut event) = serde_json::from_slice::<Value>(&bytes)
            && event["type"] == "error"
            && event["status"] == 403
            && event.pointer("/error/code").and_then(Value::as_str) == Some("policy_denied")
            && let Some(id) = event["request_id"].as_str()
            && self.take(key, id)
            && response::normalize(&mut event)
        {
            bytes = event.to_string().into_bytes();
        }
        Ok(Some(WebSocketMessage::new(WebSocketKind::Text, bytes)))
    }
}
