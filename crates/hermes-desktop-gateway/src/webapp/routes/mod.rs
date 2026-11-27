//! REST route groups the Webapp and native Desktop call, one file each (see [`super::api`]).

#[cfg(unix)]
pub(crate) mod connections;
pub(crate) mod startup;
pub(crate) mod store;
