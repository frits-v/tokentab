//! Ports-and-adapters layout: `domain` holds the model and the arithmetic,
//! `ports` the traits the outside world is reached through, `adapters` the
//! concrete transcript readers, price-list loaders and renderers.

pub mod adapters;
pub mod domain;
pub mod ports;
