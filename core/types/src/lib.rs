use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

