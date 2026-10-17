pub mod domain;
pub mod error;
pub mod http;
pub mod pg;
pub mod ssv;
pub mod store;

fn log_callback_rejection(code: &'static str) {
    use crate::operations::logging::{self, SafeLogValue};

    let _ = logging::warn(
        "admob_callback_rejected",
        &[("error_code", SafeLogValue::String(code))],
    );
}
