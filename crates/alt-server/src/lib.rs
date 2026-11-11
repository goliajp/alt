//! Authenticated native/Git data-plane transport over one publication authority.
mod publication;
pub use publication::*;
mod route;
pub use route::*;
mod native;
pub use native::*;
mod http;
pub use http::*;
mod compaction;
mod git;
mod git_packs;
mod git_read;
mod git_response;
pub use git::execute_git;
pub mod operator;

mod browse;
mod records;
mod web_assets;
mod web_route;
pub use web_assets::Assets as WebAssets;
mod web_review;

pub mod cloud;

mod portable;
pub use portable::route::{ExportAction, ImportAction, PortableRoute};

pub mod change_links;

#[cfg(test)]
mod json_tests;
