//! Telegram provider implementation for devboy-tools.
//!
//! This first slice implements read-only messenger access via the Telegram Bot
//! API `getUpdates` endpoint. It exposes chats and messages the bot has already
//! received, which matches current Bot API constraints.

mod client;

pub use client::TelegramClient;

/// Default Telegram Bot API base URL.
pub const DEFAULT_TELEGRAM_API_URL: &str = "https://api.telegram.org";
