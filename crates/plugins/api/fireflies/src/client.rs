//! Fireflies.ai GraphQL client implementing `MeetingNotesProvider`.

use async_trait::async_trait;
use devboy_core::{
    Error, MeetingFilter, MeetingNote, MeetingNotesProvider, MeetingSpeaker, MeetingTranscript,
    ProviderResult, Result, TranscriptSentence,
};
use serde_json::{Value, json};
use tracing::debug;

use crate::types::*;

const FIREFLIES_API_URL: &str = "https://api.fireflies.ai/graphql";

/// GraphQL query for listing transcripts.
const GET_TRANSCRIPTS_QUERY: &str = r#"
query GetTranscripts(
  $keyword: String
  $fromDate: DateTime
  $toDate: DateTime
  $limit: Int
  $skip: Int
  $host_email: String
  $participants: [String!]
) {
  transcripts(
    keyword: $keyword
    fromDate: $fromDate
    toDate: $toDate
    limit: $limit
    skip: $skip
    host_email: $host_email
    participants: $participants
  ) {
    id
    title
    date
    duration
    host_email
    organizer_email
    meeting_attendees { displayName email name }
    speakers { id name }
    transcript_url
    audio_url
    video_url
    meeting_link
    summary {
      keywords
      action_items
      topics_discussed
      meeting_type
      overview
      short_summary
    }
  }
}
"#;

/// GraphQL query for a single transcript with sentences.
const GET_TRANSCRIPT_QUERY: &str = r#"
query GetTranscript($transcriptId: String!) {
  transcript(id: $transcriptId) {
    id
    title
    date
    duration
    speakers { id name }
    sentences {
      speaker_id
      text
      start_time
      end_time
    }
  }
}
"#;

/// Fireflies.ai API client.
pub struct FirefliesClient {
    api_key: String,
    http: reqwest::Client,
}

impl FirefliesClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Execute a GraphQL query against the Fireflies API.
    async fn graphql<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
    ) -> Result<T> {
        let body = json!({
            "query": query,
            "variables": variables,
        });

        debug!(url = FIREFLIES_API_URL, "fireflies graphql request");

        let response = self
            .http
            .post(FIREFLIES_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized("Invalid Fireflies API key".to_string()));
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        let gql_response: GraphQLResponse<T> = response
            .json()
            .await
            .map_err(|e| Error::InvalidData(e.to_string()))?;

        if let Some(errors) = gql_response.errors {
            let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
            return Err(Error::Api {
                status: 200,
                message: messages.join("; "),
            });
        }

        gql_response
            .data
            .ok_or_else(|| Error::InvalidData("Fireflies API returned no data".to_string()))
    }
}

#[async_trait]
impl MeetingNotesProvider for FirefliesClient {
    fn provider_name(&self) -> &'static str {
        "fireflies"
    }

    async fn get_meetings(&self, filter: MeetingFilter) -> Result<ProviderResult<MeetingNote>> {
        let variables = build_filter_variables(&filter);
        let data: TranscriptsData = self.graphql(GET_TRANSCRIPTS_QUERY, variables).await?;
        Ok(data
            .transcripts
            .into_iter()
            .map(convert_transcript)
            .collect::<Vec<_>>()
            .into())
    }

    async fn get_transcript(&self, meeting_id: &str) -> Result<MeetingTranscript> {
        let variables = json!({ "transcriptId": meeting_id });
        let data: TranscriptData = self.graphql(GET_TRANSCRIPT_QUERY, variables).await?;

        let transcript = data.transcript.ok_or_else(|| {
            Error::NotFound(format!("Meeting transcript not found: {meeting_id}"))
        })?;

        let speakers = transcript.speakers.as_deref().unwrap_or(&[]);
        // Build a HashMap for O(1) speaker name lookup instead of O(n) linear scan
        let speaker_map: std::collections::HashMap<String, String> = speakers
            .iter()
            .filter_map(|sp| {
                let id = match &sp.id {
                    Some(Value::String(v)) => v.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => return None,
                };
                sp.name.as_ref().map(|name| (id, name.clone()))
            })
            .collect();
        let sentences = transcript
            .sentences
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                let speaker_id = match &s.speaker_id {
                    Some(Value::String(id)) => id.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => String::new(),
                };
                // Skip speaker lookup when speaker_id is empty
                let speaker_name = if speaker_id.is_empty() {
                    None
                } else {
                    speaker_map.get(&speaker_id).cloned()
                };

                TranscriptSentence {
                    speaker_id,
                    speaker_name,
                    text: s.text.unwrap_or_default(),
                    start_time: s.start_time.unwrap_or(0.0),
                    end_time: s.end_time.unwrap_or(0.0),
                }
            })
            .collect();

        Ok(MeetingTranscript {
            meeting_id: transcript.id,
            title: transcript.title,
            sentences,
        })
    }

    async fn search_meetings(
        &self,
        query: &str,
        filter: MeetingFilter,
    ) -> Result<ProviderResult<MeetingNote>> {
        let mut variables = build_filter_variables(&filter);
        if let Some(obj) = variables.as_object_mut() {
            obj.insert("keyword".into(), Value::String(query.to_string()));
        }
        let data: TranscriptsData = self.graphql(GET_TRANSCRIPTS_QUERY, variables).await?;
        Ok(data
            .transcripts
            .into_iter()
            .map(convert_transcript)
            .collect::<Vec<_>>()
            .into())
    }
}

/// Build GraphQL variables from a MeetingFilter.
fn build_filter_variables(filter: &MeetingFilter) -> Value {
    let mut vars = serde_json::Map::new();

    if let Some(ref keyword) = filter.keyword {
        vars.insert("keyword".into(), Value::String(keyword.clone()));
    }
    if let Some(ref from) = filter.from_date {
        vars.insert("fromDate".into(), Value::String(from.clone()));
    }
    if let Some(ref to) = filter.to_date {
        vars.insert("toDate".into(), Value::String(to.clone()));
    }
    if let Some(ref host) = filter.host_email {
        vars.insert("host_email".into(), Value::String(host.clone()));
    }
    if let Some(ref participants) = filter.participants {
        vars.insert(
            "participants".into(),
            Value::Array(
                participants
                    .iter()
                    .map(|p| Value::String(p.clone()))
                    .collect(),
            ),
        );
    }
    vars.insert(
        "limit".into(),
        Value::Number(filter.limit.unwrap_or(50).into()),
    );
    if let Some(skip) = filter.skip {
        vars.insert("skip".into(), Value::Number(skip.into()));
    }

    Value::Object(vars)
}

/// Convert a Fireflies API transcript to a unified MeetingNote.
fn convert_transcript(t: FirefliesTranscript) -> MeetingNote {
    let meeting_date = t.date.and_then(|d| parse_date_value(&d));

    let participants: Vec<String> = t
        .meeting_attendees
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.email.or(a.name).or(a.display_name))
        .collect();

    let speakers: Vec<MeetingSpeaker> = t
        .speakers
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            let id = match &s.id {
                Some(Value::String(v)) => v.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => String::new(),
            };
            MeetingSpeaker {
                id,
                name: s.name.unwrap_or_default(),
            }
        })
        .collect();

    // Fireflies duration is in MINUTES, convert to seconds
    let duration_seconds = t.duration.map(|d| (d * 60.0) as u64);

    let summary_ref = t.summary.as_ref();
    let action_items = summary_ref
        .and_then(|s| s.action_items.as_ref())
        .map(parse_array_or_string)
        .unwrap_or_default();
    let keywords = summary_ref
        .and_then(|s| s.keywords.as_ref())
        .map(parse_array_or_string)
        .unwrap_or_default();
    let topics_discussed = summary_ref
        .and_then(|s| s.topics_discussed.as_ref())
        .map(parse_array_or_string)
        .unwrap_or_default();
    let meeting_type = summary_ref.and_then(|s| s.meeting_type.clone());
    let summary_text = summary_ref.and_then(|s| s.overview.clone().or(s.short_summary.clone()));

    MeetingNote {
        id: t.id,
        title: t.title.unwrap_or_default(),
        meeting_date,
        duration_seconds,
        host_email: t.host_email,
        organizer_email: t.organizer_email,
        participants,
        speakers,
        action_items,
        keywords,
        topics_discussed,
        meeting_type,
        summary: summary_text,
        transcript_url: t.transcript_url,
        audio_url: t.audio_url,
        video_url: t.video_url,
        meeting_link: t.meeting_link,
    }
}

/// Parse a date value that can be either an ISO string or Unix timestamp in milliseconds.
fn parse_date_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => {
            // Unix timestamp in milliseconds → ISO 8601 string
            let ms = n.as_f64()?;
            let secs = (ms / 1000.0) as i64;
            let dt = chrono::DateTime::from_timestamp(secs, 0)?;
            Some(dt.to_rfc3339())
        }
        _ => None,
    }
}

/// Parse a Fireflies field that can be either a JSON array or a markdown string.
fn parse_array_or_string(value: &Value) -> Vec<String> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) => s
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                // Strip bullet prefixes, then normalize whitespace before stripping bold
                let stripped = trimmed
                    .trim_start_matches('-')
                    .trim_start_matches('*')
                    .trim()
                    .trim_start_matches("## ")
                    .trim();
                // Strip bold markers (**text**)
                let stripped = stripped.strip_prefix("**").unwrap_or(stripped);
                let stripped = stripped.strip_suffix("**").unwrap_or(stripped);
                stripped.trim().to_string()
            })
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_array_or_string_with_array() {
        let val = json!(["item1", "item2", "item3"]);
        let result = parse_array_or_string(&val);
        assert_eq!(result, vec!["item1", "item2", "item3"]);
    }

    #[test]
    fn test_parse_array_or_string_with_markdown() {
        let val = json!("**Actions**\n- Review PR\n- Update docs\n");
        let result = parse_array_or_string(&val);
        assert_eq!(result, vec!["Actions", "Review PR", "Update docs"]);
    }

    #[test]
    fn test_parse_array_or_string_with_null() {
        let val = json!(null);
        let result = parse_array_or_string(&val);
        assert!(result.is_empty());
    }

    #[test]
    fn test_convert_transcript_duration_conversion() {
        let t = FirefliesTranscript {
            id: "test-id".into(),
            title: Some("Test Meeting".into()),
            date: Some(Value::String("2025-01-15T10:00:00Z".into())),
            duration: Some(45.5), // 45.5 minutes
            host_email: None,
            organizer_email: None,
            meeting_attendees: None,
            speakers: None,
            transcript_url: None,
            audio_url: None,
            video_url: None,
            meeting_link: None,
            summary: None,
            sentences: None,
        };

        let note = convert_transcript(t);
        assert_eq!(note.duration_seconds, Some(2730)); // 45.5 * 60 = 2730
    }

    #[test]
    fn test_convert_transcript_with_attendees_and_speakers() {
        let t = FirefliesTranscript {
            id: "m-123".into(),
            title: Some("Sprint Planning".into()),
            date: None,
            duration: None,
            host_email: Some("host@example.com".into()),
            organizer_email: Some("org@example.com".into()),
            meeting_attendees: Some(vec![
                FirefliesAttendee {
                    display_name: None,
                    email: Some("alice@example.com".into()),
                    name: None,
                },
                FirefliesAttendee {
                    display_name: Some("Bob".into()),
                    email: None,
                    name: None,
                },
            ]),
            speakers: Some(vec![
                FirefliesSpeaker {
                    id: Some("1".into()),
                    name: Some("Alice".into()),
                },
                FirefliesSpeaker {
                    id: Some("2".into()),
                    name: Some("Bob".into()),
                },
            ]),
            transcript_url: None,
            audio_url: None,
            video_url: None,
            meeting_link: None,
            summary: Some(FirefliesSummary {
                keywords: Some(json!(["rust", "migration"])),
                action_items: Some(json!("- Review PR\n- Update docs")),
                topics_discussed: None,
                meeting_type: Some("planning".into()),
                overview: Some("Team discussed migration plan.".into()),
                short_summary: None,
            }),
            sentences: None,
        };

        let note = convert_transcript(t);
        assert_eq!(note.id, "m-123");
        assert_eq!(note.title, "Sprint Planning");
        assert_eq!(note.host_email, Some("host@example.com".into()));
        assert_eq!(
            note.participants,
            vec!["alice@example.com".to_string(), "Bob".to_string()]
        );
        assert_eq!(note.speakers.len(), 2);
        assert_eq!(note.speakers[0].name, "Alice");
        assert_eq!(note.keywords, vec!["rust", "migration"]);
        assert_eq!(note.action_items, vec!["Review PR", "Update docs"]);
        assert_eq!(note.meeting_type, Some("planning".into()));
        assert_eq!(note.summary, Some("Team discussed migration plan.".into()));
    }

    #[test]
    fn test_convert_transcript_no_duration() {
        let t = FirefliesTranscript {
            id: "no-dur".into(),
            title: None,
            date: None,
            duration: None,
            host_email: None,
            organizer_email: None,
            meeting_attendees: None,
            speakers: None,
            transcript_url: None,
            audio_url: None,
            video_url: None,
            meeting_link: None,
            summary: None,
            sentences: None,
        };

        let note = convert_transcript(t);
        assert_eq!(note.duration_seconds, None);
        assert!(note.title.is_empty());
    }

    #[test]
    fn test_parse_array_or_string_with_empty_array() {
        let val = json!([]);
        let result = parse_array_or_string(&val);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_array_or_string_filters_empty_strings() {
        let val = json!(["item1", "", "item2"]);
        let result = parse_array_or_string(&val);
        assert_eq!(result, vec!["item1", "item2"]);
    }

    #[test]
    fn test_build_filter_variables_empty() {
        let filter = MeetingFilter::default();
        let vars = build_filter_variables(&filter);
        let obj = vars.as_object().unwrap();
        assert_eq!(obj.get("limit").unwrap(), &json!(50));
        assert!(obj.get("keyword").is_none());
        assert!(obj.get("fromDate").is_none());
    }

    #[test]
    fn test_build_filter_variables_full() {
        let filter = MeetingFilter {
            keyword: Some("sprint".into()),
            from_date: Some("2025-01-01".into()),
            to_date: Some("2025-12-31".into()),
            participants: Some(vec!["alice@ex.com".into()]),
            host_email: Some("host@ex.com".into()),
            limit: Some(10),
            skip: Some(5),
        };
        let vars = build_filter_variables(&filter);
        let obj = vars.as_object().unwrap();
        assert_eq!(obj["keyword"], json!("sprint"));
        assert_eq!(obj["fromDate"], json!("2025-01-01"));
        assert_eq!(obj["toDate"], json!("2025-12-31"));
        assert_eq!(obj["host_email"], json!("host@ex.com"));
        assert_eq!(obj["participants"], json!(["alice@ex.com"]));
        assert_eq!(obj["limit"], json!(10));
        assert_eq!(obj["skip"], json!(5));
    }

    #[test]
    fn test_fireflies_client_new() {
        let client = FirefliesClient::new("test-key");
        assert_eq!(client.provider_name(), "fireflies");
    }
}
