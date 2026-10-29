//! Website privacy uses the verified website cookie without requiring
//! onboarding or paid billing. HTTP guards and data deletion orchestration
//! remain separate; existing domain services own all persistent changes.
mod http;
mod service;

pub use http::{WebPrivacyState, routes};
pub use service::WebDataDeletionService;
