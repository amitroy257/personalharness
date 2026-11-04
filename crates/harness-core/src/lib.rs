//! Personal Harness core: an OpenRouter-style local router over free LLM API
//! tiers, with model rankings built from public leaderboards and your own
//! (local-only) usage.

pub mod catalog;
pub mod commands;
pub mod harness;
pub mod normalize;
pub mod providers;
pub mod ranking;
pub mod relay;
pub mod router;
pub mod server;
pub mod settings;
pub mod signals;
pub mod sse;
pub mod store;
pub mod util;

pub use harness::Harness;
