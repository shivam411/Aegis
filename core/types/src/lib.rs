use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let uuid = Uuid::from_str(s)?;
                Ok(Self(uuid))
            }
        }
    };
}

define_id!(ProjectId);
define_id!(ReleaseId);
define_id!(DeploymentId);
define_id!(ProcessId);
define_id!(ServerId);
define_id!(EventId);
define_id!(ArtifactId);

pub mod error;
pub use error::AegisError;

use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub event_type: String,
    #[serde(default)]
    pub aggregate_type: String,
    #[serde(default)]
    pub aggregate_id: String,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    pub payload_json: String,
    pub created_at: String,
}

fn default_schema_version() -> u32 {
    1
}

impl Event {
    pub fn new(event_type: impl Into<String>, payload_json: impl Into<String>) -> Self {
        Self {
            id: EventId::new(),
            schema_version: 1,
            event_type: event_type.into(),
            aggregate_type: "System".to_string(),
            aggregate_id: String::new(),
            correlation_id: None,
            causation_id: None,
            metadata: HashMap::new(),
            payload_json: payload_json.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_macro_operations() {
        let id1 = ProjectId::new();
        let id2 = ProjectId::default();
        assert_ne!(id1, id2);

        let uuid = id1.as_uuid();
        let id_from_uuid = ProjectId::from_uuid(uuid);
        assert_eq!(id1, id_from_uuid);

        let id_str = id1.to_string();
        let parsed_id: ProjectId = id_str.parse().unwrap();
        assert_eq!(id1, parsed_id);

        let invalid_parse = "not-a-valid-uuid".parse::<ProjectId>();
        assert!(invalid_parse.is_err());
    }

    #[test]
    fn test_event_construction_and_serde() {
        let mut event = Event::new("TestEvent", r#"{"key":"value"}"#);
        event.aggregate_type = "Project".to_string();
        event.aggregate_id = "proj-123".to_string();
        event.correlation_id = Some("corr-456".to_string());
        event.causation_id = Some("cause-789".to_string());
        event.metadata.insert("env".to_string(), "test".to_string());

        assert_eq!(event.event_type, "TestEvent");
        assert_eq!(event.schema_version, 1);
        assert_eq!(event.aggregate_type, "Project");

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, event.id);
        assert_eq!(deserialized.event_type, "TestEvent");
        assert_eq!(deserialized.correlation_id, Some("corr-456".to_string()));
        assert_eq!(deserialized.metadata.get("env").unwrap(), "test");
    }
}
