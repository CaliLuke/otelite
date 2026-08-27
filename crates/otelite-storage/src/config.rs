//! Storage configuration

use cron::Schedule;
use otelite_core::storage::{Result, StorageError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Data directory path
    pub data_dir: PathBuf,

    /// Retention period in days (0 disables automatic retention, maximum 365)
    pub retention_days: u32,

    /// Purge schedule (cron-like format)
    pub purge_schedule: String,

    /// Enable automatic purging
    pub auto_purge_enabled: bool,

    /// Batch size for purge operations
    pub purge_batch_size: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: Self::default_data_dir(),
            retention_days: 90,
            purge_schedule: "0 2 * * *".to_string(), // Daily at 2 AM
            auto_purge_enabled: true,
            purge_batch_size: 1000,
        }
    }
}

impl StorageConfig {
    pub fn default_data_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".otelite")
            .join("data")
    }

    /// Pathname of the SQLite database opened by this configuration.
    pub fn database_path(&self) -> PathBuf {
        if self.data_dir.to_string_lossy().starts_with(":memory:") {
            self.data_dir.clone()
        } else {
            self.data_dir.join("otelite.db")
        }
    }

    /// Create configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let mut config = Self::default();

        if let Ok(data_dir) = std::env::var("OTELITE_DATA_DIR") {
            config.data_dir = PathBuf::from(data_dir);
        }

        if let Ok(retention_days) = std::env::var("OTELITE_RETENTION_DAYS") {
            config.retention_days = retention_days
                .parse()
                .map_err(|e| StorageError::ConfigError(format!("Invalid retention_days: {}", e)))?;
        }

        if let Ok(purge_schedule) = std::env::var("OTELITE_PURGE_SCHEDULE") {
            config.purge_schedule = purge_schedule;
        }

        if let Ok(auto_purge) = std::env::var("OTELITE_AUTO_PURGE_ENABLED") {
            config.auto_purge_enabled = auto_purge.parse().map_err(|e| {
                StorageError::ConfigError(format!(
                    "Invalid OTELITE_AUTO_PURGE_ENABLED value {auto_purge:?}: {e}"
                ))
            })?;
        }

        config.validate()?;
        Ok(config)
    }

    /// Resolve environment configuration, then apply an optional CLI data directory override.
    pub fn from_env_with_data_dir(data_dir: Option<PathBuf>) -> Result<Self> {
        let mut config = Self::from_env()?;
        if let Some(data_dir) = data_dir {
            config.data_dir = data_dir;
        }
        config.validate()?;
        Ok(config)
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        if self.retention_days > 365 {
            return Err(StorageError::ConfigError(
                "Retention days must be between 0 and 365; 0 disables automatic retention"
                    .to_string(),
            ));
        }

        self.parsed_purge_schedule()?;

        if self.purge_batch_size == 0 {
            return Err(StorageError::ConfigError(
                "Purge batch size must be greater than 0".to_string(),
            ));
        }

        Ok(())
    }

    pub(crate) fn parsed_purge_schedule(&self) -> Result<Schedule> {
        let field_count = self.purge_schedule.split_whitespace().count();
        let expression = match field_count {
            5 => format!("0 {} *", self.purge_schedule),
            6 => format!("{} *", self.purge_schedule),
            7 => self.purge_schedule.clone(),
            _ => {
                return Err(StorageError::ConfigError(format!(
                    "Invalid purge schedule {:?}: expected 5, 6, or 7 cron fields",
                    self.purge_schedule
                )))
            },
        };
        expression.parse::<Schedule>().map_err(|e| {
            StorageError::ConfigError(format!(
                "Invalid purge schedule {:?}: {}",
                self.purge_schedule, e
            ))
        })
    }

    /// Builder method to set data directory
    pub fn with_data_dir(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = data_dir;
        self
    }

    /// Builder method to set retention days
    pub fn with_retention_days(mut self, days: u32) -> Self {
        self.retention_days = days;
        self
    }

    /// Builder method to set purge schedule
    pub fn with_purge_schedule(mut self, schedule: String) -> Self {
        self.purge_schedule = schedule;
        self
    }

    /// Builder method to enable/disable auto purge
    pub fn with_auto_purge(mut self, enabled: bool) -> Self {
        self.auto_purge_enabled = enabled;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::ffi::OsString;

    const STORAGE_ENV: [&str; 4] = [
        "OTELITE_DATA_DIR",
        "OTELITE_RETENTION_DAYS",
        "OTELITE_PURGE_SCHEDULE",
        "OTELITE_AUTO_PURGE_ENABLED",
    ];
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct StorageEnv(Vec<(&'static str, Option<OsString>)>);

    impl StorageEnv {
        fn cleared() -> Self {
            let values = STORAGE_ENV
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect();
            for key in STORAGE_ENV {
                std::env::remove_var(key);
            }
            Self(values)
        }
    }

    impl Drop for StorageEnv {
        fn drop(&mut self) {
            for (key, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn test_default_config() {
        let config = StorageConfig::default();
        assert_eq!(config.retention_days, 90);
        assert_eq!(config.purge_schedule, "0 2 * * *");
        assert!(config.auto_purge_enabled);
        assert_eq!(config.purge_batch_size, 1000);
    }

    #[test]
    fn from_env_reads_supported_storage_settings() {
        let _lock = ENV_LOCK.lock();
        let _environment = StorageEnv::cleared();
        std::env::set_var("OTELITE_DATA_DIR", "/tmp/otelite-from-env");
        std::env::set_var("OTELITE_RETENTION_DAYS", "14");
        std::env::set_var("OTELITE_PURGE_SCHEDULE", "15 3 * * *");
        std::env::set_var("OTELITE_AUTO_PURGE_ENABLED", "false");

        let config = StorageConfig::from_env().unwrap();

        assert_eq!(config.data_dir, PathBuf::from("/tmp/otelite-from-env"));
        assert_eq!(config.retention_days, 14);
        assert_eq!(config.purge_schedule, "15 3 * * *");
        assert!(!config.auto_purge_enabled);
    }

    #[test]
    fn cli_data_dir_overrides_environment_data_dir() {
        let _lock = ENV_LOCK.lock();
        let _environment = StorageEnv::cleared();
        std::env::set_var("OTELITE_DATA_DIR", "/tmp/otelite-from-env");

        let config =
            StorageConfig::from_env_with_data_dir(Some(PathBuf::from("/tmp/otelite-from-cli")))
                .unwrap();

        assert_eq!(config.data_dir, PathBuf::from("/tmp/otelite-from-cli"));
    }

    #[test]
    fn invalid_auto_purge_environment_value_is_rejected() {
        let _lock = ENV_LOCK.lock();
        let _environment = StorageEnv::cleared();
        std::env::set_var("OTELITE_AUTO_PURGE_ENABLED", "sometimes");

        assert!(StorageConfig::from_env().is_err());
    }

    #[test]
    fn test_default_data_dir_is_dotfile() {
        // Must stay under ~/.otelite/data — not ~/Library or ~/.local/share.
        let dir = StorageConfig::default_data_dir();
        let home = dirs::home_dir().expect("home dir must exist in test env");
        assert!(
            dir.starts_with(&home),
            "data dir {:?} should be under home {:?}",
            dir,
            home
        );
        let rel = dir.strip_prefix(&home).unwrap();
        assert_eq!(
            rel,
            std::path::Path::new(".otelite/data"),
            "data dir must be ~/.otelite/data, got {:?}",
            dir
        );
    }

    #[test]
    fn database_path_appends_filename_to_data_directory() {
        let config =
            StorageConfig::default().with_data_dir(PathBuf::from("/tmp/otelite-agent-query"));
        assert_eq!(
            config.database_path(),
            PathBuf::from("/tmp/otelite-agent-query/otelite.db")
        );
    }

    #[test]
    fn test_config_validation() {
        let mut config = StorageConfig::default();
        assert!(config.validate().is_ok());

        config.retention_days = 0;
        assert!(config.validate().is_ok());

        config.retention_days = 366;
        assert!(config.validate().is_err());

        config.retention_days = 90;
        config.purge_batch_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_default_purge_schedule_is_valid() {
        assert!(StorageConfig::default().parsed_purge_schedule().is_ok());
    }

    #[test]
    fn test_invalid_purge_schedule_is_rejected() {
        let config =
            StorageConfig::default().with_purge_schedule("not a cron schedule".to_string());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_builder() {
        let config = StorageConfig::default()
            .with_retention_days(30)
            .with_auto_purge(false);

        assert_eq!(config.retention_days, 30);
        assert!(!config.auto_purge_enabled);
    }
}
