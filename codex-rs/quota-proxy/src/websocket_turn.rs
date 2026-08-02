use serde_json::Value;

pub(crate) const X_CODEX_TURN_STATE: &str = "x-codex-turn-state";

#[derive(Default)]
pub(crate) struct WebsocketTurnTracker;

impl WebsocketTurnTracker {
    pub(crate) fn turn_state(&self, text: &str) -> Option<Option<String>> {
        let Ok(request) = serde_json::from_str::<Value>(text) else {
            return None;
        };
        if request.get("type").and_then(Value::as_str) != Some("response.create") {
            return None;
        }
        Some(
            request
                .get("client_metadata")
                .and_then(Value::as_object)
                .and_then(|metadata| metadata.get(X_CODEX_TURN_STATE))
                .and_then(Value::as_str)
                .map(str::to_string),
        )
    }
}
