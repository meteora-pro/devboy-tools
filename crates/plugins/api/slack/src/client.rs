use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use devboy_core::types::ChatType;
use devboy_core::{
    Error, GetChatsParams, GetMessagesParams, MessageAttachment, MessageAuthor, MessengerChat,
    MessengerMessage, MessengerProvider, Pagination, ProviderResult, Result, SearchMessagesParams,
    SendMessageParams,
};
use reqwest::header::HeaderMap;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Instant, sleep_until};
use tracing::debug;

use crate::DEFAULT_SLACK_API_URL;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackAuthInfo {
    pub user_id: String,
    pub user_name: Option<String>,
    pub team_id: String,
    pub team_name: String,
    pub bot_id: Option<String>,
    pub url: Option<String>,
    pub scopes: Vec<String>,
    pub missing_scopes: Vec<String>,
}

#[derive(Clone)]
pub struct SlackClient {
    token: SecretString,
    base_url: String,
    http: reqwest::Client,
    required_scopes: Vec<String>,
    user_cache: Arc<RwLock<HashMap<String, MessageAuthor>>>,
    rate_limiter: Arc<SlackRateLimiter>,
}

impl fmt::Debug for SlackClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackClient")
            .field("token", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("http", &self.http)
            .field("required_scopes", &self.required_scopes)
            .field("user_cache", &self.user_cache)
            .field("rate_limiter", &self.rate_limiter)
            .finish()
    }
}

const SLACK_READ_INTERVAL: Duration = Duration::from_millis(1200);
const SLACK_WRITE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlackRateLimitBucket {
    Read,
    Write,
}

#[derive(Debug)]
struct SlackRateLimiter {
    state: Mutex<SlackRateLimitState>,
    read_interval: Duration,
    write_interval: Duration,
}

#[derive(Debug)]
struct SlackRateLimitState {
    read_ready_at: Instant,
    write_ready_at: Instant,
}

#[derive(Debug, Deserialize)]
struct SlackAuthTestResponse {
    ok: bool,
    error: Option<String>,
    url: Option<String>,
    team: Option<String>,
    user: Option<String>,
    #[serde(rename = "team_id")]
    team_id: Option<String>,
    #[serde(rename = "user_id")]
    user_id: Option<String>,
    #[serde(rename = "bot_id")]
    bot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackResponseMetadata {
    #[serde(default)]
    next_cursor: String,
}

#[derive(Debug, Deserialize)]
struct SlackConversationsListResponse {
    ok: bool,
    error: Option<String>,
    #[serde(default)]
    channels: Vec<SlackConversation>,
    response_metadata: Option<SlackResponseMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackConversation {
    id: String,
    name: Option<String>,
    user: Option<String>,
    is_group: Option<bool>,
    is_im: Option<bool>,
    is_mpim: Option<bool>,
    is_archived: Option<bool>,
    num_members: Option<u32>,
    purpose: Option<SlackTextValue>,
    topic: Option<SlackTextValue>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackTextValue {
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackMessagesResponse {
    ok: bool,
    error: Option<String>,
    #[serde(default)]
    messages: Vec<SlackMessage>,
    has_more: Option<bool>,
    response_metadata: Option<SlackResponseMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SlackSearchCursor {
    version: u8,
    current_chat_id: Option<String>,
    current_message_cursor: Option<String>,
    #[serde(default)]
    current_message_offset: usize,
    #[serde(default)]
    pending_chat_ids: Vec<String>,
    #[serde(default)]
    pending_chat_index: usize,
    next_chats_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackPostMessageResponse {
    ok: bool,
    error: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
    message: Option<SlackMessage>,
}

#[derive(Debug, Deserialize)]
struct SlackUsersInfoResponse {
    ok: bool,
    error: Option<String>,
    user: Option<SlackUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackUser {
    id: String,
    name: Option<String>,
    profile: Option<SlackUserProfile>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackUserProfile {
    real_name: Option<String>,
    display_name: Option<String>,
    image_72: Option<String>,
    /// Email — present on `users.info` + `users.lookupByEmail` when the
    /// app has the `users:read.email` scope. Issue #177.
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackMessage {
    ts: String,
    text: Option<String>,
    user: Option<String>,
    username: Option<String>,
    bot_id: Option<String>,
    thread_ts: Option<String>,
    parent_user_id: Option<String>,
    subtype: Option<String>,
    edited: Option<serde_json::Value>,
    files: Option<Vec<SlackFile>>,
    attachments: Option<Vec<SlackRichAttachment>>,
    bot_profile: Option<SlackBotProfile>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackBotProfile {
    id: Option<String>,
    name: Option<String>,
    icons: Option<SlackBotIcons>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackBotIcons {
    image_72: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackFile {
    id: Option<String>,
    name: Option<String>,
    mimetype: Option<String>,
    filetype: Option<String>,
    size: Option<u64>,
    url_private: Option<String>,
    permalink: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackRichAttachment {
    id: Option<u64>,
    title: Option<String>,
    fallback: Option<String>,
    service_name: Option<String>,
    title_link: Option<String>,
    from_url: Option<String>,
}

impl SlackClient {
    pub fn new(token: SecretString) -> Self {
        Self {
            token,
            base_url: DEFAULT_SLACK_API_URL.to_string(),
            http: reqwest::Client::new(),
            required_scopes: devboy_core::default_slack_required_scopes(),
            user_cache: Arc::new(RwLock::new(HashMap::new())),
            rate_limiter: Arc::new(SlackRateLimiter::new(
                SLACK_READ_INTERVAL,
                SLACK_WRITE_INTERVAL,
            )),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_required_scopes(mut self, required_scopes: Vec<String>) -> Self {
        self.required_scopes = required_scopes;
        self
    }

    pub fn required_scopes(&self) -> &[String] {
        &self.required_scopes
    }

    pub async fn auth_info(&self) -> Result<SlackAuthInfo> {
        let url = format!("{}/auth.test", self.base_url);
        debug!(url, "slack auth.test request");

        let response = self.send_form_request("auth.test", &[]).await?;
        let headers = response.headers().clone();
        let payload: SlackAuthTestResponse = map_http_error(response).await?;

        if !payload.ok {
            let message = payload
                .error
                .unwrap_or_else(|| "unknown_slack_error".to_string());
            return Err(match message.as_str() {
                "invalid_auth" | "not_authed" => Error::Unauthorized(message),
                "missing_scope" => Error::Forbidden(message),
                _ => Error::Api {
                    status: 200,
                    message,
                },
            });
        }

        let scopes = parse_scopes(&headers);
        let missing_scopes = self
            .required_scopes
            .iter()
            .filter(|scope| !scopes.iter().any(|actual| actual == *scope))
            .cloned()
            .collect();

        Ok(SlackAuthInfo {
            user_id: payload.user_id.unwrap_or_default(),
            user_name: payload.user,
            team_id: payload.team_id.unwrap_or_default(),
            team_name: payload.team.unwrap_or_default(),
            bot_id: payload.bot_id,
            url: payload.url,
            scopes,
            missing_scopes,
        })
    }

    pub async fn ensure_healthy(&self) -> Result<SlackAuthInfo> {
        let info = self.auth_info().await?;
        if info.missing_scopes.is_empty() {
            Ok(info)
        } else {
            Err(Error::Forbidden(format!(
                "Slack token is missing required scopes: {}",
                info.missing_scopes.join(", ")
            )))
        }
    }

    async fn post_form<T>(&self, method: &str, params: &[(&str, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let response = self.send_form_request(method, params).await?;
        map_http_error(response).await
    }

    async fn send_form_request(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<reqwest::Response> {
        let url = format!("{}/{}", self.base_url, method);
        let bucket = slack_rate_limit_bucket(method);
        debug!(url, ?bucket, "slack api request");

        for attempt in 0..2 {
            self.rate_limiter.acquire(bucket).await;

            let response = self
                .http
                .post(&url)
                .bearer_auth(self.token.expose_secret())
                .form(params)
                .send()
                .await
                .map_err(|e| Error::Network(e.to_string()))?;

            if response.status().as_u16() != 429 {
                return Ok(response);
            }

            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());

            self.rate_limiter
                .on_rate_limited(bucket, retry_after.map(Duration::from_secs))
                .await;

            if attempt == 0 && retry_after.is_some() {
                continue;
            }

            return Ok(response);
        }

        unreachable!("slack request retry loop should always return");
    }

    async fn get_conversations(
        &self,
        params: &GetChatsParams,
    ) -> Result<ProviderResult<MessengerChat>> {
        let limit = params.limit.unwrap_or(100).min(1000);
        let mut form = vec![
            ("limit", limit.to_string()),
            (
                "types",
                slack_conversation_types(params.chat_type).to_string(),
            ),
            (
                "exclude_archived",
                (!params.include_inactive.unwrap_or(false)).to_string(),
            ),
        ];
        if let Some(cursor) = params.cursor.as_ref() {
            form.push(("cursor", cursor.clone()));
        }

        let payload: SlackConversationsListResponse =
            self.post_form("conversations.list", &form).await?;
        ensure_ok(payload.ok, payload.error)?;

        let mut items: Vec<_> = payload
            .channels
            .into_iter()
            .filter(|chat| matches_chat_filter(chat, params))
            .map(map_chat)
            .collect();

        if let Some(limit) = params.limit {
            items.truncate(limit as usize);
        }

        let has_more = payload
            .response_metadata
            .as_ref()
            .map(|meta| !meta.next_cursor.is_empty())
            .unwrap_or(false);
        let next_cursor = slack_next_cursor(payload.response_metadata.as_ref());

        Ok(ProviderResult::new(items).with_pagination(Pagination {
            offset: 0,
            limit,
            total: None,
            has_more,
            next_cursor,
        }))
    }

    async fn get_messages_page(
        &self,
        params: &GetMessagesParams,
    ) -> Result<ProviderResult<MessengerMessage>> {
        let limit = params.limit.unwrap_or(100).min(1000);
        let mut form = vec![
            ("channel", params.chat_id.clone()),
            ("limit", limit.to_string()),
            ("inclusive", "true".to_string()),
        ];
        if let Some(cursor) = params.cursor.as_ref() {
            form.push(("cursor", cursor.clone()));
        }
        if let Some(since) = normalize_ts_param("since", params.since.as_deref())? {
            form.push(("oldest", since));
        }
        if let Some(until) = normalize_ts_param("until", params.until.as_deref())? {
            form.push(("latest", until));
        }

        let payload: SlackMessagesResponse = if let Some(thread_id) = params.thread_id.as_ref() {
            form.push(("ts", thread_id.clone()));
            self.post_form("conversations.replies", &form).await?
        } else {
            self.post_form("conversations.history", &form).await?
        };
        ensure_ok(payload.ok, payload.error)?;

        let mut items = Vec::with_capacity(payload.messages.len());
        for message in payload.messages {
            items.push(self.map_message(&params.chat_id, message).await?);
        }

        let has_more = payload.has_more.unwrap_or(false)
            || payload
                .response_metadata
                .as_ref()
                .map(|meta| !meta.next_cursor.is_empty())
                .unwrap_or(false);
        let next_cursor = slack_next_cursor(payload.response_metadata.as_ref());

        Ok(ProviderResult::new(items).with_pagination(Pagination {
            offset: 0,
            limit,
            total: None,
            has_more,
            next_cursor,
        }))
    }

    async fn map_message(&self, chat_id: &str, message: SlackMessage) -> Result<MessengerMessage> {
        let ts = message.ts.clone();
        let thread_id = message.thread_ts.clone();
        let reply_to_id = thread_id.as_ref().filter(|thread| *thread != &ts).cloned();

        Ok(MessengerMessage {
            id: ts.clone(),
            chat_id: chat_id.to_string(),
            text: normalize_mrkdwn(message.text.as_deref().unwrap_or_default()),
            author: self.resolve_author(&message).await?,
            source: "slack".to_string(),
            timestamp: ts,
            thread_id,
            reply_to_id,
            attachments: map_attachments(&message),
            is_edited: message.edited.is_some(),
        })
    }

    async fn resolve_author(&self, message: &SlackMessage) -> Result<MessageAuthor> {
        if let Some(user_id) = message.user.as_deref() {
            return self.get_user(user_id).await;
        }

        if let Some(bot_profile) = message.bot_profile.as_ref() {
            return Ok(MessageAuthor {
                id: bot_profile
                    .id
                    .clone()
                    .or_else(|| message.bot_id.clone())
                    .unwrap_or_else(|| "slack-bot".to_string()),
                name: bot_profile
                    .name
                    .clone()
                    .or_else(|| message.username.clone())
                    .unwrap_or_else(|| "Slack Bot".to_string()),
                username: message.username.clone(),
                avatar_url: bot_profile
                    .icons
                    .as_ref()
                    .and_then(|icons| icons.image_72.clone()),
            });
        }

        Ok(MessageAuthor {
            id: message
                .bot_id
                .clone()
                .or_else(|| message.parent_user_id.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            name: message
                .username
                .clone()
                .or_else(|| message.subtype.clone())
                .unwrap_or_else(|| "Unknown".to_string()),
            username: message.username.clone(),
            avatar_url: None,
        })
    }

    async fn get_user(&self, user_id: &str) -> Result<MessageAuthor> {
        if let Some(cached) = self.user_cache.read().await.get(user_id).cloned() {
            return Ok(cached);
        }

        let payload: SlackUsersInfoResponse = self
            .post_form("users.info", &[("user", user_id.to_string())])
            .await?;
        ensure_ok(payload.ok, payload.error)?;

        let user = payload
            .user
            .ok_or_else(|| Error::InvalidData("Slack users.info returned no user".to_string()))?;
        let profile = user.profile.as_ref();
        let display_name = profile
            .and_then(|profile| profile.display_name.clone())
            .filter(|name| !name.is_empty());
        let real_name = profile
            .and_then(|profile| profile.real_name.clone())
            .filter(|name| !name.is_empty());
        let username = user.name.filter(|name| !name.is_empty());
        let name = display_name
            .clone()
            .or(real_name)
            .or_else(|| username.clone())
            .unwrap_or_else(|| user.id.clone());

        let author = MessageAuthor {
            id: user.id,
            name,
            username,
            avatar_url: profile.and_then(|profile| profile.image_72.clone()),
        };

        self.user_cache
            .write()
            .await
            .insert(user_id.to_string(), author.clone());

        Ok(author)
    }
}

#[async_trait]
impl MessengerProvider for SlackClient {
    fn provider_name(&self) -> &'static str {
        "slack"
    }

    async fn get_chats(&self, params: GetChatsParams) -> Result<ProviderResult<MessengerChat>> {
        self.get_conversations(&params).await
    }

    async fn get_messages(
        &self,
        params: GetMessagesParams,
    ) -> Result<ProviderResult<MessengerMessage>> {
        self.get_messages_page(&params).await
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

        let limit = params.limit.unwrap_or(20) as usize;
        let mut found = Vec::new();

        let (has_more, next_cursor) = if let Some(chat_id) = params.chat_id.as_ref() {
            let mut state = parse_search_cursor(params.cursor.as_deref())?;
            if state.current_chat_id.is_none() {
                state.current_chat_id = Some(chat_id.clone());
            }
            if state.current_chat_id.as_deref() != Some(chat_id.as_str()) {
                state = SlackSearchCursor {
                    current_chat_id: Some(chat_id.clone()),
                    ..SlackSearchCursor::default()
                };
            }
            if state.current_message_cursor.is_none()
                && state.current_message_offset == 0
                && state.next_chats_cursor.is_some()
                && !has_pending_chat_ids(&state)
            {
                state.current_message_cursor = state.next_chats_cursor.take();
            }

            loop {
                let messages = self
                    .get_messages_page(&GetMessagesParams {
                        chat_id: chat_id.clone(),
                        limit: Some(params.limit.unwrap_or(100)),
                        cursor: state.current_message_cursor.clone(),
                        thread_id: None,
                        since: params.since.clone(),
                        until: params.until.clone(),
                    })
                    .await?;
                let pagination = messages.pagination.clone();
                let page_len = messages.items.len();

                for message in messages
                    .items
                    .into_iter()
                    .skip(state.current_message_offset)
                {
                    state.current_message_offset += 1;
                    if message.text.to_lowercase().contains(&query) {
                        found.push(message);
                        if found.len() >= limit {
                            break;
                        }
                    }
                }

                let page_has_more = pagination.as_ref().map(|p| p.has_more).unwrap_or(false);
                let next_page_cursor = pagination.as_ref().and_then(|p| p.next_cursor.clone());

                if found.len() >= limit {
                    if state.current_message_offset >= page_len {
                        if page_has_more {
                            state.current_message_cursor = next_page_cursor;
                            state.current_message_offset = 0;
                        } else {
                            state.current_chat_id = None;
                            state.current_message_cursor = None;
                            state.current_message_offset = 0;
                        }
                    }
                    break;
                }

                if page_has_more {
                    state.current_message_cursor = next_page_cursor;
                    state.current_message_offset = 0;
                    continue;
                }

                state.current_chat_id = None;
                state.current_message_cursor = None;
                state.current_message_offset = 0;
                break;
            }

            let has_more = state.current_chat_id.is_some();
            let next_cursor = serialize_search_cursor(&state)?;
            (has_more, next_cursor)
        } else {
            let mut state = parse_search_cursor(params.cursor.as_deref())?;
            let mut chats_loaded = state.current_chat_id.is_some()
                || has_pending_chat_ids(&state)
                || state.next_chats_cursor.is_some()
                || params.cursor.is_some();

            loop {
                if found.len() >= limit {
                    break;
                }

                if let Some(chat_id) = state.current_chat_id.clone() {
                    let messages = self
                        .get_messages_page(&GetMessagesParams {
                            chat_id,
                            limit: Some(100),
                            cursor: state.current_message_cursor.clone(),
                            thread_id: None,
                            since: params.since.clone(),
                            until: params.until.clone(),
                        })
                        .await?;
                    let pagination = messages.pagination.clone();
                    let page_len = messages.items.len();

                    for message in messages
                        .items
                        .into_iter()
                        .skip(state.current_message_offset)
                    {
                        state.current_message_offset += 1;
                        if message.text.to_lowercase().contains(&query) {
                            found.push(message);
                            if found.len() >= limit {
                                break;
                            }
                        }
                    }

                    if found.len() >= limit {
                        if state.current_message_offset >= page_len {
                            let next_message_cursor = pagination
                                .as_ref()
                                .and_then(|page| page.next_cursor.clone());
                            if pagination
                                .as_ref()
                                .map(|page| page.has_more)
                                .unwrap_or(false)
                            {
                                state.current_message_cursor = next_message_cursor;
                                state.current_message_offset = 0;
                            } else {
                                state.current_chat_id = None;
                                state.current_message_cursor = None;
                                state.current_message_offset = 0;
                            }
                        }
                        break;
                    }

                    let next_message_cursor = pagination
                        .as_ref()
                        .and_then(|page| page.next_cursor.clone());
                    if pagination
                        .as_ref()
                        .map(|page| page.has_more)
                        .unwrap_or(false)
                    {
                        state.current_message_cursor = next_message_cursor;
                        state.current_message_offset = 0;
                        continue;
                    }

                    state.current_chat_id = None;
                    state.current_message_cursor = None;
                    state.current_message_offset = 0;
                    continue;
                }

                if let Some(next_chat_id) = take_next_pending_chat_id(&mut state) {
                    state.current_chat_id = Some(next_chat_id);
                    state.current_message_cursor = None;
                    state.current_message_offset = 0;
                    continue;
                }

                if chats_loaded && state.next_chats_cursor.is_none() {
                    break;
                }

                let chats = self
                    .get_conversations(&GetChatsParams {
                        search: None,
                        chat_type: None,
                        limit: Some(100),
                        cursor: state.next_chats_cursor.clone(),
                        include_inactive: Some(false),
                    })
                    .await?;
                state.pending_chat_ids = chats.items.into_iter().map(|chat| chat.id).collect();
                state.pending_chat_index = 0;
                state.next_chats_cursor = chats.pagination.and_then(|page| page.next_cursor);
                chats_loaded = true;
            }

            let has_more = state.current_chat_id.is_some()
                || has_pending_chat_ids(&state)
                || state.next_chats_cursor.is_some();
            let next_cursor = serialize_search_cursor(&state)?;
            (has_more, next_cursor)
        };

        Ok(ProviderResult::new(found).with_pagination(Pagination {
            offset: 0,
            limit: limit as u32,
            total: None,
            has_more,
            next_cursor,
        }))
    }

    async fn send_message(&self, params: SendMessageParams) -> Result<MessengerMessage> {
        if !params.attachments.is_empty() {
            return Err(Error::ProviderUnsupported {
                provider: self.provider_name().to_string(),
                operation: "send_message attachments".to_string(),
            });
        }

        let thread_ts = params
            .thread_id
            .clone()
            .or_else(|| params.reply_to_id.clone());
        let mut form = vec![
            ("channel", params.chat_id.clone()),
            ("text", params.text.clone()),
            ("unfurl_links", "false".to_string()),
            ("unfurl_media", "false".to_string()),
        ];
        if let Some(thread_id) = thread_ts.as_ref() {
            form.push(("thread_ts", thread_id.clone()));
        }

        let payload: SlackPostMessageResponse = self.post_form("chat.postMessage", &form).await?;
        ensure_ok(payload.ok, payload.error)?;

        let mut message = payload.message.unwrap_or(SlackMessage {
            ts: payload.ts.clone().unwrap_or_default(),
            text: Some(params.text),
            user: None,
            username: None,
            bot_id: None,
            thread_ts: thread_ts.clone(),
            parent_user_id: None,
            subtype: None,
            edited: None,
            files: None,
            attachments: None,
            bot_profile: None,
        });

        if message.thread_ts.is_none() {
            message.thread_ts = thread_ts;
        }

        self.map_message(
            payload.channel.as_deref().unwrap_or(&params.chat_id),
            message,
        )
        .await
    }
}

fn parse_search_cursor(cursor: Option<&str>) -> Result<SlackSearchCursor> {
    let Some(cursor) = cursor.map(str::trim).filter(|cursor| !cursor.is_empty()) else {
        return Ok(SlackSearchCursor::default());
    };

    match serde_json::from_str::<SlackSearchCursor>(cursor) {
        Ok(state) => Ok(state),
        Err(_) => Ok(SlackSearchCursor {
            next_chats_cursor: Some(cursor.to_string()),
            ..SlackSearchCursor::default()
        }),
    }
}

fn serialize_search_cursor(state: &SlackSearchCursor) -> Result<Option<String>> {
    let has_more = state.current_chat_id.is_some()
        || has_pending_chat_ids(state)
        || state.next_chats_cursor.is_some();
    if !has_more {
        return Ok(None);
    }

    serde_json::to_string(state)
        .map(Some)
        .map_err(|e| Error::InvalidData(format!("failed to serialize Slack search cursor: {e}")))
}

fn map_chat(chat: SlackConversation) -> MessengerChat {
    let name = conversation_name(&chat);
    let description = chat
        .purpose
        .as_ref()
        .and_then(|value| value.value.clone())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            chat.topic
                .as_ref()
                .and_then(|value| value.value.clone())
                .filter(|value| !value.is_empty())
        });

    MessengerChat {
        id: chat.id.clone(),
        key: format!("slack:{}", chat.id),
        name,
        chat_type: slack_chat_type(&chat),
        source: "slack".to_string(),
        member_count: chat.num_members,
        description,
        is_active: !chat.is_archived.unwrap_or(false),
    }
}

fn map_attachments(message: &SlackMessage) -> Vec<MessageAttachment> {
    let mut attachments = Vec::new();

    if let Some(files) = message.files.as_ref() {
        attachments.extend(files.iter().map(|file| MessageAttachment {
            id: file.id.clone(),
            name: file.name.clone(),
            attachment_type: file.filetype.clone().or_else(|| Some("file".to_string())),
            url: file.permalink.clone().or_else(|| file.url_private.clone()),
            mime_type: file.mimetype.clone(),
            file_size: file.size,
        }));
    }

    if let Some(rich_attachments) = message.attachments.as_ref() {
        attachments.extend(rich_attachments.iter().map(|attachment| {
            MessageAttachment {
                id: attachment.id.map(|id| id.to_string()),
                name: attachment
                    .title
                    .clone()
                    .or_else(|| attachment.fallback.clone()),
                attachment_type: attachment.service_name.clone(),
                url: attachment
                    .title_link
                    .clone()
                    .or_else(|| attachment.from_url.clone()),
                mime_type: None,
                file_size: None,
            }
        }));
    }

    attachments
}

fn matches_chat_filter(chat: &SlackConversation, params: &GetChatsParams) -> bool {
    if let Some(expected) = params.chat_type
        && slack_chat_type(chat) != expected
    {
        return false;
    }

    if let Some(search) = params.search.as_deref() {
        let needle = search.to_lowercase();
        let haystack = conversation_name(chat).to_lowercase();
        if !haystack.contains(&needle) {
            return false;
        }
    }

    if !params.include_inactive.unwrap_or(false) && chat.is_archived.unwrap_or(false) {
        return false;
    }

    true
}

fn slack_chat_type(chat: &SlackConversation) -> ChatType {
    if chat.is_im.unwrap_or(false) {
        ChatType::Direct
    } else if chat.is_group.unwrap_or(false) || chat.is_mpim.unwrap_or(false) {
        ChatType::Group
    } else {
        ChatType::Channel
    }
}

fn conversation_name(chat: &SlackConversation) -> String {
    chat.name
        .clone()
        .filter(|name| !name.is_empty())
        .or_else(|| chat.user.clone().map(|user| format!("dm-{}", user)))
        .unwrap_or_else(|| chat.id.clone())
}

fn slack_conversation_types(chat_type: Option<ChatType>) -> &'static str {
    match chat_type {
        Some(ChatType::Direct) => "im",
        Some(ChatType::Group) => "mpim,private_channel",
        Some(ChatType::Channel) => "public_channel,private_channel",
        None => "public_channel,private_channel,mpim,im",
    }
}

fn slack_next_cursor(metadata: Option<&SlackResponseMetadata>) -> Option<String> {
    metadata
        .map(|metadata| metadata.next_cursor.trim())
        .filter(|cursor| !cursor.is_empty())
        .map(ToOwned::to_owned)
}

fn slack_rate_limit_bucket(method: &str) -> SlackRateLimitBucket {
    match method {
        "chat.postMessage" => SlackRateLimitBucket::Write,
        _ => SlackRateLimitBucket::Read,
    }
}

fn normalize_ts_param(field_name: &str, value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.parse::<f64>().is_ok() {
        Ok(Some(value.to_string()))
    } else {
        Err(Error::InvalidData(format!(
            "{field_name} must be a Slack timestamp string"
        )))
    }
}

fn has_pending_chat_ids(state: &SlackSearchCursor) -> bool {
    state.pending_chat_index < state.pending_chat_ids.len()
}

fn take_next_pending_chat_id(state: &mut SlackSearchCursor) -> Option<String> {
    let next_chat_id = state
        .pending_chat_ids
        .get(state.pending_chat_index)
        .cloned()?;
    state.pending_chat_index += 1;
    if state.pending_chat_index >= state.pending_chat_ids.len() {
        state.pending_chat_ids.clear();
        state.pending_chat_index = 0;
    }
    Some(next_chat_id)
}

fn parse_scopes(headers: &HeaderMap) -> Vec<String> {
    headers
        .get("x-oauth-scopes")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|scope| !scope.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

impl SlackRateLimiter {
    fn new(read_interval: Duration, write_interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            state: Mutex::new(SlackRateLimitState {
                read_ready_at: now,
                write_ready_at: now,
            }),
            read_interval,
            write_interval,
        }
    }

    async fn acquire(&self, bucket: SlackRateLimitBucket) {
        let (wait_until, interval) = {
            let mut state = self.state.lock().await;
            let now = Instant::now();
            let (ready_at, interval) = match bucket {
                SlackRateLimitBucket::Read => (&mut state.read_ready_at, self.read_interval),
                SlackRateLimitBucket::Write => (&mut state.write_ready_at, self.write_interval),
            };
            let wait_until = (*ready_at).max(now);
            *ready_at = wait_until + interval;
            (wait_until, interval)
        };

        debug!(
            ?bucket,
            ?wait_until,
            ?interval,
            "slack rate limiter acquired slot"
        );

        if wait_until > Instant::now() {
            sleep_until(wait_until).await;
        }
    }

    async fn on_rate_limited(&self, bucket: SlackRateLimitBucket, retry_after: Option<Duration>) {
        let delay = retry_after.unwrap_or(match bucket {
            SlackRateLimitBucket::Read => self.read_interval,
            SlackRateLimitBucket::Write => self.write_interval,
        });
        let next_ready = Instant::now() + delay;
        let mut state = self.state.lock().await;
        let ready_at = match bucket {
            SlackRateLimitBucket::Read => &mut state.read_ready_at,
            SlackRateLimitBucket::Write => &mut state.write_ready_at,
        };
        if next_ready > *ready_at {
            *ready_at = next_ready;
        }
    }
}

async fn map_http_error<T>(response: reqwest::Response) -> Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());

    if status.as_u16() == 429 {
        return Err(Error::RateLimited { retry_after });
    }

    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(Error::from_status(status.as_u16(), text));
    }

    response
        .json()
        .await
        .map_err(|e| Error::InvalidData(e.to_string()))
}

fn ensure_ok(ok: bool, error: Option<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(map_slack_error(
            error.unwrap_or_else(|| "unknown_slack_error".to_string()),
        ))
    }
}

fn map_slack_error(message: String) -> Error {
    match message.as_str() {
        "invalid_auth" | "not_authed" => Error::Unauthorized(message),
        "missing_scope" | "not_allowed_token_type" => Error::Forbidden(message),
        "channel_not_found" | "user_not_found" => Error::NotFound(message),
        "ratelimited" => Error::RateLimited { retry_after: None },
        _ => Error::Api {
            status: 200,
            message,
        },
    }
}

fn normalize_mrkdwn(text: &str) -> String {
    let decoded = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">");

    let mut output = String::new();
    let mut chars = decoded.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            let mut token = String::new();
            let mut closed = false;
            for next in chars.by_ref() {
                if next == '>' {
                    closed = true;
                    break;
                }
                token.push(next);
            }

            if closed {
                output.push_str(&normalize_slack_token(&token));
            } else {
                output.push('<');
                output.push_str(&token);
            }
        } else {
            output.push(ch);
        }
    }

    output
}

fn normalize_slack_token(token: &str) -> String {
    if let Some(user) = token.strip_prefix('@') {
        let mut parts = user.splitn(2, '|');
        let user_id = parts.next().unwrap_or(user);
        let label = parts
            .next()
            .filter(|label| !label.is_empty())
            .unwrap_or(user_id);
        return format!("@{}", label);
    }
    if let Some(rest) = token.strip_prefix('#') {
        let mut parts = rest.splitn(2, '|');
        let _ = parts.next();
        let label = parts.next().unwrap_or(rest);
        return format!("#{}", label);
    }
    if let Some(rest) = token.strip_prefix('!') {
        return rest.replace('|', " ");
    }
    if let Some((url, label)) = token.split_once('|') {
        return format!("[{}]({})", label, url);
    }
    token.to_string()
}

fn slack_user_to_core(user: SlackUser) -> devboy_core::User {
    let profile = user.profile.clone();
    let display = profile
        .as_ref()
        .and_then(|p| p.display_name.clone())
        .filter(|s| !s.is_empty());
    let real = profile
        .as_ref()
        .and_then(|p| p.real_name.clone())
        .filter(|s| !s.is_empty());
    let username_candidate = user.name.clone().filter(|s| !s.is_empty());
    let name = display
        .clone()
        .or(real.clone())
        .or(username_candidate.clone());
    devboy_core::User {
        id: user.id,
        username: username_candidate.unwrap_or_default(),
        name,
        email: profile.as_ref().and_then(|p| p.email.clone()),
        avatar_url: profile.and_then(|p| p.image_72),
    }
}

#[async_trait]
impl devboy_core::UserProvider for SlackClient {
    fn provider_name(&self) -> &'static str {
        "slack"
    }

    async fn get_user_profile(&self, user_id: &str) -> Result<devboy_core::User> {
        let payload: SlackUsersInfoResponse = self
            .post_form("users.info", &[("user", user_id.to_string())])
            .await?;
        ensure_ok(payload.ok, payload.error)?;
        let user = payload
            .user
            .ok_or_else(|| Error::InvalidData("Slack users.info returned no user".to_string()))?;
        Ok(slack_user_to_core(user))
    }

    async fn lookup_user_by_email(&self, email: &str) -> Result<Option<devboy_core::User>> {
        let payload: SlackUsersInfoResponse = self
            .post_form("users.lookupByEmail", &[("email", email.to_string())])
            .await?;
        if !payload.ok {
            // Slack returns `users_not_found` when the email simply has no
            // match — surface this as Ok(None) rather than an error so
            // callers can treat "no match" and "api error" distinctly.
            match payload.error.as_deref() {
                Some("users_not_found") => return Ok(None),
                _ => {
                    ensure_ok(payload.ok, payload.error)?;
                }
            }
        }
        Ok(payload.user.map(slack_user_to_core))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    fn token(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[tokio::test]
    async fn auth_info_reads_identity_and_scopes() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/auth.test");
            then.status(200)
                .header(
                    "x-oauth-scopes",
                    "channels:read, channels:history, groups:read, groups:history, im:read, im:history, mpim:read, mpim:history, chat:write, users:read",
                )
                .json_body(serde_json::json!({
                    "ok": true,
                    "url": "https://example.slack.com/",
                    "team": "Example",
                    "user": "devboy",
                    "team_id": "T123",
                    "user_id": "U123",
                    "bot_id": "B123"
                }));
        });

        let info = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .auth_info()
            .await
            .unwrap();

        assert_eq!(info.team_name, "Example");
        assert_eq!(info.user_id, "U123");
        assert!(info.missing_scopes.is_empty());
    }

    #[tokio::test]
    async fn ensure_healthy_fails_when_scopes_missing() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/auth.test");
            then.status(200)
                .header("x-oauth-scopes", "channels:read")
                .json_body(serde_json::json!({
                    "ok": true,
                    "team": "Example",
                    "team_id": "T123",
                    "user_id": "U123"
                }));
        });

        let error = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .ensure_healthy()
            .await
            .unwrap_err();

        assert!(error.to_string().contains("missing required scopes"));
    }

    #[tokio::test]
    async fn get_chats_maps_slack_conversations() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/conversations.list");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "channels": [
                    {
                        "id": "C123",
                        "name": "engineering",
                        "is_channel": true,
                        "is_archived": false,
                        "num_members": 4,
                        "purpose": { "value": "Team chat" }
                    }
                ],
                "response_metadata": { "next_cursor": "chat-cursor-1" }
            }));
        });

        let result = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .get_chats(GetChatsParams::default())
            .await
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].name, "engineering");
        assert_eq!(result.items[0].chat_type, ChatType::Channel);
        assert_eq!(
            result
                .pagination
                .as_ref()
                .and_then(|pagination| pagination.next_cursor.as_deref()),
            Some("chat-cursor-1")
        );
    }

    #[tokio::test]
    async fn get_messages_fetches_thread_replies() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/conversations.replies");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "messages": [
                    {
                        "ts": "1710000000.000100",
                        "text": "Root",
                        "user": "U123",
                        "thread_ts": "1710000000.000100"
                    },
                    {
                        "ts": "1710000001.000100",
                        "text": "Reply",
                        "user": "U123",
                        "thread_ts": "1710000000.000100"
                    }
                ],
                "response_metadata": { "next_cursor": "reply-cursor-1" }
            }));
        });
        server.mock(|when, then| {
            when.method(POST).path("/users.info");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "user": {
                    "id": "U123",
                    "name": "andrey",
                    "profile": {
                        "display_name": "Andrey",
                        "real_name": "Andrey Maznyak",
                        "image_72": "https://example.com/avatar.png"
                    }
                }
            }));
        });

        let result = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .get_messages(GetMessagesParams {
                chat_id: "C123".to_string(),
                limit: Some(20),
                cursor: None,
                thread_id: Some("1710000000.000100".to_string()),
                since: None,
                until: None,
            })
            .await
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(
            result.items[1].reply_to_id.as_deref(),
            Some("1710000000.000100")
        );
        assert_eq!(result.items[0].author.name, "Andrey");
        assert_eq!(
            result
                .pagination
                .as_ref()
                .and_then(|pagination| pagination.next_cursor.as_deref()),
            Some("reply-cursor-1")
        );
    }

    #[tokio::test]
    async fn send_message_maps_response() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/chat.postMessage");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "channel": "C123",
                "ts": "1710000100.000200",
                "message": {
                    "ts": "1710000100.000200",
                    "text": "hello world",
                    "bot_profile": {
                        "id": "B123",
                        "name": "Devboy",
                        "icons": { "image_72": "https://example.com/bot.png" }
                    }
                }
            }));
        });

        let result = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .send_message(SendMessageParams {
                chat_id: "C123".to_string(),
                text: "hello world".to_string(),
                thread_id: None,
                reply_to_id: None,
                attachments: vec![],
            })
            .await
            .unwrap();

        assert_eq!(result.chat_id, "C123");
        assert_eq!(result.text, "hello world");
        assert_eq!(result.author.name, "Devboy");
    }

    #[tokio::test]
    async fn send_message_uses_reply_to_id_as_thread_ts_when_thread_id_missing() {
        let server = MockServer::start();
        let post_message = server.mock(|when, then| {
            when.method(POST).path("/chat.postMessage");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "channel": "C123",
                "ts": "1710000100.000200",
                "message": {
                    "ts": "1710000100.000200",
                    "text": "reply message"
                }
            }));
        });

        let result = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .send_message(SendMessageParams {
                chat_id: "C123".to_string(),
                text: "reply message".to_string(),
                thread_id: None,
                reply_to_id: Some("1710000000.000100".to_string()),
                attachments: vec![],
            })
            .await
            .unwrap();

        post_message.assert_calls(1);
        assert_eq!(result.thread_id.as_deref(), Some("1710000000.000100"));
        assert_eq!(result.reply_to_id.as_deref(), Some("1710000000.000100"));
    }

    #[tokio::test]
    async fn send_message_rejects_attachments() {
        let err = SlackClient::new(token("xoxb-test"))
            .send_message(SendMessageParams {
                chat_id: "C123".to_string(),
                text: "reply message".to_string(),
                thread_id: None,
                reply_to_id: None,
                attachments: vec![MessageAttachment {
                    id: Some("att-1".to_string()),
                    name: Some("report.txt".to_string()),
                    attachment_type: None,
                    url: None,
                    mime_type: None,
                    file_size: None,
                }],
            })
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            Error::ProviderUnsupported { provider, operation }
                if provider == "slack" && operation == "send_message attachments"
        ));
    }

    #[tokio::test]
    async fn get_messages_caches_resolved_users() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/conversations.history");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "messages": [
                    {
                        "ts": "1710000000.000100",
                        "text": "First",
                        "user": "U123"
                    },
                    {
                        "ts": "1710000001.000100",
                        "text": "Second",
                        "user": "U123"
                    }
                ]
            }));
        });
        let users_info = server.mock(|when, then| {
            when.method(POST).path("/users.info");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "user": {
                    "id": "U123",
                    "name": "andrey",
                    "profile": {
                        "display_name": "Andrey"
                    }
                }
            }));
        });

        let client = SlackClient::new(token("xoxb-test")).with_base_url(server.base_url());
        let result = client
            .get_messages(GetMessagesParams {
                chat_id: "C123".to_string(),
                limit: Some(20),
                cursor: None,
                thread_id: None,
                since: None,
                until: None,
            })
            .await
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].author.name, "Andrey");
        assert_eq!(result.items[1].author.name, "Andrey");
        users_info.assert_calls(1);
    }

    #[test]
    fn client_configuration_helpers_update_settings() {
        let client = SlackClient::new(token("xoxb-test"))
            .with_base_url("https://slack.example.test/")
            .with_required_scopes(vec!["search:read".to_string(), "chat:write".to_string()]);

        assert_eq!(client.base_url, "https://slack.example.test");
        assert_eq!(
            client.required_scopes(),
            ["search:read".to_string(), "chat:write".to_string()]
        );
    }

    #[tokio::test]
    async fn auth_info_maps_invalid_auth_to_unauthorized() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/auth.test");
            then.status(200).json_body(serde_json::json!({
                "ok": false,
                "error": "invalid_auth"
            }));
        });

        let error = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .auth_info()
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Unauthorized(message) if message == "invalid_auth"));
    }

    #[tokio::test]
    async fn auth_info_maps_missing_scope_to_forbidden() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/auth.test");
            then.status(200).json_body(serde_json::json!({
                "ok": false,
                "error": "missing_scope"
            }));
        });

        let error = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .auth_info()
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Forbidden(message) if message == "missing_scope"));
    }

    #[tokio::test]
    async fn auth_info_maps_rate_limit_to_rate_limited_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/auth.test");
            then.status(429).header("retry-after", "7");
        });

        let error = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .auth_info()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            Error::RateLimited {
                retry_after: Some(7)
            }
        ));
    }

    #[tokio::test]
    async fn search_messages_rejects_empty_query() {
        let error = SlackClient::new(token("xoxb-test"))
            .search_messages(SearchMessagesParams {
                query: "   ".to_string(),
                ..SearchMessagesParams::default()
            })
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::InvalidData(message) if message.contains("must not be empty"))
        );
    }

    #[tokio::test]
    async fn search_messages_global_cursor_resumes_within_chat_before_next_chat_page() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/conversations.list");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "channels": [
                    { "id": "C1", "name": "general", "is_channel": true, "is_archived": false },
                    { "id": "C2", "name": "random", "is_channel": true, "is_archived": false }
                ],
                "response_metadata": { "next_cursor": "chat-page-2" }
            }));
        });
        server.mock(|when, then| {
            when.method(POST).path("/conversations.history");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "messages": [
                    { "ts": "1710000000.000100", "text": "no match", "username": "bot" },
                    { "ts": "1710000001.000100", "text": "Needle on first page", "username": "bot" }
                ],
                "has_more": true,
                "response_metadata": { "next_cursor": "msg-page-2" }
            }));
        });

        let client = SlackClient::new(token("xoxb-test")).with_base_url(server.base_url());
        let first = client
            .search_messages(SearchMessagesParams {
                query: "needle".to_string(),
                limit: Some(1),
                ..SearchMessagesParams::default()
            })
            .await
            .unwrap();

        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].chat_id, "C1");
        assert!(first.pagination.as_ref().unwrap().has_more);

        let cursor = first
            .pagination
            .as_ref()
            .and_then(|pagination| pagination.next_cursor.clone())
            .unwrap();
        let state = parse_search_cursor(Some(&cursor)).unwrap();
        assert_eq!(state.current_chat_id.as_deref(), Some("C1"));
        assert_eq!(state.current_message_cursor.as_deref(), Some("msg-page-2"));
        assert_eq!(state.current_message_offset, 0);
        assert_eq!(state.pending_chat_ids, vec!["C1", "C2"]);
        assert_eq!(state.pending_chat_index, 1);
        assert_eq!(state.next_chats_cursor.as_deref(), Some("chat-page-2"));
    }

    #[tokio::test]
    async fn search_messages_chat_cursor_resumes_within_page_before_next_page() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/conversations.history");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "messages": [
                    { "ts": "1710000000.000100", "text": "needle first", "username": "bot" },
                    { "ts": "1710000001.000100", "text": "needle second", "username": "bot" },
                    { "ts": "1710000002.000100", "text": "no match", "username": "bot" }
                ],
                "has_more": true,
                "response_metadata": { "next_cursor": "msg-page-2" }
            }));
        });

        let client = SlackClient::new(token("xoxb-test")).with_base_url(server.base_url());
        let first = client
            .search_messages(SearchMessagesParams {
                chat_id: Some("C1".to_string()),
                query: "needle".to_string(),
                limit: Some(1),
                ..SearchMessagesParams::default()
            })
            .await
            .unwrap();

        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].text, "needle first");
        assert!(first.pagination.as_ref().unwrap().has_more);

        let cursor = first
            .pagination
            .as_ref()
            .and_then(|pagination| pagination.next_cursor.clone())
            .unwrap();
        let state = parse_search_cursor(Some(&cursor)).unwrap();
        assert_eq!(state.current_chat_id.as_deref(), Some("C1"));
        assert_eq!(state.current_message_cursor, None);
        assert_eq!(state.current_message_offset, 1);

        let second = client
            .search_messages(SearchMessagesParams {
                chat_id: Some("C1".to_string()),
                query: "needle".to_string(),
                limit: Some(1),
                cursor: Some(cursor),
                ..SearchMessagesParams::default()
            })
            .await
            .unwrap();

        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].text, "needle second");
        assert!(second.pagination.as_ref().unwrap().has_more);

        let second_cursor = second
            .pagination
            .as_ref()
            .and_then(|pagination| pagination.next_cursor.clone())
            .unwrap();
        let second_state = parse_search_cursor(Some(&second_cursor)).unwrap();
        assert_eq!(second_state.current_chat_id.as_deref(), Some("C1"));
        assert_eq!(second_state.current_message_cursor, None);
        assert_eq!(second_state.current_message_offset, 2);
    }

    #[test]
    fn helper_functions_cover_filtering_and_mapping_cases() {
        let archived_group = SlackConversation {
            id: "C1".to_string(),
            name: Some("Project Alpha".to_string()),
            user: None,
            is_group: Some(true),
            is_im: None,
            is_mpim: None,
            is_archived: Some(true),
            num_members: Some(3),
            purpose: Some(SlackTextValue {
                value: Some("".to_string()),
            }),
            topic: Some(SlackTextValue {
                value: Some("Topic text".to_string()),
            }),
        };

        assert_eq!(slack_chat_type(&archived_group), ChatType::Group);
        assert_eq!(conversation_name(&archived_group), "Project Alpha");
        assert!(!matches_chat_filter(
            &archived_group,
            &GetChatsParams {
                search: Some("alpha".to_string()),
                chat_type: Some(ChatType::Group),
                include_inactive: Some(false),
                ..GetChatsParams::default()
            }
        ));
        assert!(matches_chat_filter(
            &archived_group,
            &GetChatsParams {
                search: Some("alpha".to_string()),
                chat_type: Some(ChatType::Group),
                include_inactive: Some(true),
                ..GetChatsParams::default()
            }
        ));

        let direct_chat = SlackConversation {
            id: "D1".to_string(),
            name: None,
            user: Some("U123".to_string()),
            is_group: None,
            is_im: Some(true),
            is_mpim: None,
            is_archived: Some(false),
            num_members: None,
            purpose: None,
            topic: None,
        };
        assert_eq!(slack_chat_type(&direct_chat), ChatType::Direct);
        assert_eq!(conversation_name(&direct_chat), "dm-U123");

        let mapped = map_chat(archived_group);
        assert_eq!(mapped.description.as_deref(), Some("Topic text"));
        assert!(!mapped.is_active);
    }

    #[test]
    fn attachment_and_cursor_helpers_cover_fallback_paths() {
        let attachments = map_attachments(&SlackMessage {
            ts: "1710000000.000100".to_string(),
            text: None,
            user: None,
            username: None,
            bot_id: None,
            thread_ts: None,
            parent_user_id: None,
            subtype: None,
            edited: None,
            files: Some(vec![SlackFile {
                id: Some("F1".to_string()),
                name: Some("report.pdf".to_string()),
                mimetype: Some("application/pdf".to_string()),
                filetype: None,
                size: Some(1234),
                url_private: Some("https://private.example/report.pdf".to_string()),
                permalink: None,
            }]),
            attachments: Some(vec![SlackRichAttachment {
                id: Some(42),
                title: None,
                fallback: Some("Fallback title".to_string()),
                service_name: Some("docs".to_string()),
                title_link: None,
                from_url: Some("https://example.com/doc".to_string()),
            }]),
            bot_profile: None,
        });

        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].attachment_type.as_deref(), Some("file"));
        assert_eq!(
            attachments[0].url.as_deref(),
            Some("https://private.example/report.pdf")
        );
        assert_eq!(attachments[1].id.as_deref(), Some("42"));
        assert_eq!(attachments[1].name.as_deref(), Some("Fallback title"));
        assert_eq!(
            attachments[1].url.as_deref(),
            Some("https://example.com/doc")
        );

        assert_eq!(slack_conversation_types(Some(ChatType::Direct)), "im");
        assert_eq!(
            slack_conversation_types(Some(ChatType::Group)),
            "mpim,private_channel"
        );
        assert_eq!(
            slack_conversation_types(Some(ChatType::Channel)),
            "public_channel,private_channel"
        );
        assert_eq!(
            slack_conversation_types(None),
            "public_channel,private_channel,mpim,im"
        );
        assert_eq!(
            slack_next_cursor(Some(&SlackResponseMetadata {
                next_cursor: " cursor-1 ".to_string(),
            })),
            Some("cursor-1".to_string())
        );
        assert_eq!(slack_next_cursor(None), None);
    }

    #[test]
    fn ts_scope_and_markdown_helpers_cover_edge_cases() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-oauth-scopes",
            " channels:read, , chat:write ".parse().unwrap(),
        );

        assert_eq!(
            normalize_ts_param("since", Some(" 1710000000.000100 "))
                .unwrap()
                .as_deref(),
            Some("1710000000.000100")
        );
        assert!(matches!(
            normalize_ts_param("until", Some("not-a-ts")).unwrap_err(),
            Error::InvalidData(message) if message == "until must be a Slack timestamp string"
        ));
        assert_eq!(normalize_ts_param("since", Some("   ")).unwrap(), None);
        assert_eq!(
            parse_scopes(&headers),
            vec!["channels:read".to_string(), "chat:write".to_string()]
        );
        assert!(parse_scopes(&HeaderMap::new()).is_empty());

        assert_eq!(normalize_slack_token("@U123"), "@U123");
        assert_eq!(normalize_slack_token("@U123|andrey"), "@andrey");
        assert_eq!(normalize_slack_token("#C123|general"), "#general");
        assert_eq!(normalize_slack_token("!here"), "here");
        assert_eq!(
            normalize_slack_token("https://example.com|docs"),
            "[docs](https://example.com)"
        );
        assert_eq!(normalize_slack_token("plain-token"), "plain-token");
        assert_eq!(
            normalize_mrkdwn("unterminated <https://example.com"),
            "unterminated <https://example.com"
        );
    }

    #[test]
    fn slack_error_helpers_map_expected_variants() {
        assert!(ensure_ok(true, None).is_ok());
        assert!(matches!(
            ensure_ok(false, Some("missing_scope".to_string())).unwrap_err(),
            Error::Forbidden(message) if message == "missing_scope"
        ));
        assert!(matches!(
            map_slack_error("invalid_auth".to_string()),
            Error::Unauthorized(message) if message == "invalid_auth"
        ));
        assert!(matches!(
            map_slack_error("not_allowed_token_type".to_string()),
            Error::Forbidden(message) if message == "not_allowed_token_type"
        ));
        assert!(matches!(
            map_slack_error("channel_not_found".to_string()),
            Error::NotFound(message) if message == "channel_not_found"
        ));
        assert!(matches!(
            map_slack_error("ratelimited".to_string()),
            Error::RateLimited { retry_after: None }
        ));
        assert!(matches!(
            map_slack_error("other".to_string()),
            Error::Api { status: 200, message } if message == "other"
        ));
    }

    #[test]
    fn normalize_slack_markup_to_markdownish_text() {
        let text = normalize_mrkdwn(
            "See <https://example.com|docs> and talk to <@U123> in <#C123|general>",
        );
        assert!(text.contains("[docs](https://example.com)"));
        assert!(text.contains("@U123"));
        assert!(text.contains("#general"));
    }

    #[test]
    fn rate_limit_bucket_matches_write_method() {
        assert_eq!(
            slack_rate_limit_bucket("chat.postMessage"),
            SlackRateLimitBucket::Write
        );
        assert_eq!(
            slack_rate_limit_bucket("conversations.history"),
            SlackRateLimitBucket::Read
        );
    }

    #[tokio::test]
    async fn rate_limiter_spaces_same_bucket_requests() {
        let limiter = SlackRateLimiter::new(Duration::from_millis(25), Duration::from_millis(10));

        let start = std::time::Instant::now();
        limiter.acquire(SlackRateLimitBucket::Read).await;
        limiter.acquire(SlackRateLimitBucket::Read).await;

        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[tokio::test]
    async fn rate_limiter_keeps_read_and_write_buckets_independent() {
        let limiter = SlackRateLimiter::new(Duration::from_millis(50), Duration::from_millis(10));

        limiter.acquire(SlackRateLimitBucket::Read).await;

        let start = std::time::Instant::now();
        limiter.acquire(SlackRateLimitBucket::Write).await;

        assert!(start.elapsed() < Duration::from_millis(25));
    }

    #[test]
    fn parse_search_cursor_accepts_legacy_chat_cursor() {
        let state = parse_search_cursor(Some("chat-cursor-1")).unwrap();

        assert_eq!(state.next_chats_cursor.as_deref(), Some("chat-cursor-1"));
        assert!(state.current_chat_id.is_none());
        assert!(state.pending_chat_ids.is_empty());
    }

    #[test]
    fn search_cursor_round_trips_with_message_progress() {
        let cursor = SlackSearchCursor {
            version: 1,
            current_chat_id: Some("C123".to_string()),
            current_message_cursor: Some("msg-cursor-2".to_string()),
            current_message_offset: 37,
            pending_chat_ids: vec!["C124".to_string(), "C125".to_string()],
            pending_chat_index: 1,
            next_chats_cursor: Some("chat-cursor-9".to_string()),
        };

        let encoded = serialize_search_cursor(&cursor).unwrap().unwrap();
        let decoded = parse_search_cursor(Some(&encoded)).unwrap();

        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.current_chat_id.as_deref(), Some("C123"));
        assert_eq!(
            decoded.current_message_cursor.as_deref(),
            Some("msg-cursor-2")
        );
        assert_eq!(decoded.current_message_offset, 37);
        assert_eq!(decoded.pending_chat_ids, vec!["C124", "C125"]);
        assert_eq!(decoded.pending_chat_index, 1);
        assert_eq!(decoded.next_chats_cursor.as_deref(), Some("chat-cursor-9"));
    }

    #[test]
    fn take_next_pending_chat_id_advances_without_shifting() {
        let mut state = SlackSearchCursor {
            pending_chat_ids: vec!["C124".to_string(), "C125".to_string()],
            ..SlackSearchCursor::default()
        };

        assert_eq!(
            take_next_pending_chat_id(&mut state).as_deref(),
            Some("C124")
        );
        assert_eq!(state.pending_chat_ids, vec!["C124", "C125"]);
        assert_eq!(state.pending_chat_index, 1);
        assert!(has_pending_chat_ids(&state));

        assert_eq!(
            take_next_pending_chat_id(&mut state).as_deref(),
            Some("C125")
        );
        assert!(state.pending_chat_ids.is_empty());
        assert_eq!(state.pending_chat_index, 0);
        assert!(!has_pending_chat_ids(&state));
    }

    #[test]
    fn serialize_search_cursor_omits_finished_state() {
        let encoded = serialize_search_cursor(&SlackSearchCursor::default()).unwrap();

        assert!(encoded.is_none());
    }

    // =========================================================================
    // UserProvider — issue #177
    // =========================================================================

    #[tokio::test]
    async fn user_provider_get_user_profile_maps_fields() {
        use devboy_core::UserProvider;
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/users.info");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "user": {
                    "id": "U1",
                    "name": "alice",
                    "profile": {
                        "real_name": "Alice Liddell",
                        "display_name": "alice.l",
                        "image_72": "https://cdn/alice.png",
                        "email": "alice@example.com"
                    }
                }
            }));
        });

        let user = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .get_user_profile("U1")
            .await
            .unwrap();

        assert_eq!(user.id, "U1");
        assert_eq!(user.username, "alice");
        assert_eq!(user.name.as_deref(), Some("alice.l"));
        assert_eq!(user.email.as_deref(), Some("alice@example.com"));
        assert_eq!(user.avatar_url.as_deref(), Some("https://cdn/alice.png"));
    }

    #[tokio::test]
    async fn user_provider_lookup_by_email_users_not_found_returns_none() {
        use devboy_core::UserProvider;
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/users.lookupByEmail");
            then.status(200).json_body(serde_json::json!({
                "ok": false,
                "error": "users_not_found"
            }));
        });

        let result = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .lookup_user_by_email("missing@example.com")
            .await
            .unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn user_provider_lookup_by_email_hit_returns_user() {
        use devboy_core::UserProvider;
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/users.lookupByEmail");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "user": {
                    "id": "U2",
                    "name": "bob",
                    "profile": {
                        "real_name": "Bob",
                        "display_name": "",
                        "image_72": null,
                        "email": "bob@example.com"
                    }
                }
            }));
        });

        let user = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .lookup_user_by_email("bob@example.com")
            .await
            .unwrap()
            .expect("user match");

        assert_eq!(user.id, "U2");
        assert_eq!(user.email.as_deref(), Some("bob@example.com"));
        // display_name empty → falls back to real_name
        assert_eq!(user.name.as_deref(), Some("Bob"));
    }

    #[tokio::test]
    async fn user_provider_lookup_by_email_propagates_other_errors() {
        use devboy_core::UserProvider;
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/users.lookupByEmail");
            then.status(200).json_body(serde_json::json!({
                "ok": false,
                "error": "missing_scope"
            }));
        });

        let err = SlackClient::new(token("xoxb-test"))
            .with_base_url(server.base_url())
            .lookup_user_by_email("alice@example.com")
            .await
            .expect_err("missing_scope must not be silently swallowed");
        let msg = err.to_string();
        assert!(msg.contains("missing_scope"), "unexpected: {msg}");
    }
}
