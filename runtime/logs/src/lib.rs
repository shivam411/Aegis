use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initializes the tracing logging framework.
pub fn init_logging(default_level: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}
