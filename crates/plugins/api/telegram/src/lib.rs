//! Telegram provider implementation for devboy-tools.
//!
//! Telegram Bot API access is driven by `getUpdates`.
//!
//! The Bot API does not expose a standalone "list every chat this bot currently
//! belongs to" method, so chat discovery is reconstructed from incoming updates:
//! message traffic plus `my_chat_member` membership updates.

mod client;

pub use client::TelegramClient;

/// Default Telegram Bot API base URL.
pub const DEFAULT_TELEGRAM_API_URL: &str = "https://api.telegram.org";
