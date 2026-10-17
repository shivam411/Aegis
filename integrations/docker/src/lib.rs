//! Docker runtime for Aegis apps (`[run] runtime = "docker"` in `aegis.toml`).
//!
//! - [`args`]: the `docker` command lines (pure, unit-tested).
//! - [`Docker`]: runs them, checks whether Docker is usable, and writes the
//!   private env files containers get their environment from.
//! - [`DockerLauncher`]: plugs containers into the process supervisor, so
//!   restarts, crash loops, logs and stops work as for process apps.
//!
//! Images are built (or pulled and tagged) during a deployment's Build
//! stage as `aegis/<project id>:<release>`; rollbacks reuse that image.

pub mod args;
mod cli;
mod launcher;

pub use cli::{docker_bin, probe, run_docker, ContainerState, Docker, DockerStatus};
pub use launcher::{ContainerSettings, DockerLauncher, LimitsSource};
