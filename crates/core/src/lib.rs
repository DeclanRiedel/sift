//! `sift-core` — shared server-internal behavior that is neither protocol nor
//! driver-trait surface.

pub mod ai_delegation;
pub mod catalog;
pub mod comparison;
pub mod performance;
pub mod schema_diff;

pub mod sqlite_parameters;

pub mod transfer_columns;

pub mod performance_pair;
pub mod sqlite_maintenance;
pub mod sqlserver_index_maintenance;
pub mod sqlserver_metadata;

pub mod transfer_expressions;

mod transfer_decimals;

pub mod performance_suite;

pub mod performance_result;
