use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

const MAX_SETTINGS_FILE_BYTES: usize = 16 * 1024;
const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorScheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default)]
    pub color_scheme: ColorScheme,
    #[serde(default)]
    pub auto_reopen_last_project: bool,
    #[serde(default)]
    pub restore_last_document_on_open: bool,
}

#[derive(Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    #[serde(flatten)]
    settings: AppSettings,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn settings_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("settings.json")
}

/// 把损坏或超限的文件挪到一边，从默认设置重新开始，不让一次坏数据永久卡死
/// 应用启动。
fn quarantine(path: &Path, reason: &str) {
    let target = path.with_extension(format!("json.corrupt-{}", now_ms()));
    match fs::rename(path, &target) {
        Ok(()) => log::warn!("设置文件已损坏（{reason}），已备份到 {}", target.display()),
        Err(error) => log::warn!("设置文件已损坏（{reason}），备份失败：{error}"),
    }
}

/// 缺失文件、损坏、超限、版本不认识都回退默认设置，不阻塞启动。
pub fn load(app_data_dir: &Path) -> AppSettings {
    let path = settings_path(app_data_dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AppSettings::default()
        }
        Err(error) => {
            log::warn!("读取设置文件失败：{error}");
            return AppSettings::default();
        }
    };
    if bytes.len() > MAX_SETTINGS_FILE_BYTES {
        quarantine(&path, "文件异常过大");
        return AppSettings::default();
    }
    match serde_json::from_slice::<SettingsFile>(&bytes) {
        Ok(parsed) if parsed.version == CURRENT_VERSION => parsed.settings,
        Ok(_) => {
            quarantine(&path, "版本不受支持");
            AppSettings::default()
        }
        Err(error) => {
            quarantine(&path, &error.to_string());
            AppSettings::default()
        }
    }
}

pub fn save(app_data_dir: &Path, settings: &AppSettings) -> Result<(), AppError> {
    let file = SettingsFile {
        version: CURRENT_VERSION,
        settings: settings.clone(),
    };
    let mut bytes = serde_json::to_vec(&file)
        .map_err(|error| AppError::Io(format!("设置序列化失败：{error}")))?;
    bytes.push(b'\n');

    fs::create_dir_all(app_data_dir)?;
    let path = settings_path(app_data_dir);
    let mut temporary = tempfile::NamedTempFile::new_in(app_data_dir)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|error| AppError::Io(error.to_string()))?;
    fs::File::open(app_data_dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_default() {
        let app_data = tempfile::tempdir().unwrap();
        let settings = load(app_data.path());
        assert_eq!(settings.color_scheme, ColorScheme::System);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let app_data = tempfile::tempdir().unwrap();
        let settings = AppSettings {
            color_scheme: ColorScheme::Dark,
            ..Default::default()
        };
        save(app_data.path(), &settings).unwrap();
        assert_eq!(load(app_data.path()).color_scheme, ColorScheme::Dark);
    }

    #[test]
    fn save_then_load_roundtrips_for_auto_reopen() {
        let app_data = tempfile::tempdir().unwrap();
        let settings = AppSettings {
            auto_reopen_last_project: true,
            ..Default::default()
        };
        save(app_data.path(), &settings).unwrap();
        assert!(load(app_data.path()).auto_reopen_last_project);
    }

    #[test]
    fn save_then_load_roundtrips_for_restore_last_document() {
        let app_data = tempfile::tempdir().unwrap();
        let settings = AppSettings {
            restore_last_document_on_open: true,
            ..Default::default()
        };
        save(app_data.path(), &settings).unwrap();
        assert!(load(app_data.path()).restore_last_document_on_open);
    }

    #[test]
    fn load_corrupted_file_falls_back_to_default_and_quarantines_it() {
        let app_data = tempfile::tempdir().unwrap();
        fs::create_dir_all(app_data.path()).unwrap();
        fs::write(settings_path(app_data.path()), b"not json").unwrap();

        let settings = load(app_data.path());
        assert_eq!(settings.color_scheme, ColorScheme::System);

        let quarantined = fs::read_dir(app_data.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("settings.json.corrupt-")
            });
        assert!(quarantined, "损坏文件应被改名隔离");
    }

    #[test]
    fn load_oversized_file_falls_back_to_default_and_quarantines_it() {
        let app_data = tempfile::tempdir().unwrap();
        fs::create_dir_all(app_data.path()).unwrap();
        let oversized = "x".repeat(MAX_SETTINGS_FILE_BYTES + 1);
        fs::write(settings_path(app_data.path()), oversized).unwrap();

        let settings = load(app_data.path());
        assert_eq!(settings.color_scheme, ColorScheme::System);
        assert!(!settings_path(app_data.path()).exists());
    }
}
