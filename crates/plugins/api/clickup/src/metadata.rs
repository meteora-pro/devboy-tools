//! ClickUp provider metadata types for dynamic schema enrichment.

use serde::{Deserialize, Serialize};

/// Metadata for a ClickUp list, used for dynamic schema enrichment.
///
/// In cloud mode, this is loaded from the database (project.issueTrackerMetadata).
/// In CLI mode, this can be fetched from the ClickUp API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpMetadata {
    /// Available statuses for the list.
    #[serde(default)]
    pub statuses: Vec<ClickUpStatus>,
    /// Custom fields defined for the list.
    #[serde(default, alias = "customFields")]
    pub custom_fields: Vec<ClickUpCustomField>,
}

/// ClickUp list status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpStatus {
    /// Name.
    pub name: String,
    /// Status type: "open", "closed", "custom", etc.
    #[serde(default)]
    pub r#type: Option<String>,
}

/// ClickUp custom field definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpCustomField {
    /// Field ID in ClickUp.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Field type.
    #[serde(alias = "type")]
    pub field_type: ClickUpFieldType,
    /// Whether this field is required.
    #[serde(default)]
    pub required: bool,
    /// Options for dropdown/labels fields.
    #[serde(default)]
    pub options: Vec<ClickUpFieldOption>,
}

/// ClickUp custom field types with their transformation semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ClickUpFieldType {
    /// Single select dropdown → value is name, transforms to orderindex.
    #[serde(alias = "drop_down")]
    Dropdown,
    /// Multi-select labels → value is name array, transforms to id array.
    Labels,
    /// Numeric value → pass-through.
    Number,
    /// Currency value → pass-through as number.
    Currency,
    /// Boolean → pass-through.
    Checkbox,
    /// Date → ISO 8601 or Unix timestamp ms.
    Date,
    /// Free text → pass-through.
    Text,
    /// Email → pass-through as string.
    Email,
    /// URL → pass-through as string.
    Url,
    /// Phone → pass-through as string.
    Phone,
    /// Catch-all for unknown/unsupported field types (automatic_progress, etc.)
    #[serde(other)]
    Unknown,
}

/// Option for dropdown/labels custom fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpFieldOption {
    /// Option ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Position in dropdown list (ClickUp-specific, used for dropdown value).
    #[serde(default)]
    pub orderindex: Option<u32>,
}

impl ClickUpCustomField {
    /// Convert a human-readable value to ClickUp API format.
    ///
    /// - Dropdown: name → orderindex (position in options array)
    /// - Labels: name array → id array
    /// - Other types: pass-through
    pub fn transform_value(&self, value: &serde_json::Value) -> serde_json::Value {
        match self.field_type {
            ClickUpFieldType::Dropdown => {
                if let Some(name) = value.as_str() {
                    // Find option by name and return orderindex
                    for (idx, opt) in self.options.iter().enumerate() {
                        if opt.name.eq_ignore_ascii_case(name) {
                            return serde_json::json!(opt.orderindex.unwrap_or(idx as u32));
                        }
                    }
                }
                value.clone()
            }
            ClickUpFieldType::Labels => {
                if let Some(names) = value.as_array() {
                    let ids: Vec<serde_json::Value> = names
                        .iter()
                        .filter_map(|n| {
                            let name = n.as_str()?;
                            self.options
                                .iter()
                                .find(|o| o.name.eq_ignore_ascii_case(name))
                                .map(|o| serde_json::json!(o.id))
                        })
                        .collect();
                    return serde_json::json!(ids);
                }
                value.clone()
            }
            ClickUpFieldType::Checkbox => {
                // Ensure boolean
                if let Some(b) = value.as_bool() {
                    serde_json::json!(b)
                } else if let Some(s) = value.as_str() {
                    serde_json::json!(s == "true" || s == "1")
                } else {
                    value.clone()
                }
            }
            // Number, Currency, Date, Text, Email, Url, Phone — pass-through
            _ => value.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_dropdown_field() -> ClickUpCustomField {
        ClickUpCustomField {
            id: "uuid-1".into(),
            name: "Risk Level".into(),
            field_type: ClickUpFieldType::Dropdown,
            required: false,
            options: vec![
                ClickUpFieldOption {
                    id: "opt-1".into(),
                    name: "Low".into(),
                    orderindex: Some(0),
                },
                ClickUpFieldOption {
                    id: "opt-2".into(),
                    name: "Medium".into(),
                    orderindex: Some(1),
                },
                ClickUpFieldOption {
                    id: "opt-3".into(),
                    name: "High".into(),
                    orderindex: Some(2),
                },
            ],
        }
    }

    fn sample_labels_field() -> ClickUpCustomField {
        ClickUpCustomField {
            id: "uuid-2".into(),
            name: "Tags".into(),
            field_type: ClickUpFieldType::Labels,
            required: false,
            options: vec![
                ClickUpFieldOption {
                    id: "label-1".into(),
                    name: "Frontend".into(),
                    orderindex: None,
                },
                ClickUpFieldOption {
                    id: "label-2".into(),
                    name: "Backend".into(),
                    orderindex: None,
                },
            ],
        }
    }

    #[test]
    fn test_dropdown_transform_by_name() {
        let field = sample_dropdown_field();
        assert_eq!(field.transform_value(&json!("Medium")), json!(1));
        assert_eq!(field.transform_value(&json!("High")), json!(2));
    }

    #[test]
    fn test_dropdown_case_insensitive() {
        let field = sample_dropdown_field();
        assert_eq!(field.transform_value(&json!("low")), json!(0));
    }

    #[test]
    fn test_labels_transform_names_to_ids() {
        let field = sample_labels_field();
        let result = field.transform_value(&json!(["Frontend", "Backend"]));
        assert_eq!(result, json!(["label-1", "label-2"]));
    }

    #[test]
    fn test_checkbox_transform() {
        let field = ClickUpCustomField {
            id: "uuid-3".into(),
            name: "Done".into(),
            field_type: ClickUpFieldType::Checkbox,
            required: false,
            options: vec![],
        };
        assert_eq!(field.transform_value(&json!(true)), json!(true));
        assert_eq!(field.transform_value(&json!("true")), json!(true));
        assert_eq!(field.transform_value(&json!("false")), json!(false));
    }

    #[test]
    fn test_number_passthrough() {
        let field = ClickUpCustomField {
            id: "uuid-4".into(),
            name: "Story Points".into(),
            field_type: ClickUpFieldType::Number,
            required: false,
            options: vec![],
        };
        assert_eq!(field.transform_value(&json!(5)), json!(5));
    }

    #[test]
    fn test_metadata_deserialization() {
        let json = json!({
            "statuses": [
                { "name": "To Do", "type": "open" },
                { "name": "In Progress", "type": "custom" },
                { "name": "Done", "type": "closed" },
            ],
            "custom_fields": [
                {
                    "id": "uuid-1",
                    "name": "Story Points",
                    "field_type": "number",
                    "required": false,
                    "options": []
                }
            ]
        });
        let meta: ClickUpMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(meta.statuses.len(), 3);
        assert_eq!(meta.custom_fields.len(), 1);
        assert_eq!(meta.custom_fields[0].field_type, ClickUpFieldType::Number);
    }
}
