mod credentials;
mod proxy;
mod selfcheck;
mod settings;
mod status;
mod usage;
mod usage_wire;
mod websocket_turn;
pub mod win_env;

pub use credentials::CredentialLoadReport;
pub use credentials::LoadedAccountCredentials;
pub use proxy::serve;
pub use selfcheck::SelfcheckReport;
pub use selfcheck::resolve_codex_binary;
pub use selfcheck::run as run_selfcheck;
pub use settings::PoolSettings;
pub use settings::ProfileSettings;
pub use settings::validate_label;
pub use status::PoolStatus;
pub use status::pool_status;

#[cfg(test)]
#[path = "usage_tests.rs"]
mod usage_tests;
