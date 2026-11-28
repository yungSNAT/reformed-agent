//! REST route groups the Webapp and native Desktop call, one file each (see [`super::api`]).

#[cfg(unix)]
pub(crate) mod connections;
#[cfg(unix)]
pub(crate) mod files;
pub(crate) mod startup;
pub(crate) mod store;
