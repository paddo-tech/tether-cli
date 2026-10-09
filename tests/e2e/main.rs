//! End-to-end tests. Each test runs Tether machines in Docker containers that sync through a
//! git server container. Package managers run only inside the containers: most machines have
//! logging shims, and the upgrade test has real npm.
//!
//! The suite runs only with TETHER_E2E=1, and skips with a message when Docker is missing.
//! The first test builds the binaries and images with tests/e2e/images.sh. See AGENTS.md.

mod config_merge;
mod contract;
mod fleet;
mod harness;
mod identity;
mod inbox;
mod linux;
mod trust;
mod unattended;
mod upgrade;
