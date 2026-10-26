use serde::{Deserialize, Serialize};

use crate::shared::text::javascript_trim;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DataRequestType {
    Access,
    Erasure,
    Portability,
    Rectification,
    Restriction,
    Objection,
}

impl DataRequestType {
    pub const fn requires_details(self) -> bool {
        matches!(self, Self::Rectification)
    }

    pub const fn is_retired(self) -> bool {
        matches!(self, Self::Restriction | Self::Objection)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Erasure => "erasure",
            Self::Portability => "portability",
            Self::Rectification => "rectification",
            Self::Restriction => "restriction",
            Self::Objection => "objection",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "access" => Some(Self::Access),
            "erasure" => Some(Self::Erasure),
            "portability" => Some(Self::Portability),
            "rectification" => Some(Self::Rectification),
            "restriction" => Some(Self::Restriction),
            "objection" => Some(Self::Objection),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataRequestStatus {
    Pending,
    InProgress,
    Completed,
    Rejected,
}

impl DataRequestStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DataRequestTransition {
    InProgress,
    Completed,
    Rejected,
}

impl DataRequestTransition {
    pub const fn as_status(self) -> DataRequestStatus {
        match self {
            Self::InProgress => DataRequestStatus::InProgress,
            Self::Completed => DataRequestStatus::Completed,
            Self::Rejected => DataRequestStatus::Rejected,
        }
    }
}

/// The repository evaluates this policy against the row locked in its transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RequestReview {
    Update(DataRequestStatus),
    ScheduleErasure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RequestReviewError {
    InvalidTransition,
    MissingResponse,
}

impl RequestReview {
    pub(super) fn decide(
        request_type: DataRequestType,
        current_status: DataRequestStatus,
        transition: DataRequestTransition,
        response: Option<&str>,
    ) -> Result<Self, RequestReviewError> {
        if request_type.is_retired() {
            return Err(RequestReviewError::InvalidTransition);
        }
        let requested_status = transition.as_status();
        if request_type.requires_details()
            && matches!(
                requested_status,
                DataRequestStatus::Completed | DataRequestStatus::Rejected
            )
            && normalized_request_text(response).is_none()
        {
            return Err(RequestReviewError::MissingResponse);
        }
        let allowed = match current_status {
            DataRequestStatus::Pending => matches!(
                requested_status,
                DataRequestStatus::InProgress | DataRequestStatus::Rejected
            ),
            DataRequestStatus::InProgress => matches!(
                requested_status,
                DataRequestStatus::Completed | DataRequestStatus::Rejected
            ),
            DataRequestStatus::Completed | DataRequestStatus::Rejected => false,
        };
        if !allowed {
            return Err(RequestReviewError::InvalidTransition);
        }
        if request_type == DataRequestType::Erasure
            && transition == DataRequestTransition::Completed
        {
            Ok(Self::ScheduleErasure)
        } else {
            Ok(Self::Update(requested_status))
        }
    }

    pub(super) const fn stored_status(self) -> DataRequestStatus {
        match self {
            Self::Update(status) => status,
            Self::ScheduleErasure => DataRequestStatus::InProgress,
        }
    }
}

pub(super) fn normalized_request_text(value: Option<&str>) -> Option<&str> {
    value.map(javascript_trim).filter(|value| !value.is_empty())
}

pub const REQUEST_DETAILS_MAX_LENGTH: usize = 2_000;

/// Legacy self-service request types may omit details. Manual requests need a
/// bounded explanation; never put this user-supplied content in operational logs.
pub fn valid_request_details(request_type: DataRequestType, details: Option<&str>) -> bool {
    let nonempty = normalized_request_text(details).is_some();
    !request_type.is_retired()
        && (!request_type.requires_details() || nonempty)
        && valid_request_text(details)
}

pub fn valid_request_text(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        let value = javascript_trim(value);
        value.chars().count() <= REQUEST_DETAILS_MAX_LENGTH
            && !value
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_messages_use_unicode_lengths_and_reject_control_characters() {
        let message = "🦀".repeat(REQUEST_DETAILS_MAX_LENGTH);
        assert!(valid_request_details(
            DataRequestType::Rectification,
            Some(&format!("\u{feff}  {message}\n")),
        ));
        assert!(!valid_request_text(Some(&format!("{message}🦀"))));
        assert!(valid_request_text(Some("First line\r\nSecond\tline")));
        for control in ['\0', '\u{0008}', '\u{001b}', '\u{007f}'] {
            assert!(!valid_request_text(Some(&format!("before{control}after"))));
        }
        assert!(valid_request_text(None));
        assert!(valid_request_details(DataRequestType::Access, None));
        assert!(!valid_request_details(
            DataRequestType::Rectification,
            Some(" \n\t")
        ));
    }

    #[test]
    fn reviews_preserve_allowed_transitions_and_schedule_erasure_asynchronously() {
        for request_type in [
            DataRequestType::Access,
            DataRequestType::Erasure,
            DataRequestType::Portability,
            DataRequestType::Rectification,
            DataRequestType::Restriction,
            DataRequestType::Objection,
        ] {
            for current in [
                DataRequestStatus::Pending,
                DataRequestStatus::InProgress,
                DataRequestStatus::Completed,
                DataRequestStatus::Rejected,
            ] {
                for transition in [
                    DataRequestTransition::InProgress,
                    DataRequestTransition::Completed,
                    DataRequestTransition::Rejected,
                ] {
                    let expected = match (request_type, current, transition) {
                        (DataRequestType::Restriction | DataRequestType::Objection, _, _) => {
                            Err(RequestReviewError::InvalidTransition)
                        }
                        (_, DataRequestStatus::Pending, DataRequestTransition::InProgress) => {
                            Ok(RequestReview::Update(DataRequestStatus::InProgress))
                        }
                        (
                            _,
                            DataRequestStatus::Pending | DataRequestStatus::InProgress,
                            DataRequestTransition::Rejected,
                        ) => Ok(RequestReview::Update(DataRequestStatus::Rejected)),
                        (
                            DataRequestType::Erasure,
                            DataRequestStatus::InProgress,
                            DataRequestTransition::Completed,
                        ) => Ok(RequestReview::ScheduleErasure),
                        (_, DataRequestStatus::InProgress, DataRequestTransition::Completed) => {
                            Ok(RequestReview::Update(DataRequestStatus::Completed))
                        }
                        _ => Err(RequestReviewError::InvalidTransition),
                    };
                    assert_eq!(
                        RequestReview::decide(
                            request_type,
                            current,
                            transition,
                            Some("Public reply")
                        ),
                        expected,
                        "{request_type:?}, {current:?}, {transition:?}",
                    );
                }
            }
        }
        assert_eq!(
            RequestReview::ScheduleErasure.stored_status(),
            DataRequestStatus::InProgress
        );
    }

    #[test]
    fn completing_and_rejecting_corrections_requires_a_nonempty_response() {
        for (current, transition) in [
            (DataRequestStatus::Pending, DataRequestTransition::Rejected),
            (
                DataRequestStatus::InProgress,
                DataRequestTransition::Completed,
            ),
            (
                DataRequestStatus::InProgress,
                DataRequestTransition::Rejected,
            ),
        ] {
            for response in [None, Some(""), Some(" \n\t\u{feff}")] {
                assert_eq!(
                    RequestReview::decide(
                        DataRequestType::Rectification,
                        current,
                        transition,
                        response
                    ),
                    Err(RequestReviewError::MissingResponse),
                );
            }
        }
    }
}
