//! Domain model, TOS order parser, statement parser, trade assembly,
//! P&L and reconciliation for Wingspan.
//!
//! Everything here is pure, synchronous and unit-testable; I/O lives in
//! `wingspan-marketdata` and `wingspan-server`.

pub mod contract;
pub mod market;
