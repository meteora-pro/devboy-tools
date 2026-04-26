use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use devboy_core::{
    CreatePageParams, Error, KbPage, KbPageContent, KbSpace, KnowledgeBaseProvider,
    ListPagesParams, Pagination, ProviderResult, Result, SearchKbParams, UpdatePageParams,
};
use reqwest::RequestBuilder;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::DEFAULT_CONFLUENCE_API_PATH;

#[derive(Clone, PartialEq, Eq)]
pub enum ConfluenceAuth {
    None,
    BearerToken(String),
    Basic { username: String, password: String },
}

impl fmt::Debug for ConfluenceAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::BearerToken(_) => f.debug_tuple("BearerToken").field(&"<redacted>").finish(),
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone)]
pub struct ConfluenceClient {
    base_url: String,
    api_path: String,
    auth: ConfluenceAuth,
    proxy_headers: Option<HashMap<String, String>>,
    http: reqwest::Client,
}

impl fmt::Debug for ConfluenceClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfluenceClient")
            .field("base_url", &self.base_url)
            .field("api_path", &self.api_path)
            .field("auth", &self.auth)
            .field("http", &self.http)
            .finish()
    }
}

impl ConfluenceClient {
    pub fn new(base_url: impl Into<String>, auth: ConfluenceAuth) -> Self {
        Self {
            base_url: normalize_base_url(base_url.into()),
            api_path: DEFAULT_CONFLUENCE_API_PATH.to_string(),
            auth,
            proxy_headers: None,
            http: reqwest::Client::new(),
        }
    }

    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn auth(&self) -> &ConfluenceAuth {
        &self.auth
    }

    pub fn with_api_version(mut self, api_version: Option<&str>) -> Self {
        self.api_path = api_path_for_version(api_version);
        self
    }

    /// Configure proxy mode with headers added to every request.
    /// When proxy is active, provider auth headers are suppressed.
    pub fn with_proxy(mut self, headers: HashMap<String, String>) -> Self {
        self.proxy_headers = Some(headers);
        self
    }

    pub fn rest_api_url(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("{}{}/{}", self.base_url, self.api_path, path)
    }

    pub async fn get_json<T>(&self, path: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let request = self
            .http
            .get(self.rest_api_url(path))
            .header(reqwest::header::ACCEPT, "application/json");
        self.send_json(request).await
    }

    pub async fn post_json<T, B>(&self, path: &str, body: &B) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let request = self
            .http
            .post(self.rest_api_url(path))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        self.send_json(request).await
    }

    pub async fn put_json<T, B>(&self, path: &str, body: &B) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let request = self
            .http
            .put(self.rest_api_url(path))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        self.send_json(request).await
    }

    async fn send_json<T>(&self, request: RequestBuilder) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let response = self
            .apply_auth(request)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), message));
        }

        response
            .json()
            .await
            .map_err(|e| Error::InvalidData(e.to_string()))
    }

    fn apply_auth(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(headers) = &self.proxy_headers {
            return request.headers(proxy_headers_to_headermap(headers));
        }

        match &self.auth {
            ConfluenceAuth::None => request,
            ConfluenceAuth::BearerToken(token) => request.bearer_auth(token),
            ConfluenceAuth::Basic { username, password } => {
                request.basic_auth(username, Some(password))
            }
        }
    }
}

fn proxy_headers_to_headermap(headers: &HashMap<String, String>) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (key, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(key.as_str()),
            HeaderValue::try_from(value.as_str()),
        ) {
            map.insert(name, value);
        }
    }
    map
}

fn normalize_base_url(base_url: String) -> String {
    base_url.trim_end_matches('/').to_string()
}

fn api_path_for_version(api_version: Option<&str>) -> String {
    match api_version.map(str::trim).filter(|v| !v.is_empty()) {
        Some("v2") => "/api/v2".to_string(),
        _ => DEFAULT_CONFLUENCE_API_PATH.to_string(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct ConfluenceListResponse<T> {
    #[serde(default)]
    results: Vec<T>,
    #[serde(default)]
    start: Option<u32>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    size: Option<u32>,
    #[serde(default, rename = "totalSize")]
    total_size: Option<u32>,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ConfluenceLinks {
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    webui: Option<String>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfluenceSpace {
    id: String,
    key: String,
    name: String,
    #[serde(rename = "type", default)]
    space_type: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    description: Option<ConfluenceSpaceDescription>,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Deserialize)]
struct ConfluenceSpaceDescription {
    #[serde(default)]
    plain: Option<ConfluenceValueContainer>,
    #[serde(default)]
    view: Option<ConfluenceValueContainer>,
}

#[derive(Debug, Deserialize)]
struct ConfluenceValueContainer {
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluencePage {
    id: String,
    title: String,
    #[serde(default)]
    space: Option<ConfluenceSpaceRef>,
    #[serde(default)]
    version: Option<ConfluenceVersion>,
    #[serde(default)]
    history: Option<ConfluenceHistory>,
    #[serde(default)]
    body: Option<ConfluenceBody>,
    #[serde(default)]
    metadata: Option<ConfluenceMetadata>,
    #[serde(default)]
    ancestors: Vec<ConfluenceAncestor>,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceSpaceRef {
    #[serde(default)]
    key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceVersion {
    #[serde(default)]
    number: Option<u32>,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    by: Option<ConfluenceUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceHistory {
    #[serde(default, rename = "lastUpdated")]
    last_updated: Option<ConfluenceVersion>,
    #[serde(default, rename = "createdBy")]
    created_by: Option<ConfluenceUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceUser {
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceBody {
    #[serde(default)]
    storage: Option<ConfluenceBodyValue>,
    #[serde(default)]
    view: Option<ConfluenceBodyValue>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceBodyValue {
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceMetadata {
    #[serde(default)]
    labels: Option<ConfluenceLabelList>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceLabelList {
    #[serde(default)]
    results: Vec<ConfluenceLabel>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceLabel {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceAncestor {
    id: String,
    title: String,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Serialize)]
struct ConfluenceContentBody<'a> {
    value: &'a str,
    representation: &'static str,
}

#[derive(Debug, Serialize)]
struct ConfluenceContentPayload<'a> {
    #[serde(rename = "type")]
    content_type: &'static str,
    title: &'a str,
    space: ConfluenceCreateSpaceRef<'a>,
    body: ConfluenceCreateBodyPayload<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ancestors: Vec<ConfluenceCreateAncestorRef<'a>>,
}

#[derive(Debug, Serialize)]
struct ConfluenceCreateSpaceRef<'a> {
    key: &'a str,
}

#[derive(Debug, Serialize)]
struct ConfluenceCreateBodyPayload<'a> {
    storage: ConfluenceContentBody<'a>,
}

#[derive(Debug, Serialize)]
struct ConfluenceCreateAncestorRef<'a> {
    id: &'a str,
}

#[derive(Debug, Serialize)]
struct ConfluenceUpdatePayload<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    content_type: &'static str,
    title: &'a str,
    version: ConfluenceUpdateVersion,
    body: ConfluenceCreateBodyPayload<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ancestors: Option<Vec<ConfluenceCreateAncestorRef<'a>>>,
}

#[derive(Debug, Serialize)]
struct ConfluenceUpdateVersion {
    number: u32,
}

fn join_link(base_url: &str, base_hint: Option<&str>, path: Option<&str>) -> Option<String> {
    let path = path?;
    if path.starts_with("http://") || path.starts_with("https://") {
        return Some(path.to_string());
    }
    let base = base_hint.unwrap_or(base_url).trim_end_matches('/');
    if path.starts_with('/') {
        Some(format!("{base}{path}"))
    } else {
        Some(format!("{base}/{path}"))
    }
}

fn display_name(user: Option<&ConfluenceUser>) -> Option<String> {
    user.and_then(|u| u.display_name.clone().or_else(|| u.username.clone()))
}

fn page_excerpt(page: &ConfluencePage) -> Option<String> {
    page.body
        .as_ref()
        .and_then(|body| body.view.as_ref().or(body.storage.as_ref()))
        .and_then(|body| body.value.clone())
        .map(|value| truncate_string(strip_html_tags(&value), 280))
        .filter(|value| !value.is_empty())
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_html_tags_preserve_layout(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn truncate_string(input: String, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input;
    }
    input.chars().take(max_chars).collect::<String>()
}

fn normalize_confluence_write_content(content: &str, content_type: Option<&str>) -> Result<String> {
    match content_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("markdown")
    {
        "markdown" => Ok(markdown_to_confluence_storage(content)),
        "html" => Ok(html_to_confluence_storage(content)),
        "storage" => Ok(content.to_string()),
        other => Err(Error::InvalidData(format!(
            "unsupported confluence content_type '{other}', expected markdown, html, or storage"
        ))),
    }
}

fn html_to_confluence_storage(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains('<') && trimmed.contains('>') {
        trimmed.to_string()
    } else {
        format!("<p>{}</p>", escape_html(trimmed))
    }
}

fn markdown_to_confluence_storage(markdown: &str) -> String {
    let markdown = markdown.replace("\r\n", "\n");
    let mut out = String::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut in_ul = false;
    let mut in_ol = false;
    let mut lines = markdown.lines().peekable();

    let flush_paragraph = |out: &mut String, paragraph: &mut Vec<String>| {
        if paragraph.is_empty() {
            return;
        }
        let text = paragraph.join(" ");
        out.push_str("<p>");
        out.push_str(&markdown_inline_to_html(&text));
        out.push_str("</p>");
        paragraph.clear();
    };

    let close_lists = |out: &mut String, in_ul: &mut bool, in_ol: &mut bool| {
        if *in_ul {
            out.push_str("</ul>");
            *in_ul = false;
        }
        if *in_ol {
            out.push_str("</ol>");
            *in_ol = false;
        }
    };

    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            flush_paragraph(&mut out, &mut paragraph);
            close_lists(&mut out, &mut in_ul, &mut in_ol);

            let mut code_lines = Vec::new();
            for code_line in lines.by_ref() {
                if code_line.trim_start().starts_with("```") {
                    break;
                }
                code_lines.push(code_line);
            }

            out.push_str(r#"<ac:structured-macro ac:name="code"><ac:plain-text-body><![CDATA["#);
            out.push_str(&code_lines.join("\n"));
            out.push_str("]]></ac:plain-text-body></ac:structured-macro>");
            continue;
        }

        if trimmed.is_empty() {
            flush_paragraph(&mut out, &mut paragraph);
            close_lists(&mut out, &mut in_ul, &mut in_ol);
            continue;
        }

        if let Some((level, title)) = parse_markdown_heading(trimmed) {
            flush_paragraph(&mut out, &mut paragraph);
            close_lists(&mut out, &mut in_ul, &mut in_ol);
            out.push_str(&format!(
                "<h{level}>{}</h{level}>",
                markdown_inline_to_html(title)
            ));
            continue;
        }

        if let Some(item) = parse_unordered_list_item(trimmed) {
            flush_paragraph(&mut out, &mut paragraph);
            if in_ol {
                out.push_str("</ol>");
                in_ol = false;
            }
            if !in_ul {
                out.push_str("<ul>");
                in_ul = true;
            }
            out.push_str("<li>");
            out.push_str(&markdown_inline_to_html(item));
            out.push_str("</li>");
            continue;
        }

        if let Some(item) = parse_ordered_list_item(trimmed) {
            flush_paragraph(&mut out, &mut paragraph);
            if in_ul {
                out.push_str("</ul>");
                in_ul = false;
            }
            if !in_ol {
                out.push_str("<ol>");
                in_ol = true;
            }
            out.push_str("<li>");
            out.push_str(&markdown_inline_to_html(item));
            out.push_str("</li>");
            continue;
        }

        close_lists(&mut out, &mut in_ul, &mut in_ol);
        paragraph.push(trimmed.to_string());
    }

    flush_paragraph(&mut out, &mut paragraph);
    close_lists(&mut out, &mut in_ul, &mut in_ol);
    out
}

fn parse_markdown_heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|&ch| ch == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = line.get(hashes..)?.trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((hashes, rest))
}

fn parse_unordered_list_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .map(str::trim)
}

fn parse_ordered_list_item(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = line.get(digits..)?;
    rest.strip_prefix(". ").map(str::trim)
}

fn markdown_inline_to_html(input: &str) -> String {
    let escaped = escape_html(input);
    let linked = replace_markdown_links(&escaped);
    let code = replace_inline_delimited(&linked, "`", "<code>", "</code>");
    let bold = replace_inline_delimited(&code, "**", "<strong>", "</strong>");
    replace_inline_delimited(&bold, "*", "<em>", "</em>")
}

fn replace_markdown_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;

    while let Some(start_rel) = input[cursor..].find('[') {
        let start = cursor + start_rel;
        out.push_str(&input[cursor..start]);

        let Some(text_end_rel) = input[start + 1..].find(']') else {
            out.push_str(&input[start..]);
            return out;
        };
        let text_end = start + 1 + text_end_rel;
        let after_bracket = text_end + 1;
        if !input[after_bracket..].starts_with('(') {
            out.push('[');
            cursor = start + 1;
            continue;
        }

        let Some(url_end_rel) = input[after_bracket + 1..].find(')') else {
            out.push_str(&input[start..]);
            return out;
        };
        let url_end = after_bracket + 1 + url_end_rel;
        let text = &input[start + 1..text_end];
        let url = &input[after_bracket + 1..url_end];
        out.push_str(&format!(r#"<a href="{url}">{text}</a>"#));
        cursor = url_end + 1;
    }

    out.push_str(&input[cursor..]);
    out
}

fn replace_inline_delimited(input: &str, delimiter: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    let mut is_open = false;

    while let Some(found_rel) = input[cursor..].find(delimiter) {
        let found = cursor + found_rel;
        out.push_str(&input[cursor..found]);
        if is_open {
            out.push_str(close);
        } else {
            out.push_str(open);
        }
        is_open = !is_open;
        cursor = found + delimiter.len();
    }

    out.push_str(&input[cursor..]);
    if is_open && let Some(position) = out.rfind(open) {
        out.replace_range(position..position + open.len(), delimiter);
        out.push_str(delimiter);
    }
    out
}

fn confluence_storage_to_markdown(storage: &str) -> String {
    let with_code_blocks = replace_confluence_code_macros(storage);
    let with_links = replace_anchor_tags(&with_code_blocks);
    let with_formatting = replace_paired_tag(
        &replace_paired_tag(
            &replace_paired_tag(
                &replace_paired_tag(&with_links, "strong", "**", "**"),
                "b",
                "**",
                "**",
            ),
            "em",
            "*",
            "*",
        ),
        "i",
        "*",
        "*",
    );
    let with_inline_code = replace_paired_tag(&with_formatting, "code", "`", "`");
    let markdownish = with_inline_code
        .replace("<br />", "\n")
        .replace("<br/>", "\n")
        .replace("<br>", "\n")
        .replace("<p>", "")
        .replace("</p>", "\n\n")
        .replace("<div>", "")
        .replace("</div>", "\n\n")
        .replace("<ul>", "")
        .replace("</ul>", "\n")
        .replace("<ol>", "")
        .replace("</ol>", "\n")
        .replace("<li>", "- ")
        .replace("</li>", "\n")
        .replace("<h1>", "# ")
        .replace("</h1>", "\n\n")
        .replace("<h2>", "## ")
        .replace("</h2>", "\n\n")
        .replace("<h3>", "### ")
        .replace("</h3>", "\n\n")
        .replace("<h4>", "#### ")
        .replace("</h4>", "\n\n")
        .replace("<h5>", "##### ")
        .replace("</h5>", "\n\n")
        .replace("<h6>", "###### ")
        .replace("</h6>", "\n\n");

    let text = strip_html_tags_preserve_layout(&markdownish);
    collapse_markdown_whitespace(&decode_html_entities(&text))
}

fn replace_confluence_code_macros(input: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0usize;
    let macro_start = r#"<ac:structured-macro ac:name="code">"#;
    let body_start = "<ac:plain-text-body><![CDATA[";
    let body_end = "]]></ac:plain-text-body>";
    let macro_end = "</ac:structured-macro>";

    while let Some(start_rel) = input[cursor..].find(macro_start) {
        let start = cursor + start_rel;
        out.push_str(&input[cursor..start]);
        let Some(code_start_rel) = input[start..].find(body_start) else {
            out.push_str(&input[start..]);
            return out;
        };
        let code_start = start + code_start_rel + body_start.len();
        let Some(code_end_rel) = input[code_start..].find(body_end) else {
            out.push_str(&input[start..]);
            return out;
        };
        let code_end = code_start + code_end_rel;
        let Some(macro_end_rel) = input[code_end..].find(macro_end) else {
            out.push_str(&input[start..]);
            return out;
        };
        let end = code_end + macro_end_rel + macro_end.len();
        let code = &input[code_start..code_end];
        out.push_str("```");
        out.push('\n');
        out.push_str(code);
        out.push('\n');
        out.push_str("```");
        cursor = end;
    }

    out.push_str(&input[cursor..]);
    out
}

fn replace_anchor_tags(input: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0usize;

    while let Some(start_rel) = input[cursor..].find("<a ") {
        let start = cursor + start_rel;
        out.push_str(&input[cursor..start]);
        let Some(tag_end_rel) = input[start..].find('>') else {
            out.push_str(&input[start..]);
            return out;
        };
        let tag_end = start + tag_end_rel;
        let tag = &input[start..=tag_end];
        let Some(close_rel) = input[tag_end + 1..].find("</a>") else {
            out.push_str(&input[start..]);
            return out;
        };
        let close = tag_end + 1 + close_rel;
        let label = &input[tag_end + 1..close];
        let href = extract_attribute(tag, "href").unwrap_or_default();
        out.push('[');
        out.push_str(label);
        out.push_str("](");
        out.push_str(&href);
        out.push(')');
        cursor = close + "</a>".len();
    }

    out.push_str(&input[cursor..]);
    out
}

fn replace_paired_tag(input: &str, tag: &str, open: &str, close: &str) -> String {
    input
        .replace(&format!("<{tag}>"), open)
        .replace(&format!("</{tag}>"), close)
}

fn extract_attribute(tag: &str, attr: &str) -> Option<String> {
    let needle = format!(r#"{attr}=""#);
    let start = tag.find(&needle)? + needle.len();
    let rest = tag.get(start..)?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn collapse_markdown_whitespace(input: &str) -> String {
    let mut normalized = Vec::new();
    let mut previous_blank = false;

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !normalized.is_empty() && !previous_blank {
                normalized.push(String::new());
                previous_blank = true;
            }
            continue;
        }

        normalized.push(trimmed.to_string());
        previous_blank = false;
    }

    normalized.join("\n").trim().to_string()
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn map_space(base_url: &str, raw: ConfluenceSpace) -> KbSpace {
    let description = raw
        .description
        .and_then(|d| {
            d.plain
                .and_then(|v| v.value)
                .or_else(|| d.view.and_then(|v| v.value))
        })
        .map(|value| truncate_string(strip_html_tags(&value), 500))
        .filter(|value| !value.is_empty());

    KbSpace {
        id: raw.id,
        key: raw.key,
        name: raw.name,
        space_type: raw.space_type,
        status: raw.status,
        description,
        url: join_link(
            base_url,
            raw._links.base.as_deref(),
            raw._links.webui.as_deref(),
        ),
    }
}

fn map_page_summary(base_url: &str, raw: &ConfluencePage) -> KbPage {
    let version = raw
        .history
        .as_ref()
        .and_then(|h| h.last_updated.as_ref())
        .or(raw.version.as_ref());

    KbPage {
        id: raw.id.clone(),
        title: raw.title.clone(),
        space_key: raw.space.as_ref().and_then(|space| space.key.clone()),
        url: join_link(
            base_url,
            raw._links.base.as_deref(),
            raw._links.webui.as_deref(),
        ),
        version: version.and_then(|v| v.number),
        last_modified: version.and_then(|v| v.when.clone()),
        author: display_name(version.and_then(|v| v.by.as_ref()))
            .or_else(|| display_name(raw.history.as_ref().and_then(|h| h.created_by.as_ref()))),
        excerpt: page_excerpt(raw),
    }
}

fn map_pagination<T>(
    response: &ConfluenceListResponse<T>,
    requested_limit: Option<u32>,
) -> Pagination {
    let offset = response.start.unwrap_or(0);
    let limit = requested_limit
        .or(response.limit)
        .or(response.size)
        .unwrap_or(response.results.len() as u32);
    let total = response.total_size;
    let has_more = response._links.next.is_some()
        || total
            .map(|total| {
                offset.saturating_add(response.size.unwrap_or(response.results.len() as u32))
                    < total
            })
            .unwrap_or(false);

    Pagination {
        offset,
        limit,
        total,
        has_more,
        next_cursor: response._links.next.clone(),
    }
}

fn encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                encoded.push('%');
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
    encoded
}

fn escape_cql_string(value: &str) -> String {
    value.replace('\\', r"\\").replace('"', r#"\""#)
}

fn build_search_cql(params: &SearchKbParams) -> String {
    if params.raw_query {
        return params.query.clone();
    }

    let mut parts = vec!["type = page".to_string()];
    if let Some(space_key) = params.space_key.as_ref() {
        parts.push(format!("space = \"{}\"", escape_cql_string(space_key)));
    }
    parts.push(format!("text ~ \"{}\"", escape_cql_string(&params.query)));
    parts.join(" AND ")
}

fn path_from_cursor(cursor: &str, api_path: &str) -> String {
    let api_prefix = format!("{}/", api_path.trim_end_matches('/'));
    if let Some(path) = cursor.strip_prefix(&api_prefix) {
        path.to_string()
    } else if let Some(path) = cursor.strip_prefix(api_path) {
        path.trim_start_matches('/').to_string()
    } else if let Some(path) = cursor.strip_prefix("http://") {
        let path = path.split_once(&api_prefix).map(|(_, rhs)| rhs);
        path.unwrap_or(cursor).to_string()
    } else if let Some(path) = cursor.strip_prefix("https://") {
        let path = path.split_once(&api_prefix).map(|(_, rhs)| rhs);
        path.unwrap_or(cursor).to_string()
    } else {
        cursor.trim_start_matches('/').to_string()
    }
}

#[async_trait]
impl KnowledgeBaseProvider for ConfluenceClient {
    fn provider_name(&self) -> &'static str {
        "confluence"
    }

    async fn get_spaces(&self) -> Result<ProviderResult<KbSpace>> {
        let response: ConfluenceListResponse<ConfluenceSpace> = self
            .get_json("space?limit=100&type=global,personal")
            .await?;
        let pagination = map_pagination(&response, Some(100));
        let items = response
            .results
            .into_iter()
            .map(|space| map_space(&self.base_url, space))
            .collect::<Vec<_>>();

        Ok(ProviderResult::new(items).with_pagination(pagination))
    }

    async fn list_pages(&self, params: ListPagesParams) -> Result<ProviderResult<KbPage>> {
        let limit = params.limit.unwrap_or(25);
        let path = if let Some(cursor) = params.cursor.as_ref() {
            path_from_cursor(cursor, &self.api_path)
        } else {
            let offset = params.offset.unwrap_or(0);
            let query = [
                format!("spaceKey={}", encode_query_value(&params.space_key)),
                "type=page".to_string(),
                format!("limit={limit}"),
                format!("start={offset}"),
                "expand=space,version,history.lastUpdated,body.view,ancestors".to_string(),
            ];
            format!("content?{}", query.join("&"))
        };

        let response: ConfluenceListResponse<ConfluencePage> = self.get_json(&path).await?;
        let pagination = map_pagination(&response, Some(limit));
        let mut items = response
            .results
            .iter()
            .filter(|page| {
                params
                    .parent_id
                    .as_ref()
                    .map(|parent_id| {
                        page.ancestors
                            .last()
                            .map(|ancestor| ancestor.id == *parent_id)
                            .unwrap_or(false)
                    })
                    .unwrap_or(true)
            })
            .map(|page| map_page_summary(&self.base_url, page))
            .collect::<Vec<_>>();

        if let Some(search) = params.search.as_ref() {
            let search = search.to_ascii_lowercase();
            items.retain(|page| {
                page.title.to_ascii_lowercase().contains(&search)
                    || page
                        .excerpt
                        .as_ref()
                        .map(|excerpt| excerpt.to_ascii_lowercase().contains(&search))
                        .unwrap_or(false)
            });
        }

        Ok(ProviderResult::new(items).with_pagination(pagination))
    }

    async fn get_page(&self, page_id: &str) -> Result<KbPageContent> {
        let path = format!(
            "content/{page_id}?expand=space,version,history.lastUpdated,body.storage,metadata.labels,ancestors"
        );
        let page: ConfluencePage = self.get_json(&path).await?;
        let summary = map_page_summary(&self.base_url, &page);
        let storage_content = page
            .body
            .as_ref()
            .and_then(|body| body.storage.as_ref())
            .and_then(|storage| storage.value.clone())
            .unwrap_or_default();
        let content = confluence_storage_to_markdown(&storage_content);
        let content_type = "markdown".to_string();
        let ancestors = page
            .ancestors
            .iter()
            .map(|ancestor| KbPage {
                id: ancestor.id.clone(),
                title: ancestor.title.clone(),
                space_key: None,
                url: join_link(
                    &self.base_url,
                    ancestor._links.base.as_deref(),
                    ancestor._links.webui.as_deref(),
                ),
                version: None,
                last_modified: None,
                author: None,
                excerpt: None,
            })
            .collect();
        let labels = page
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.labels.as_ref())
            .map(|labels| {
                labels
                    .results
                    .iter()
                    .filter_map(|label| label.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(KbPageContent {
            page: summary,
            content,
            content_type,
            ancestors,
            labels,
        })
    }

    async fn create_page(&self, _params: CreatePageParams) -> Result<KbPage> {
        let storage_content =
            normalize_confluence_write_content(&_params.content, _params.content_type.as_deref())?;

        let payload = ConfluenceContentPayload {
            content_type: "page",
            title: &_params.title,
            space: ConfluenceCreateSpaceRef {
                key: &_params.space_key,
            },
            body: ConfluenceCreateBodyPayload {
                storage: ConfluenceContentBody {
                    value: &storage_content,
                    representation: "storage",
                },
            },
            ancestors: _params
                .parent_id
                .as_deref()
                .map(|id| vec![ConfluenceCreateAncestorRef { id }])
                .unwrap_or_default(),
        };

        let page: ConfluencePage = self.post_json("content", &payload).await?;
        Ok(map_page_summary(&self.base_url, &page))
    }

    async fn update_page(&self, _params: UpdatePageParams) -> Result<KbPage> {
        let current_path = format!(
            "content/{}?expand=space,version,body.storage,ancestors",
            _params.page_id
        );
        let current: ConfluencePage = self.get_json(&current_path).await?;

        let current_title = current.title.clone();
        let current_content = current
            .body
            .as_ref()
            .and_then(|body| body.storage.as_ref())
            .and_then(|storage| storage.value.clone())
            .unwrap_or_default();
        let current_version = current
            .version
            .as_ref()
            .and_then(|version| version.number)
            .ok_or_else(|| {
                Error::InvalidData(format!(
                    "confluence page {} is missing a version number",
                    _params.page_id
                ))
            })?;

        if let Some(expected_version) = _params.version
            && expected_version != current_version
        {
            return Err(Error::Api {
                status: 409,
                message: format!(
                    "version conflict for page {}: expected current version {}, found {}",
                    _params.page_id, expected_version, current_version
                ),
            });
        }

        let title = _params.title.as_deref().unwrap_or(&current_title);
        let content = match _params.content.as_deref() {
            Some(updated) => {
                normalize_confluence_write_content(updated, _params.content_type.as_deref())?
            }
            None => current_content,
        };
        let ancestors = _params
            .parent_id
            .as_deref()
            .map(|id| vec![ConfluenceCreateAncestorRef { id }]);

        let payload = ConfluenceUpdatePayload {
            id: &_params.page_id,
            content_type: "page",
            title,
            version: ConfluenceUpdateVersion {
                number: current_version.saturating_add(1),
            },
            body: ConfluenceCreateBodyPayload {
                storage: ConfluenceContentBody {
                    value: &content,
                    representation: "storage",
                },
            },
            ancestors,
        };

        let path = format!("content/{}", _params.page_id);
        let page: ConfluencePage = self.put_json(&path, &payload).await?;
        Ok(map_page_summary(&self.base_url, &page))
    }

    async fn search(&self, params: SearchKbParams) -> Result<ProviderResult<KbPage>> {
        let limit = params.limit.unwrap_or(25);

        let path = if let Some(cursor) = params.cursor.as_ref() {
            path_from_cursor(cursor, &self.api_path)
        } else {
            let cql = build_search_cql(&params);
            format!(
                "content/search?cql={}&limit={limit}&expand=space,version,history.lastUpdated,body.view",
                encode_query_value(&cql)
            )
        };

        let response: ConfluenceListResponse<ConfluencePage> = self.get_json(&path).await?;
        let pagination = map_pagination(&response, Some(limit));
        let items = response
            .results
            .iter()
            .map(|page| map_page_summary(&self.base_url, page))
            .collect::<Vec<_>>();

        Ok(ProviderResult::new(items).with_pagination(pagination))
    }
}

#[cfg(test)]
mod tests {
    use httpmock::Method::{GET, POST, PUT};
    use httpmock::MockServer;
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Deserialize)]
    struct EchoResponse {
        ok: bool,
    }

    #[derive(Debug, Serialize)]
    struct CreatePayload {
        title: String,
    }

    #[tokio::test]
    async fn rest_api_url_normalizes_base_url() {
        let client = ConfluenceClient::new(
            "https://wiki.example.com/",
            ConfluenceAuth::BearerToken("token".into()),
        );

        assert_eq!(
            client.rest_api_url("content"),
            "https://wiki.example.com/rest/api/content"
        );
    }

    #[tokio::test]
    async fn rest_api_url_honors_v2_api_version() {
        let client = ConfluenceClient::new(
            "https://wiki.example.com/",
            ConfluenceAuth::BearerToken("token".into()),
        )
        .with_api_version(Some("v2"));

        assert_eq!(
            client.rest_api_url("pages"),
            "https://wiki.example.com/api/v2/pages"
        );
    }

    #[tokio::test]
    async fn get_json_uses_bearer_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .header("authorization", "Bearer secret-token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let response: EchoResponse = client.get_json("content").await.unwrap();

        mock.assert();
        assert!(response.ok);
    }

    #[tokio::test]
    async fn post_json_uses_basic_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/content")
                .header(
                    "authorization",
                    "Basic dXNlckBleGFtcGxlLmNvbTpwYXNzd29yZA==",
                )
                .json_body_obj(&serde_json::json!({ "title": "ADR-001" }));
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::Basic {
                username: "user@example.com".into(),
                password: "password".into(),
            },
        );
        let response: EchoResponse = client
            .post_json(
                "content",
                &CreatePayload {
                    title: "ADR-001".into(),
                },
            )
            .await
            .unwrap();

        mock.assert();
        assert!(response.ok);
    }

    #[tokio::test]
    async fn proxy_headers_suppress_provider_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .header("x-proxy-auth", "secret")
                .header_missing("authorization");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let mut headers = HashMap::new();
        headers.insert("x-proxy-auth".into(), "secret".into());

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        )
        .with_proxy(headers);
        let response: EchoResponse = client.get_json("content").await.unwrap();

        mock.assert();
        assert!(response.ok);
    }

    #[tokio::test]
    async fn get_spaces_maps_confluence_spaces() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "123",
                                "key": "ENG",
                                "name": "Engineering",
                                "type": "global",
                                "status": "current",
                                "description": { "plain": { "value": "Team docs" } },
                                "_links": { "base": "https://wiki.example.com", "webui": "/spaces/ENG/overview" }
                            }
                        ],
                        "start": 0,
                        "limit": 100,
                        "size": 1,
                        "totalSize": 1,
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let result = client.get_spaces().await.unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].key, "ENG");
        assert_eq!(result.items[0].name, "Engineering");
        assert_eq!(result.items[0].description.as_deref(), Some("Team docs"));
        assert_eq!(
            result.items[0].url.as_deref(),
            Some("https://wiki.example.com/spaces/ENG/overview")
        );
        assert_eq!(result.pagination.unwrap().total, Some(1));
    }

    #[tokio::test]
    async fn list_pages_maps_page_summaries_and_pagination() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .query_param("spaceKey", "ENG")
                .query_param("type", "page")
                .query_param("limit", "25")
                .query_param("start", "0")
                .query_param("expand", "space,version,history.lastUpdated,body.view,ancestors");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "42",
                                "title": "ADR-001",
                                "space": { "key": "ENG" },
                                "version": {
                                    "number": 7,
                                    "when": "2026-04-26T10:00:00.000Z",
                                    "by": { "displayName": "Alice" }
                                },
                                "body": {
                                    "view": { "value": "<p>Architecture decision record</p>", "representation": "view" }
                                },
                                "ancestors": [],
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42", "next": "/rest/api/content?start=25" }
                            }
                        ],
                        "start": 0,
                        "limit": 25,
                        "size": 1,
                        "totalSize": 30,
                        "_links": { "next": "/rest/api/content?start=25" }
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let result = client
            .list_pages(ListPagesParams {
                space_key: "ENG".into(),
                limit: Some(25),
                offset: Some(0),
                cursor: None,
                search: None,
                parent_id: None,
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "42");
        assert_eq!(result.items[0].space_key.as_deref(), Some("ENG"));
        assert_eq!(result.items[0].version, Some(7));
        assert_eq!(result.items[0].author.as_deref(), Some("Alice"));
        assert_eq!(
            result.items[0].excerpt.as_deref(),
            Some("Architecture decision record")
        );
        let pagination = result.pagination.unwrap();
        assert!(pagination.has_more);
        assert_eq!(
            pagination.next_cursor.as_deref(),
            Some("/rest/api/content?start=25")
        );
        assert_eq!(pagination.total, Some(30));
    }

    #[tokio::test]
    async fn list_pages_uses_cursor_path_for_followup_requests() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .query_param("limit", "25")
                .query_param("start", "25");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "77",
                                "title": "Next Page",
                                "space": { "key": "ENG" },
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=77" }
                            }
                        ],
                        "start": 25,
                        "limit": 25,
                        "size": 1,
                        "totalSize": 26,
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let result = client
            .list_pages(ListPagesParams {
                space_key: "ENG".into(),
                limit: Some(25),
                offset: Some(0),
                cursor: Some("/rest/api/content?limit=25&start=25".into()),
                search: None,
                parent_id: None,
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "77");
    }

    #[tokio::test]
    async fn get_page_maps_storage_content_labels_and_ancestors() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/42")
                .query_param(
                    "expand",
                    "space,version,history.lastUpdated,body.storage,metadata.labels,ancestors",
                );
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "space": { "key": "ENG" },
                        "version": {
                            "number": 7,
                            "when": "2026-04-26T10:00:00.000Z",
                            "by": { "displayName": "Alice" }
                        },
                        "body": {
                            "storage": {
                                "value": "<p>Hello <strong>world</strong></p>",
                                "representation": "storage"
                            }
                        },
                        "metadata": {
                            "labels": {
                                "results": [
                                    { "name": "adr" },
                                    { "name": "architecture" }
                                ]
                            }
                        },
                        "ancestors": [
                            {
                                "id": "10",
                                "title": "Architecture Decisions",
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=10" }
                            }
                        ],
                        "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42" }
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let page = client.get_page("42").await.unwrap();

        mock.assert();
        assert_eq!(page.page.id, "42");
        assert_eq!(page.page.title, "ADR-001");
        assert_eq!(page.page.version, Some(7));
        assert_eq!(page.content_type, "markdown");
        assert_eq!(page.content, "Hello **world**");
        assert_eq!(page.labels, vec!["adr", "architecture"]);
        assert_eq!(page.ancestors.len(), 1);
        assert_eq!(page.ancestors[0].id, "10");
        assert_eq!(page.ancestors[0].title, "Architecture Decisions");
    }

    #[tokio::test]
    async fn create_page_accepts_markdown_and_posts_storage_payload() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/content")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!({
                    "type": "page",
                    "title": "ADR-002",
                    "space": { "key": "ENG" },
                    "body": {
                        "storage": {
                            "value": "<h1>Decision</h1><p>Hello <strong>world</strong></p>",
                            "representation": "storage"
                        }
                    },
                    "ancestors": [{ "id": "10" }]
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "43",
                        "title": "ADR-002",
                        "space": { "key": "ENG" },
                        "version": {
                            "number": 1,
                            "when": "2026-04-26T10:00:00.000Z",
                            "by": { "displayName": "Alice" }
                        },
                        "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=43" }
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let page = client
            .create_page(CreatePageParams {
                space_key: "ENG".into(),
                title: "ADR-002".into(),
                content: "# Decision\n\nHello **world**".into(),
                content_type: Some("markdown".into()),
                parent_id: Some("10".into()),
                labels: vec![],
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(page.id, "43");
        assert_eq!(page.title, "ADR-002");
        assert_eq!(page.space_key.as_deref(), Some("ENG"));
        assert_eq!(page.version, Some(1));
    }

    #[tokio::test]
    async fn update_page_accepts_markdown_and_puts_incremented_version() {
        let server = MockServer::start();
        let get_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/42")
                .query_param("expand", "space,version,body.storage,ancestors");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "space": { "key": "ENG" },
                        "version": { "number": 7 },
                        "body": {
                            "storage": {
                                "value": "<p>Old</p>",
                                "representation": "storage"
                            }
                        },
                        "ancestors": [
                            { "id": "10", "title": "Architecture", "_links": {} }
                        ],
                        "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42" }
                    }"#,
                );
        });
        let put_mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/rest/api/content/42")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!({
                    "id": "42",
                    "type": "page",
                    "title": "ADR-001 Revised",
                    "version": { "number": 8 },
                    "body": {
                        "storage": {
                            "value": "<p>New <strong>decision</strong></p>",
                            "representation": "storage"
                        }
                    },
                    "ancestors": [{ "id": "11" }]
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001 Revised",
                        "space": { "key": "ENG" },
                        "version": {
                            "number": 8,
                            "when": "2026-04-26T11:00:00.000Z",
                            "by": { "displayName": "Bob" }
                        },
                        "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42" }
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let page = client
            .update_page(UpdatePageParams {
                page_id: "42".into(),
                title: Some("ADR-001 Revised".into()),
                content: Some("New **decision**".into()),
                content_type: Some("markdown".into()),
                version: Some(7),
                labels: None,
                parent_id: Some("11".into()),
            })
            .await
            .unwrap();

        get_mock.assert();
        put_mock.assert();
        assert_eq!(page.id, "42");
        assert_eq!(page.title, "ADR-001 Revised");
        assert_eq!(page.version, Some(8));
        assert_eq!(page.author.as_deref(), Some("Bob"));
    }

    #[tokio::test]
    async fn update_page_returns_conflict_when_expected_version_is_stale() {
        let server = MockServer::start();
        let get_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/42")
                .query_param("expand", "space,version,body.storage,ancestors");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "space": { "key": "ENG" },
                        "version": { "number": 7 },
                        "body": {
                            "storage": {
                                "value": "<p>Old</p>",
                                "representation": "storage"
                            }
                        },
                        "ancestors": [],
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let error = client
            .update_page(UpdatePageParams {
                page_id: "42".into(),
                title: Some("ADR-001 Revised".into()),
                content: Some("<p>New</p>".into()),
                content_type: Some("storage".into()),
                version: Some(6),
                labels: None,
                parent_id: None,
            })
            .await
            .unwrap_err();

        get_mock.assert();
        match error {
            Error::Api { status, message } => {
                assert_eq!(status, 409);
                assert!(message.contains("expected current version 6"));
                assert!(message.contains("found 7"));
            }
            other => panic!("expected conflict error, got {other:?}"),
        }
    }

    #[test]
    fn storage_and_markdown_converters_cover_basic_formatting() {
        let markdown = confluence_storage_to_markdown(
            r#"<h2>ADR</h2><p>Hello <strong>world</strong> and <a href="https://example.com">link</a></p><ul><li>One</li><li>Two</li></ul>"#,
        );
        assert_eq!(
            markdown,
            "## ADR\n\nHello **world** and [link](https://example.com)\n\n- One\n- Two"
        );

        let storage = markdown_to_confluence_storage(
            "## ADR\n\nHello **world** and [link](https://example.com)\n\n- One\n- Two",
        );
        assert_eq!(
            storage,
            "<h2>ADR</h2><p>Hello <strong>world</strong> and <a href=\"https://example.com\">link</a></p><ul><li>One</li><li>Two</li></ul>"
        );
    }

    #[tokio::test]
    async fn search_builds_free_text_cql_and_maps_results() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/search")
                .query_param("cql", "type = page AND space = \"ENG\" AND text ~ \"architecture\"")
                .query_param("limit", "10")
                .query_param("expand", "space,version,history.lastUpdated,body.view");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "99",
                                "title": "Architecture Overview",
                                "space": { "key": "ENG" },
                                "version": {
                                    "number": 3,
                                    "when": "2026-04-26T10:00:00.000Z",
                                    "by": { "displayName": "Alice" }
                                },
                                "body": {
                                    "view": { "value": "<p>System architecture</p>", "representation": "view" }
                                },
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=99" }
                            }
                        ],
                        "start": 0,
                        "limit": 10,
                        "size": 1,
                        "totalSize": 1,
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let result = client
            .search(SearchKbParams {
                query: "architecture".into(),
                space_key: Some("ENG".into()),
                cursor: None,
                limit: Some(10),
                raw_query: false,
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "99");
        assert_eq!(result.items[0].title, "Architecture Overview");
        assert_eq!(result.items[0].space_key.as_deref(), Some("ENG"));
    }

    #[tokio::test]
    async fn search_uses_raw_cql_and_cursor_path() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/search")
                .query_param("cql", "label = \"adr\"")
                .query_param("limit", "5")
                .query_param("expand", "space,version,history.lastUpdated,body.view");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [],
                        "start": 0,
                        "limit": 5,
                        "size": 0,
                        "totalSize": 6,
                        "_links": { "next": "/rest/api/content/search?cql=label%20%3D%20%22adr%22&limit=5&start=5" }
                    }"#,
                );
        });
        let next_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/search")
                .query_param("cql", "label = \"adr\"")
                .query_param("limit", "5")
                .query_param("start", "5");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "123",
                                "title": "ADR-123",
                                "space": { "key": "ENG" },
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=123" }
                            }
                        ],
                        "start": 5,
                        "limit": 5,
                        "size": 1,
                        "totalSize": 6,
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let first = client
            .search(SearchKbParams {
                query: r#"label = "adr""#.into(),
                space_key: None,
                cursor: None,
                limit: Some(5),
                raw_query: true,
            })
            .await
            .unwrap();
        let next_cursor = first
            .pagination
            .as_ref()
            .and_then(|p| p.next_cursor.clone());

        mock.assert();
        assert!(first.items.is_empty());
        assert_eq!(
            next_cursor.as_deref(),
            Some("/rest/api/content/search?cql=label%20%3D%20%22adr%22&limit=5&start=5")
        );

        let second = client
            .search(SearchKbParams {
                query: String::new(),
                space_key: None,
                cursor: next_cursor,
                limit: Some(5),
                raw_query: true,
            })
            .await
            .unwrap();

        next_mock.assert();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].id, "123");
        assert_eq!(second.items[0].title, "ADR-123");
    }

    #[tokio::test]
    async fn search_percent_encodes_reserved_query_characters() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/search")
                .query_param("cql", "type = page AND text ~ \"R&D?x=y+z\"")
                .query_param("limit", "5")
                .query_param("expand", "space,version,history.lastUpdated,body.view");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"results":[],"start":0,"limit":5,"size":0,"_links":{}}"#);
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let result = client
            .search(SearchKbParams {
                query: "R&D?x=y+z".into(),
                space_key: None,
                cursor: None,
                limit: Some(5),
                raw_query: false,
            })
            .await
            .unwrap();

        mock.assert();
        assert!(result.items.is_empty());
    }
}
