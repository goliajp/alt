//! Authenticated native/Git data-plane transport over one publication authority.
mod publication;
pub use publication::*;
mod route;
pub use route::*;
mod native;
pub use native::*;
mod http;
pub use http::*;
mod git;
mod git_receive;
pub use git::execute_git;
pub mod operator;
