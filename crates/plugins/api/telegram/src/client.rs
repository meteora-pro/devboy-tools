use std::collections::HashSet;
use std::fmt;

use async_trait::async_trait;
use devboy_core::types::ChatType;
use devboy_core::{
    Error, GetChatsParams, GetMessagesParams, MessageAttachment, MessageAuthor, MessengerChat,
    MessengerMessage, MessengerProvider, Pagination, ProviderResult, Result, SearchMessagesParams,
    SendMessageParams,
};
use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::DEFAULT_TELEGRAM_API_URL;

#[derive(Clone)]
pub struct TelegramClient {
    token: SecretString,
    base_url: String,
    http: reqwest::Client,
    bot_username: Option<String>,
}

impl fmt::Debug for TelegramClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramClient")
            .field("token", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("http", &self.http)
            .field("bot_username", &self.bot_username)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct TelegramApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    error_code: Option<u16>,
    parameters: Option<TelegramResponseParameters>,
}

#[derive(Debug, Deserialize)]
struct TelegramResponseParameters {
    retry_after: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
    edited_message: Option<TelegramMessage>,
    channel_post: Option<TelegramMessage>,
    edited_channel_post: Option<TelegramMessage>,
}

impl TelegramUpdate {
    fn into_message(self) -> Option<(TelegramMessage, bool)> {
        if let Some(message) = self.message {
            return Some((message, false));
        }
        if let Some(message) = self.edited_message {
            return Some((message, true));
        }
        if let Some(message) = self.channel_post {
            return Some((message, false));
        }
        self.edited_channel_post.map(|message| (message, true))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    date: i64,
    chat: TelegramChat,
    from: Option<TelegramUser>,
    sender_chat: Option<TelegramChat>,
    text: Option<String>,
    caption: Option<String>,
    edit_date: Option<i64>,
    reply_to_message: Option<Box<TelegramReplyMessage>>,
    message_thread_id: Option<i64>,
    document: Option<TelegramDocument>,
    audio: Option<TelegramFile>,
    video: Option<TelegramFile>,
    voice: Option<TelegramFile>,
    sticker: Option<TelegramSticker>,
    #[serde(default)]
    photo: Vec<TelegramPhotoSize>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramReplyMessage {
    message_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramUser {
    id: i64,
    first_name: String,
    last_name: Option<String>,
    username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
    title: Option<String>,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramFile {
    file_id: String,
    mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramSticker {
    file_id: String,
    emoji: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramPhotoSize {
    file_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TelegramSearchCursor {
    next_update_offset: Option<i64>,
}

impl TelegramClient {
    pub fn new(token: SecretString) -> Self {
        Self {
            token,
            base_url: DEFAULT_TELEGRAM_API_URL.to_string(),
            http: reqwest::Client::new(),
            bot_username: None,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_bot_username(mut self, bot_username: Option<String>) -> Self {
        self.bot_username = bot_username;
        self
    }

    async fn get_updates(&self, offset: Option<i64>, limit: u32) -> Result<Vec<TelegramUpdate>> {
        let mut query = vec![("timeout", "0".to_string())];
        query.push(("limit", limit.min(100).to_string()));
        if let Some(offset) = offset {
            query.push(("offset", offset.to_string()));
        }

        self.get_api("getUpdates", &query).await
    }

    async fn get_api<T>(&self, method: &str, query: &[(&str, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let url = format!(
            "{}/bot{}/{}",
            self.base_url.trim_end_matches('/'),
            self.token.expose_secret(),
            method
        );

        let response = self
            .http
            .get(url)
            .query(&query)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let body = response.text().await.map_err(map_reqwest_error)?;
        map_telegram_api_payload(status, &body)
    }

    async fn post_api<T>(&self, method: &str, form: &[(&str, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let url = format!(
            "{}/bot{}/{}",
            self.base_url.trim_end_matches('/'),
            self.token.expose_secret(),
            method
        );

        let response = self
            .http
            .post(url)
            .form(form)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let body = response.text().await.map_err(map_reqwest_error)?;
        map_telegram_api_payload(status, &body)
    }
}

fn map_telegram_api_payload<T>(status: StatusCode, body: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    if !status.is_success() {
        return Err(map_http_error(status, body));
    }

    let payload: TelegramApiResponse<T> = serde_json::from_str(body)?;
    if payload.ok {
        payload.result.ok_or_else(|| {
            Error::InvalidData("Telegram API response missing result payload".to_string())
        })
    } else if let Some(retry_after) = payload
        .parameters
        .as_ref()
        .and_then(|parameters| parameters.retry_after)
    {
        Err(Error::RateLimited {
            retry_after: Some(retry_after),
        })
    } else {
        Err(Error::from_status(
            payload.error_code.unwrap_or(400),
            payload
                .description
                .unwrap_or_else(|| "Telegram API request failed".to_string()),
        ))
    }
}

#[async_trait]
impl MessengerProvider for TelegramClient {
    fn provider_name(&self) -> &'static str {
        "telegram"
    }

    async fn get_chats(&self, params: GetChatsParams) -> Result<ProviderResult<MessengerChat>> {
        let limit = params.limit.unwrap_or(20).min(100);
        let mut offset = parse_cursor(params.cursor.as_deref())?;
        let mut items = Vec::new();
        let mut seen_chat_ids = HashSet::new();
        let search = params.search.as_deref().map(str::to_lowercase);
        let mut has_more = false;
        let mut next_cursor = None;

        loop {
            let updates = self.get_updates(offset, 100).await?;
            if updates.is_empty() {
                break;
            }

            let raw_len = updates.len();
            for (index, update) in updates.into_iter().enumerate() {
                let resume_offset = update.update_id + 1;
                let Some((message, _)) = update.into_message() else {
                    offset = Some(resume_offset);
                    continue;
                };

                let chat = map_chat(&message.chat);
                offset = Some(resume_offset);

                if !matches_chat_type(chat.chat_type, params.chat_type) {
                    continue;
                }
                if !matches_search(&chat, search.as_deref()) {
                    continue;
                }
                if !seen_chat_ids.insert(chat.id.clone()) {
                    continue;
                }

                items.push(chat);
                if items.len() >= limit as usize {
                    has_more = index + 1 < raw_len || raw_len == 100;
                    next_cursor = Some(resume_offset.to_string());
                    break;
                }
            }

            if items.len() >= limit as usize {
                break;
            }

            if raw_len < 100 {
                break;
            }

            has_more = true;
            next_cursor = offset.map(|value| value.to_string());
        }

        Ok(ProviderResult::new(items).with_pagination(Pagination {
            offset: 0,
            limit,
            total: None,
            has_more,
            next_cursor,
        }))
    }

    async fn get_messages(
        &self,
        params: GetMessagesParams,
    ) -> Result<ProviderResult<MessengerMessage>> {
        let limit = params.limit.unwrap_or(100).min(100);
        let mut offset = parse_cursor(params.cursor.as_deref())?;
        let since = parse_timestamp("since", params.since.as_deref())?;
        let until = parse_timestamp("until", params.until.as_deref())?;
        let thread_id = parse_optional_i64("thread_id", params.thread_id.as_deref())?;
        let mut items = Vec::new();
        let mut has_more = false;
        let mut next_cursor = None;

        loop {
            let updates = self.get_updates(offset, 100).await?;
            if updates.is_empty() {
                break;
            }

            let raw_len = updates.len();
            for (index, update) in updates.into_iter().enumerate() {
                let resume_offset = update.update_id + 1;
                let Some((message, edited_from_update)) = update.into_message() else {
                    offset = Some(resume_offset);
                    continue;
                };
                offset = Some(resume_offset);

                if message.chat.id.to_string() != params.chat_id {
                    continue;
                }
                if !matches_message_window(&message, since, until) {
                    continue;
                }
                if !matches_thread(&message, thread_id) {
                    continue;
                }

                items.push(map_message(message, edited_from_update));
                if items.len() >= limit as usize {
                    has_more = index + 1 < raw_len || raw_len == 100;
                    next_cursor = Some(resume_offset.to_string());
                    break;
                }
            }

            if items.len() >= limit as usize {
                break;
            }

            if raw_len < 100 {
                break;
            }

            has_more = true;
            next_cursor = offset.map(|value| value.to_string());
        }

        Ok(ProviderResult::new(items).with_pagination(Pagination {
            offset: 0,
            limit,
            total: None,
            has_more,
            next_cursor,
        }))
    }

    async fn search_messages(
        &self,
        params: SearchMessagesParams,
    ) -> Result<ProviderResult<MessengerMessage>> {
        let query = params.query.trim().to_lowercase();
        if query.is_empty() {
            return Err(Error::InvalidData(
                "search query must not be empty".to_string(),
            ));
        }

        let limit = params.limit.unwrap_or(20).min(100) as usize;
        let mut offset = parse_search_cursor(params.cursor.as_deref())?.next_update_offset;
        let since = parse_timestamp("since", params.since.as_deref())?;
        let until = parse_timestamp("until", params.until.as_deref())?;
        let mut found = Vec::new();

        loop {
            let updates = self.get_updates(offset, 100).await?;
            if updates.is_empty() {
                return Ok(ProviderResult::new(found).with_pagination(Pagination {
                    offset: 0,
                    limit: limit as u32,
                    total: None,
                    has_more: false,
                    next_cursor: None,
                }));
            }

            let raw_len = updates.len();
            for (index, update) in updates.into_iter().enumerate() {
                let resume_offset = update.update_id + 1;
                let Some((message, edited_from_update)) = update.into_message() else {
                    offset = Some(resume_offset);
                    continue;
                };
                offset = Some(resume_offset);

                if let Some(chat_id) = params.chat_id.as_deref()
                    && message.chat.id.to_string() != chat_id
                {
                    continue;
                }
                if !matches_message_window(&message, since, until) {
                    continue;
                }

                let normalized = normalized_message_text(&message);
                if !normalized.contains(&query) {
                    continue;
                }

                found.push(map_message(message, edited_from_update));
                if found.len() >= limit {
                    let has_more = index + 1 < raw_len || raw_len == 100;
                    let next_cursor = if has_more {
                        serialize_search_cursor(&TelegramSearchCursor {
                            next_update_offset: offset,
                        })?
                    } else {
                        None
                    };

                    return Ok(ProviderResult::new(found).with_pagination(Pagination {
                        offset: 0,
                        limit: limit as u32,
                        total: None,
                        has_more,
                        next_cursor,
                    }));
                }
            }

            if raw_len < 100 {
                return Ok(ProviderResult::new(found).with_pagination(Pagination {
                    offset: 0,
                    limit: limit as u32,
                    total: None,
                    has_more: false,
                    next_cursor: None,
                }));
            }
        }
    }

    async fn send_message(&self, params: SendMessageParams) -> Result<MessengerMessage> {
        if !params.attachments.is_empty() {
            return Err(Error::ProviderUnsupported {
                provider: self.provider_name().to_string(),
                operation: "send_message attachments".to_string(),
            });
        }

        let mut form = vec![
            ("chat_id", params.chat_id.clone()),
            ("text", params.text.clone()),
            ("parse_mode", "Markdown".to_string()),
            ("disable_web_page_preview", "true".to_string()),
        ];

        if let Some(thread_id) = parse_optional_i64("thread_id", params.thread_id.as_deref())? {
            form.push(("message_thread_id", thread_id.to_string()));
        }
        if let Some(reply_to_id) = parse_optional_i64("reply_to_id", params.reply_to_id.as_deref())?
        {
            form.push(("reply_to_message_id", reply_to_id.to_string()));
        }

        let message: TelegramMessage = self.post_api("sendMessage", &form).await?;
        Ok(map_message(message, false))
    }
}

fn parse_cursor(cursor: Option<&str>) -> Result<Option<i64>> {
    parse_optional_i64("cursor", cursor)
}

fn parse_search_cursor(cursor: Option<&str>) -> Result<TelegramSearchCursor> {
    let Some(cursor) = cursor.map(str::trim).filter(|cursor| !cursor.is_empty()) else {
        return Ok(TelegramSearchCursor::default());
    };

    match serde_json::from_str::<TelegramSearchCursor>(cursor) {
        Ok(state) => Ok(state),
        Err(_) => Ok(TelegramSearchCursor {
            next_update_offset: parse_optional_i64("cursor", Some(cursor))?,
        }),
    }
}

fn serialize_search_cursor(state: &TelegramSearchCursor) -> Result<Option<String>> {
    if state.next_update_offset.is_none() {
        return Ok(None);
    }

    serde_json::to_string(state).map(Some).map_err(|e| {
        Error::InvalidData(format!("failed to serialize Telegram search cursor: {e}"))
    })
}

fn parse_timestamp(field_name: &str, value: Option<&str>) -> Result<Option<i64>> {
    parse_optional_i64(field_name, value)
}

fn parse_optional_i64(field_name: &str, value: Option<&str>) -> Result<Option<i64>> {
    value
        .map(|raw| {
            raw.trim().parse::<i64>().map_err(|_| {
                Error::InvalidData(format!(
                    "{field_name} must be a Telegram integer timestamp or cursor"
                ))
            })
        })
        .transpose()
}

fn map_reqwest_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else if error.is_connect() {
        Error::Network(error.to_string())
    } else {
        Error::Http(error.to_string())
    }
}

fn map_http_error(status: StatusCode, body: &str) -> Error {
    let message = serde_json::from_str::<TelegramApiResponse<serde_json::Value>>(body)
        .ok()
        .and_then(|payload| payload.description)
        .unwrap_or_else(|| body.to_string());
    Error::from_status(status.as_u16(), message)
}

fn map_chat(chat: &TelegramChat) -> MessengerChat {
    MessengerChat {
        id: chat.id.to_string(),
        key: chat_key(chat),
        name: chat_name(chat),
        chat_type: telegram_chat_type(chat),
        source: "telegram".to_string(),
        member_count: None,
        description: None,
        is_active: true,
    }
}

fn map_message(message: TelegramMessage, edited_from_update: bool) -> MessengerMessage {
    let text = message_text(&message);
    let thread_id = message
        .message_thread_id
        .map(|value| value.to_string())
        .or_else(|| {
            message
                .reply_to_message
                .as_ref()
                .map(|reply| reply.message_id.to_string())
        });
    let reply_to_id = message
        .reply_to_message
        .as_ref()
        .map(|reply| reply.message_id.to_string());

    MessengerMessage {
        id: message.message_id.to_string(),
        chat_id: message.chat.id.to_string(),
        text,
        author: message_author(&message),
        source: "telegram".to_string(),
        timestamp: message.date.to_string(),
        thread_id,
        reply_to_id,
        attachments: map_attachments(&message),
        is_edited: edited_from_update || message.edit_date.is_some(),
    }
}

fn message_text(message: &TelegramMessage) -> String {
    message
        .text
        .as_ref()
        .or(message.caption.as_ref())
        .cloned()
        .unwrap_or_default()
}

fn normalized_message_text(message: &TelegramMessage) -> String {
    message_text(message)
        .to_lowercase()
}

fn message_author(message: &TelegramMessage) -> MessageAuthor {
    if let Some(chat) = message.sender_chat.as_ref() {
        return MessageAuthor {
            id: chat.id.to_string(),
            name: chat_name(chat),
            username: chat.username.clone(),
            avatar_url: None,
        };
    }

    if let Some(user) = message.from.as_ref() {
        return MessageAuthor {
            id: user.id.to_string(),
            name: telegram_user_name(user),
            username: user.username.clone(),
            avatar_url: None,
        };
    }

    MessageAuthor {
        id: "unknown".to_string(),
        name: "Unknown".to_string(),
        username: None,
        avatar_url: None,
    }
}

fn map_attachments(message: &TelegramMessage) -> Vec<MessageAttachment> {
    let mut attachments = Vec::new();

    if let Some(document) = message.document.as_ref() {
        attachments.push(MessageAttachment {
            id: Some(document.file_id.clone()),
            name: document.file_name.clone(),
            attachment_type: Some("file".to_string()),
            url: None,
            mime_type: document.mime_type.clone(),
        });
    }
    if let Some(audio) = message.audio.as_ref() {
        attachments.push(MessageAttachment {
            id: Some(audio.file_id.clone()),
            name: None,
            attachment_type: Some("audio".to_string()),
            url: None,
            mime_type: audio.mime_type.clone(),
        });
    }
    if let Some(video) = message.video.as_ref() {
        attachments.push(MessageAttachment {
            id: Some(video.file_id.clone()),
            name: None,
            attachment_type: Some("video".to_string()),
            url: None,
            mime_type: video.mime_type.clone(),
        });
    }
    if let Some(voice) = message.voice.as_ref() {
        attachments.push(MessageAttachment {
            id: Some(voice.file_id.clone()),
            name: None,
            attachment_type: Some("voice".to_string()),
            url: None,
            mime_type: voice.mime_type.clone(),
        });
    }
    if let Some(sticker) = message.sticker.as_ref() {
        attachments.push(MessageAttachment {
            id: Some(sticker.file_id.clone()),
            name: sticker.emoji.clone(),
            attachment_type: Some("sticker".to_string()),
            url: None,
            mime_type: None,
        });
    }
    if let Some(photo) = message.photo.last() {
        attachments.push(MessageAttachment {
            id: Some(photo.file_id.clone()),
            name: None,
            attachment_type: Some("image".to_string()),
            url: None,
            mime_type: None,
        });
    }

    attachments
}

fn chat_name(chat: &TelegramChat) -> String {
    chat.title
        .clone()
        .or_else(|| {
            let first = chat.first_name.clone().unwrap_or_default();
            let last = chat.last_name.clone().unwrap_or_default();
            let combined = format!("{first} {last}").trim().to_string();
            if combined.is_empty() {
                None
            } else {
                Some(combined)
            }
        })
        .or_else(|| chat.username.clone())
        .unwrap_or_else(|| chat.id.to_string())
}

fn chat_key(chat: &TelegramChat) -> String {
    chat.username
        .clone()
        .map(|username| format!("@{username}"))
        .unwrap_or_else(|| chat.id.to_string())
}

fn telegram_user_name(user: &TelegramUser) -> String {
    let combined = format!(
        "{} {}",
        user.first_name,
        user.last_name.clone().unwrap_or_default()
    )
    .trim()
    .to_string();

    if combined.is_empty() {
        user.username.clone().unwrap_or_else(|| user.id.to_string())
    } else {
        combined
    }
}

fn telegram_chat_type(chat: &TelegramChat) -> ChatType {
    match chat.kind.as_str() {
        "private" => ChatType::Direct,
        "group" | "supergroup" => ChatType::Group,
        _ => ChatType::Channel,
    }
}

fn matches_chat_type(chat_type: ChatType, filter: Option<ChatType>) -> bool {
    filter.map(|value| value == chat_type).unwrap_or(true)
}

fn matches_search(chat: &MessengerChat, search: Option<&str>) -> bool {
    let Some(search) = search else {
        return true;
    };
    let search = search.trim();
    if search.is_empty() {
        return true;
    }

    let haystacks = [chat.name.to_lowercase(), chat.key.to_lowercase()];
    haystacks.iter().any(|value| value.contains(search))
}

fn matches_message_window(
    message: &TelegramMessage,
    since: Option<i64>,
    until: Option<i64>,
) -> bool {
    if since.is_some_and(|since| message.date < since) {
        return false;
    }
    if until.is_some_and(|until| message.date > until) {
        return false;
    }
    true
}

fn matches_thread(message: &TelegramMessage, thread_id: Option<i64>) -> bool {
    let Some(thread_id) = thread_id else {
        return true;
    };

    message.message_thread_id == Some(thread_id)
        || message
            .reply_to_message
            .as_ref()
            .map(|reply| reply.message_id == thread_id)
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;

    fn token(value: &str) -> SecretString {
        SecretString::from(value.to_string())
    }

    #[tokio::test]
    async fn get_chats_maps_unique_chats_from_updates() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/botbot-token/getUpdates")
                .query_param("timeout", "0")
                .query_param("limit", "100");
            then.status(200).json_body_obj(&serde_json::json!({
                "ok": true,
                "result": [
                    {
                        "update_id": 10,
                        "message": {
                            "message_id": 1,
                            "date": 1710000000,
                            "chat": { "id": 100, "type": "private", "first_name": "Ada" },
                            "from": { "id": 1, "is_bot": false, "first_name": "Ada" },
                            "text": "hello"
                        }
                    },
                    {
                        "update_id": 11,
                        "message": {
                            "message_id": 2,
                            "date": 1710000001,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 2, "is_bot": false, "first_name": "Bob" },
                            "text": "hi"
                        }
                    },
                    {
                        "update_id": 12,
                        "message": {
                            "message_id": 3,
                            "date": 1710000002,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 2, "is_bot": false, "first_name": "Bob" },
                            "text": "again"
                        }
                    }
                ]
            }));
        });

        let result = TelegramClient::new(token("bot-token"))
            .with_base_url(server.base_url())
            .get_chats(GetChatsParams {
                limit: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].chat_type, ChatType::Direct);
        assert_eq!(result.items[1].chat_type, ChatType::Group);
        assert!(!result.pagination.unwrap().has_more);
    }

    #[tokio::test]
    async fn get_messages_filters_chat_and_maps_reply_fields() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/botbot-token/getUpdates")
                .query_param("timeout", "0")
                .query_param("limit", "100");
            then.status(200).json_body_obj(&serde_json::json!({
                "ok": true,
                "result": [
                    {
                        "update_id": 20,
                        "message": {
                            "message_id": 7,
                            "date": 1710000100,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 5, "is_bot": false, "first_name": "K" },
                            "text": "root"
                        }
                    },
                    {
                        "update_id": 21,
                        "edited_message": {
                            "message_id": 8,
                            "date": 1710000200,
                            "edit_date": 1710000300,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 6, "is_bot": false, "first_name": "L" },
                            "text": "reply",
                            "reply_to_message": { "message_id": 7 }
                        }
                    },
                    {
                        "update_id": 22,
                        "message": {
                            "message_id": 9,
                            "date": 1710000400,
                            "chat": { "id": 123, "type": "private", "first_name": "Else" },
                            "from": { "id": 9, "is_bot": false, "first_name": "Else" },
                            "text": "other chat"
                        }
                    }
                ]
            }));
        });

        let result = TelegramClient::new(token("bot-token"))
            .with_base_url(server.base_url())
            .get_messages(GetMessagesParams {
                chat_id: "-200".to_string(),
                limit: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[1].reply_to_id.as_deref(), Some("7"));
        assert_eq!(result.items[1].thread_id.as_deref(), Some("7"));
        assert!(result.items[1].is_edited);
    }

    #[tokio::test]
    async fn send_message_maps_response() {
        let server = MockServer::start();
        let send_message = server.mock(|when, then| {
            when.method(POST).path("/botbot-token/sendMessage");
            then.status(200).json_body_obj(&serde_json::json!({
                "ok": true,
                "result": {
                    "message_id": 30,
                    "date": 1710000500,
                    "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                    "from": { "id": 99, "first_name": "Devboy", "username": "devboy_bot" },
                    "text": "hello *team*"
                }
            }));
        });

        let result = TelegramClient::new(token("bot-token"))
            .with_base_url(server.base_url())
            .send_message(SendMessageParams {
                chat_id: "-200".to_string(),
                text: "hello *team*".to_string(),
                thread_id: None,
                reply_to_id: None,
                attachments: vec![],
            })
            .await
            .unwrap();

        send_message.assert_calls(1);
        assert_eq!(result.chat_id, "-200");
        assert_eq!(result.text, "hello *team*");
        assert_eq!(result.author.name, "Devboy");
        assert!(!result.is_edited);
    }

    #[tokio::test]
    async fn send_message_includes_thread_and_reply_ids() {
        let server = MockServer::start();
        let send_message = server.mock(|when, then| {
            when.method(POST).path("/botbot-token/sendMessage");
            then.status(200).json_body_obj(&serde_json::json!({
                "ok": true,
                "result": {
                    "message_id": 31,
                    "date": 1710000600,
                    "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                    "from": { "id": 99, "first_name": "Devboy", "username": "devboy_bot" },
                    "text": "reply",
                    "message_thread_id": 123,
                    "reply_to_message": { "message_id": 7 }
                }
            }));
        });

        let result = TelegramClient::new(token("bot-token"))
            .with_base_url(server.base_url())
            .send_message(SendMessageParams {
                chat_id: "-200".to_string(),
                text: "reply".to_string(),
                thread_id: Some("123".to_string()),
                reply_to_id: Some("7".to_string()),
                attachments: vec![],
            })
            .await
            .unwrap();

        send_message.assert_calls(1);
        assert_eq!(result.thread_id.as_deref(), Some("123"));
        assert_eq!(result.reply_to_id.as_deref(), Some("7"));
    }

    #[tokio::test]
    async fn search_messages_filters_by_query_and_chat() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/botbot-token/getUpdates")
                .query_param("timeout", "0")
                .query_param("limit", "100");
            then.status(200).json_body_obj(&serde_json::json!({
                "ok": true,
                "result": [
                    {
                        "update_id": 40,
                        "message": {
                            "message_id": 100,
                            "date": 1710001000,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 5, "is_bot": false, "first_name": "K" },
                            "text": "deploy failed on prod"
                        }
                    },
                    {
                        "update_id": 41,
                        "message": {
                            "message_id": 101,
                            "date": 1710001100,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 6, "is_bot": false, "first_name": "L" },
                            "text": "all green now"
                        }
                    },
                    {
                        "update_id": 42,
                        "message": {
                            "message_id": 102,
                            "date": 1710001200,
                            "chat": { "id": 123, "type": "private", "first_name": "Else" },
                            "from": { "id": 9, "is_bot": false, "first_name": "Else" },
                            "text": "deploy failed elsewhere"
                        }
                    }
                ]
            }));
        });

        let result = TelegramClient::new(token("bot-token"))
            .with_base_url(server.base_url())
            .search_messages(SearchMessagesParams {
                query: "DEPLOY FAILED".to_string(),
                chat_id: Some("-200".to_string()),
                limit: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "100");
        assert_eq!(result.items[0].chat_id, "-200");
        assert_eq!(result.items[0].text, "deploy failed on prod");
        assert!(!result.pagination.unwrap().has_more);
    }

    #[tokio::test]
    async fn search_messages_returns_cursor_for_follow_up_page() {
        let first_server = MockServer::start();
        first_server.mock(|when, then| {
            when.method(GET)
                .path("/botbot-token/getUpdates")
                .query_param("timeout", "0")
                .query_param("limit", "100");
            then.status(200).json_body_obj(&serde_json::json!({
                "ok": true,
                "result": [
                    {
                        "update_id": 50,
                        "message": {
                            "message_id": 200,
                            "date": 1710002000,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 5, "is_bot": false, "first_name": "K" },
                            "text": "deploy failed alpha"
                        }
                    },
                    {
                        "update_id": 51,
                        "message": {
                            "message_id": 201,
                            "date": 1710002100,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 6, "is_bot": false, "first_name": "L" },
                            "text": "deploy failed beta"
                        }
                    }
                ]
            }));
        });

        let client = TelegramClient::new(token("bot-token")).with_base_url(first_server.base_url());
        let first = client
            .search_messages(SearchMessagesParams {
                query: "deploy failed".to_string(),
                limit: Some(1),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(first.items.len(), 1);
        let first_pagination = first.pagination.unwrap();
        assert!(first_pagination.has_more);
        let cursor = first_pagination.next_cursor.unwrap();
        let state = parse_search_cursor(Some(&cursor)).unwrap();
        assert_eq!(state.next_update_offset, Some(51));

        let second_server = MockServer::start();
        second_server.mock(|when, then| {
            when.method(GET)
                .path("/botbot-token/getUpdates")
                .query_param("timeout", "0")
                .query_param("limit", "100")
                .query_param("offset", "51");
            then.status(200).json_body_obj(&serde_json::json!({
                "ok": true,
                "result": [
                    {
                        "update_id": 51,
                        "message": {
                            "message_id": 201,
                            "date": 1710002100,
                            "chat": { "id": -200, "type": "supergroup", "title": "Eng" },
                            "from": { "id": 6, "is_bot": false, "first_name": "L" },
                            "text": "deploy failed beta"
                        }
                    }
                ]
            }));
        });

        let second = TelegramClient::new(token("bot-token"))
            .with_base_url(second_server.base_url())
            .search_messages(SearchMessagesParams {
                query: "deploy failed".to_string(),
                limit: Some(1),
                cursor: Some(cursor),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].id, "201");
        assert!(!second.pagination.unwrap().has_more);
    }

    #[tokio::test]
    async fn search_messages_rejects_empty_query() {
        let err = TelegramClient::new(token("bot-token"))
            .search_messages(SearchMessagesParams {
                query: "   ".to_string(),
                ..Default::default()
            })
            .await
            .unwrap_err();

        assert!(matches!(err, Error::InvalidData(message) if message.contains("must not be empty")));
    }

    #[tokio::test]
    async fn send_message_rejects_attachments() {
        let err = TelegramClient::new(token("bot-token"))
            .send_message(SendMessageParams {
                chat_id: "-200".to_string(),
                text: "reply".to_string(),
                thread_id: None,
                reply_to_id: None,
                attachments: vec![MessageAttachment {
                    id: Some("att-1".to_string()),
                    name: Some("report.txt".to_string()),
                    attachment_type: None,
                    url: None,
                    mime_type: None,
                }],
            })
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            Error::ProviderUnsupported { provider, operation }
            if provider == "telegram" && operation == "send_message attachments"
        ));
    }
}
