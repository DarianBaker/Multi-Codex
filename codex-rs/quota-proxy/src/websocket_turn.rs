use serde_json::Value;

#[derive(Default)]
pub(crate) struct WebsocketTurnTracker {
    turn_id: Option<String>,
}

impl WebsocketTurnTracker {
    pub(crate) fn starts_new_turn(&mut self, text: &str) -> bool {
        let Ok(request) = serde_json::from_str::<Value>(text) else {
            return false;
        };
        if request.get("type").and_then(Value::as_str) != Some("response.create") {
            return false;
        }
        let Some(turn_id) = request
            .get("client_metadata")
            .and_then(|metadata| metadata.get("turn_id"))
            .and_then(Value::as_str)
        else {
            return false;
        };
        match self.turn_id.replace(turn_id.to_string()) {
            Some(previous) => previous != turn_id,
            None => false,
        }
    }
}
