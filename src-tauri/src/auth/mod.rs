//! Authentication module

pub mod claude;
pub mod claude_oauth;
pub mod claude_token_refresh;
pub mod oauth_server;
pub mod storage;
pub mod switcher;
pub mod token_refresh;

// ponytail: refreshes are rare; one global lock keeps auth.json/credentials and accounts.json ordered.
pub(crate) static AUTH_OPERATION_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

pub use claude::*;
pub use claude_oauth::*;
pub use claude_token_refresh::*;
pub use oauth_server::*;
pub use storage::*;
pub use switcher::*;
pub use token_refresh::*;
