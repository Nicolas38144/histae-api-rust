pub mod domain;
pub mod http;
pub mod maintenance;
pub mod pg;
pub mod s3;
pub mod service;
pub mod storage;

pub use pg::PgPhotoRepository;
pub use s3::S3ObjectStorage;
pub use service::{PhotoDeletionHandler, PhotoService};
