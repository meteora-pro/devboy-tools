use std::borrow::Cow;

use async_trait::async_trait;
use devboy_core::types::ChatType;
use devboy_core::{
    Error, GetChatsParams, GetMessagesParams, MessageAttachment, MessageAuthor, MessengerChat,
    MessengerMessage, MessengerProvider, Pagination, ProviderResult, Result, SearchMessagesParams,
    SendMessageParams,
};
use reqwest::header::HeaderMap;
use serde::Deserialize;
use serde::de::DeserializeOwned;
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

#[derive(Debug, Clone)]
pub struct SlackClient {
    token: String,
    base_url: String,
    http: reqwest::Client,
    required_scopes: Vec<String>,
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
    is_channel: Option<bool>,
    is_group: Option<bool>,
    is_im: Option<bool>,
    is_mpim: Option<bool>,
    is_private: Option<bool>,
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
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            base_url: DEFAULT_SLACK_API_URL.to_string(),
            http: reqwest::Client::new(),
            required_scopes: devboy_core::default_slack_required_scopes(),
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

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let status = response.status();
        let headers = response.headers().clone();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), text));
        }

        let payload: SlackAuthTestResponse = response
            .json()
            .await
            .map_err(|e| Error::InvalidData(e.to_string()))?;

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
        let url = format!("{}/{}", self.base_url, method);
        debug!(url, "slack api request");

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .form(params)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        map_http_error(response).await
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

        Ok(ProviderResult::new(items).with_pagination(Pagination {
            offset: 0,
            limit,
            total: None,
            has_more,
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
        if let Some(since) = normalize_ts_param(params.since.as_deref()) {
            form.push(("oldest", since.into_owned()));
        }
        if let Some(until) = normalize_ts_param(params.until.as_deref()) {
            form.push(("latest", until.into_owned()));
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

        Ok(ProviderResult::new(items).with_pagination(Pagination {
            offset: 0,
            limit,
            total: None,
            has_more,
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

        Ok(MessageAuthor {
            id: user.id,
            name,
            username,
            avatar_url: profile.and_then(|profile| profile.image_72.clone()),
        })
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

        let has_more = if let Some(chat_id) = params.chat_id.as_ref() {
            let messages = self
                .get_messages_page(&GetMessagesParams {
                    chat_id: chat_id.clone(),
                    limit: Some(params.limit.unwrap_or(100)),
                    cursor: params.cursor.clone(),
                    thread_id: None,
                    since: params.since.clone(),
                    until: params.until.clone(),
                })
                .await?;
            for message in messages.items {
                if message.text.to_lowercase().contains(&query) {
                    found.push(message);
                    if found.len() >= limit {
                        break;
                    }
                }
            }
            messages.pagination.map(|p| p.has_more).unwrap_or(false)
        } else {
            let chats = self
                .get_conversations(&GetChatsParams {
                    search: None,
                    chat_type: None,
                    limit: Some(100),
                    cursor: params.cursor.clone(),
                    include_inactive: Some(false),
                })
                .await?;
            let mut has_more = chats
                .pagination
                .as_ref()
                .map(|p| p.has_more)
                .unwrap_or(false);

            for chat in chats.items {
                let messages = self
                    .get_messages_page(&GetMessagesParams {
                        chat_id: chat.id.clone(),
                        limit: Some(100),
                        cursor: None,
                        thread_id: None,
                        since: params.since.clone(),
                        until: params.until.clone(),
                    })
                    .await?;

                for message in messages.items {
                    if message.text.to_lowercase().contains(&query) {
                        found.push(message);
                        if found.len() >= limit {
                            break;
                        }
                    }
                }

                has_more = has_more || messages.pagination.map(|p| p.has_more).unwrap_or(false);
                if found.len() >= limit {
                    break;
                }
            }
            has_more
        };

        Ok(ProviderResult::new(found).with_pagination(Pagination {
            offset: 0,
            limit: limit as u32,
            total: None,
            has_more,
        }))
    }

    async fn send_message(&self, params: SendMessageParams) -> Result<MessengerMessage> {
        let mut form = vec![
            ("channel", params.chat_id.clone()),
            ("text", params.text.clone()),
            ("unfurl_links", "false".to_string()),
            ("unfurl_media", "false".to_string()),
        ];
        if let Some(thread_id) = params.thread_id.as_ref() {
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
            thread_ts: params.thread_id.clone(),
            parent_user_id: None,
            subtype: None,
            edited: None,
            files: None,
            attachments: None,
            bot_profile: None,
        });

        if message.thread_ts.is_none() {
            message.thread_ts = params.thread_id;
        }

        self.map_message(
            payload.channel.as_deref().unwrap_or(&params.chat_id),
            message,
        )
        .await
    }
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
    } else if chat.is_channel.unwrap_or(false) || chat.is_private.unwrap_or(false) {
        ChatType::Channel
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

fn normalize_ts_param(value: Option<&str>) -> Option<Cow<'_, str>> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    if value.parse::<f64>().is_ok() {
        Some(Cow::Borrowed(value))
    } else {
        None
    }
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
        return format!("@{}", user);
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

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    #[tokio::test]
    async fn auth_info_reads_identity_and_scopes() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/auth.test");
            then.status(200)
                .header(
                    "x-oauth-scopes",
                    "channels:read, channels:history, chat:write, users:read",
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

        let info = SlackClient::new("xoxb-test")
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

        let error = SlackClient::new("xoxb-test")
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
                "response_metadata": { "next_cursor": "" }
            }));
        });

        let result = SlackClient::new("xoxb-test")
            .with_base_url(server.base_url())
            .get_chats(GetChatsParams::default())
            .await
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].name, "engineering");
        assert_eq!(result.items[0].chat_type, ChatType::Channel);
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
                ]
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

        let result = SlackClient::new("xoxb-test")
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

        let result = SlackClient::new("xoxb-test")
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

    #[test]
    fn normalize_slack_markup_to_markdownish_text() {
        let text = normalize_mrkdwn(
            "See <https://example.com|docs> and talk to <@U123> in <#C123|general>",
        );
        assert!(text.contains("[docs](https://example.com)"));
        assert!(text.contains("@U123"));
        assert!(text.contains("#general"));
    }
}
