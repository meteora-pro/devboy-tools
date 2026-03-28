//! Fireflies.ai GraphQL API response types.

use serde::Deserialize;

/// GraphQL response wrapper.
#[derive(Debug, Deserialize)]
pub(crate) struct GraphQLResponse<T> {
    pub data: Option<T>,
    pub errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GraphQLError {
    pub message: String,
}

/// Response for `transcripts` query.
#[derive(Debug, Deserialize)]
pub(crate) struct TranscriptsData {
    pub transcripts: Vec<FirefliesTranscript>,
}

/// Response for `transcript` query (single).
#[derive(Debug, Deserialize)]
pub(crate) struct TranscriptData {
    pub transcript: Option<FirefliesTranscript>,
}

/// A Fireflies transcript record.
#[derive(Debug, Deserialize)]
pub(crate) struct FirefliesTranscript {
    pub id: String,
    pub title: Option<String>,
    pub date: Option<String>,
    /// Duration in minutes (float) from Fireflies API.
    pub duration: Option<f64>,
    pub host_email: Option<String>,
    pub organizer_email: Option<String>,
    pub meeting_attendees: Option<Vec<FirefliesAttendee>>,
    pub speakers: Option<Vec<FirefliesSpeaker>>,
    pub transcript_url: Option<String>,
    pub audio_url: Option<String>,
    pub video_url: Option<String>,
    pub meeting_link: Option<String>,
    pub summary: Option<FirefliesSummary>,
    pub sentences: Option<Vec<FirefliesSentence>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FirefliesAttendee {
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FirefliesSpeaker {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FirefliesSummary {
    pub keywords: Option<serde_json::Value>,
    pub action_items: Option<serde_json::Value>,
    pub topics_discussed: Option<serde_json::Value>,
    pub meeting_type: Option<String>,
    pub overview: Option<String>,
    pub short_summary: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FirefliesSentence {
    pub speaker_id: Option<serde_json::Value>,
    pub text: Option<String>,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
}
