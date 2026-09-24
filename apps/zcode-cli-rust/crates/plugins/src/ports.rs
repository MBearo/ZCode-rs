//! External effects of plugin management: storage root, cancellation, HTTP
//! and system Git (spec rust-m10-4-plugin-sources §2).
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub struct Ports<'a> {
    pub storage: &'a Path,
    pub cancel: &'a CancellationToken,
    pub http: &'a dyn crate::http::Http,
    pub git: &'a crate::git::Git,
}
