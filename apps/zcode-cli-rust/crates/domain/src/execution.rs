//! Typed session execution state. Serialized strings are unchanged from the
//! previous `String` fields, so stored sessions and wire projections stay compatible.
use serde::{Deserialize, Deserializer, Serialize};

/// Permission mode, matching Node `execution-state.ts` (`plan` is an input alias, see [`Mode::parse`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Legacy native sessions without a stored mode deserialize as build.
    #[default]
    Build,
    Edit,
    Yolo,
    Auto,
}

impl Mode {
    /// Parse a stored or imported mode. `plan` keeps the build base mode; callers
    /// record the plan flag separately (Node keeps `planEnabled` beside the mode).
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "build" | "plan" => Self::Build,
            "edit" => Self::Edit,
            "yolo" => Self::Yolo,
            "auto" => Self::Auto,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Edit => "edit",
            Self::Yolo => "yolo",
            Self::Auto => "auto",
        }
    }
}

impl<'de> Deserialize<'de> for Mode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| {
            serde::de::Error::unknown_variant(&value, &["build", "edit", "yolo", "auto", "plan"])
        })
    }
}

/// Session lifecycle phase as projected to the App.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Draft,
    Prewarming,
    Running,
    CompletedSuccess,
    CompletedInterrupted,
    Error,
}

impl Phase {
    pub fn ended(self) -> bool {
        matches!(self, Self::CompletedSuccess | Self::CompletedInterrupted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_strings_match_previous_fields() {
        assert_eq!(serde_json::to_value(Mode::Yolo).unwrap(), "yolo");
        assert_eq!(
            serde_json::to_value(Phase::CompletedInterrupted).unwrap(),
            "completedInterrupted"
        );
        assert_eq!(
            serde_json::from_value::<Phase>("draft".into()).unwrap(),
            Phase::Draft
        );
        assert_eq!(
            serde_json::from_value::<Mode>("plan".into()).unwrap(),
            Mode::Build
        );
        assert!(serde_json::from_value::<Mode>("unknown".into()).is_err());
        assert!(serde_json::from_value::<Phase>("unknown".into()).is_err());
    }
}
