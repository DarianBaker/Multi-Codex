use serde_json::Value;

pub(crate) const X_CODEX_TURN_STATE: &str = "x-codex-turn-state";

#[derive(Default)]
pub(crate) struct WebsocketTurnTracker;

impl WebsocketTurnTracker {
    pub(crate) fn starts_new_turn(&self, text: &str) -> bool {
        let Ok(request) = serde_json::from_str::<Value>(text) else {
            return false;
        };
        if request.get("type").and_then(Value::as_str) != Some("response.create") {
            return false;
        }
        request
            .get("client_metadata")
            .and_then(Value::as_object)
            .is_none_or(|metadata| !metadata.contains_key(X_CODEX_TURN_STATE))
    }
}
