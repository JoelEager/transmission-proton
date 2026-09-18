use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use arboard::Clipboard;
use chrono::Local;
use clap::Parser;
use serde::Serialize;
use serde_json::Value;

#[derive(Parser, Debug)]
#[command(author, version, about = "Updates Transmission peer-port from clipboard and launches Transmission.")]
pub struct Args {
    /// Directory containing the `transmission/settings.json` file.
    /// Defaults to %LOCALAPPDATA% on Windows, or ~/.config on Linux/macOS.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// Path to the Transmission executable.
    /// Defaults to `C:\Program Files\Transmission\transmission-qt.exe` on Windows, or `transmission-qt` on Linux/macOS.
    #[arg(long)]
    pub transmission_path: Option<PathBuf>,

    /// Path to the log file. Defaults to `log.txt` in the current directory.
    #[arg(long, default_value = "log.txt")]
    pub log_file: PathBuf,

    /// Override port value directly instead of reading from clipboard.
    #[arg(long)]
    pub port: Option<u16>,

    /// Skip launching Transmission (useful for dry runs and testing).
    #[arg(long)]
    pub skip_launch: bool,
}

pub struct Logger {
    log_path: PathBuf,
}

impl Logger {
    pub fn new(log_path: PathBuf) -> Self {
        Self { log_path }
    }

    pub fn log(&self, msg: &str) {
        println!("{}", msg);
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(file, "{}", msg);
        }
    }
}

fn default_config_dir() -> Result<PathBuf, String> {
    if cfg!(target_os = "windows") {
        std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .map_err(|_| "LOCALAPPDATA environment variable is not set".to_string())
    } else {
        dirs::config_dir().ok_or_else(|| "Could not determine user config directory".to_string())
    }
}

fn default_transmission_path() -> PathBuf {
    if cfg!(target_os = "windows") {
        PathBuf::from(r"C:\Program Files\Transmission\transmission-qt.exe")
    } else {
        PathBuf::from("transmission-qt")
    }
}

fn get_port_from_clipboard() -> Result<u16, String> {
    let mut clipboard = Clipboard::new().map_err(|e| format!("Failed to access clipboard: {}", e))?;
    let content = clipboard
        .get_text()
        .map_err(|e| format!("Failed to get clipboard text: {}", e))?;

    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("Clipboard is empty".to_string());
    }

    let port: u16 = trimmed
        .parse()
        .map_err(|_| format!("Invalid port number in clipboard: '{}'", trimmed))?;

    if port == 0 {
        return Err("Port number cannot be 0".to_string());
    }

    Ok(port)
}

fn update_settings_file(config_dir: &Path, port: u16, logger: &Logger) -> Result<(), String> {
    let settings_path = config_dir.join("transmission").join("settings.json");
    let settings_path_str = settings_path.to_string_lossy();
    logger.log(&format!("Opening {}", settings_path_str));

    if !settings_path.exists() {
        return Err(format!("Settings file does not exist: {}", settings_path_str));
    }

    let content = fs::read_to_string(&settings_path)
        .map_err(|e| format!("Failed to read {}: {}", settings_path_str, e))?;

    let mut settings: Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse JSON from {}: {}", settings_path_str, e))?;

    if !settings.is_object() {
        return Err(format!("Settings file JSON root is not an object: {}", settings_path_str));
    }

    settings["peer-port"] = Value::from(port);

    // Format JSON with 4 spaces indentation to match previous python output
    let buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(buf, formatter);
    settings
        .serialize(&mut ser)
        .map_err(|e| format!("Failed to serialize settings JSON: {}", e))?;

    let mut updated_json = String::from_utf8(ser.into_inner())
        .map_err(|e| format!("Failed to convert serialized JSON to string: {}", e))?;
    updated_json.push('\n');

    fs::write(&settings_path, updated_json)
        .map_err(|e| format!("Failed to write updated settings to {}: {}", settings_path_str, e))?;

    logger.log(&format!("Successfully updated port to {}", port));
    Ok(())
}

fn run_app(args: Args) -> Result<(), String> {
    let logger = Logger::new(args.log_file);

    let config_dir = match args.config_dir {
        Some(dir) => dir,
        None => default_config_dir()?,
    };

    let port = match args.port {
        Some(p) => {
            if p == 0 {
                return Err("Port number cannot be 0".to_string());
            }
            p
        }
        None => get_port_from_clipboard()?,
    };

    update_settings_file(&config_dir, port, &logger)?;

    if args.skip_launch {
        logger.log("Transmission launch skipped per --skip-launch flag");
        return Ok(());
    }

    let trans_path = args.transmission_path.unwrap_or_else(default_transmission_path);
    let start_time = Local::now().format("%Y-%m-%d %H:%M:%S");
    logger.log(&format!("Starting Transmission at {} ({})", start_time, trans_path.display()));

    let status = Command::new(&trans_path)
        .status()
        .map_err(|e| format!("Failed to launch Transmission at '{}': {}", trans_path.display(), e))?;

    let finish_time = Local::now().format("%Y-%m-%d %H:%M:%S");
    logger.log(&format!("Transmission finished at {} with status: {}", finish_time, status));
    logger.log("Transmission done");

    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Err(err) = run_app(args) {
        eprintln!("Error: {}", err);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_update_settings_file() {
        let dir = tempdir().unwrap();
        let trans_dir = dir.path().join("transmission");
        fs::create_dir_all(&trans_dir).unwrap();
        let settings_file = trans_dir.join("settings.json");

        let initial_json = r#"{
    "peer-port": 50000,
    "download-dir": "/downloads"
}"#;
        fs::write(&settings_file, initial_json).unwrap();

        let log_file = dir.path().join("log.txt");
        let logger = Logger::new(log_file.clone());

        update_settings_file(dir.path(), 51413, &logger).unwrap();

        let updated_content = fs::read_to_string(&settings_file).unwrap();
        let parsed: Value = serde_json::from_str(&updated_content).unwrap();

        assert_eq!(parsed["peer-port"], 51413);
        assert_eq!(parsed["download-dir"], "/downloads");

        let log_content = fs::read_to_string(&log_file).unwrap();
        assert!(log_content.contains("Successfully updated port to 51413"));
    }

    #[test]
    fn test_run_app_with_port_override_and_skip_launch() {
        let dir = tempdir().unwrap();
        let trans_dir = dir.path().join("transmission");
        fs::create_dir_all(&trans_dir).unwrap();
        let settings_file = trans_dir.join("settings.json");

        fs::write(&settings_file, r#"{"peer-port": 12345}"#).unwrap();

        let log_file = dir.path().join("log.txt");

        let args = Args {
            config_dir: Some(dir.path().to_path_buf()),
            transmission_path: None,
            log_file: log_file.clone(),
            port: Some(60000),
            skip_launch: true,
        };

        assert!(run_app(args).is_ok());

        let updated_content = fs::read_to_string(&settings_file).unwrap();
        let parsed: Value = serde_json::from_str(&updated_content).unwrap();
        assert_eq!(parsed["peer-port"], 60000);

        let log_content = fs::read_to_string(&log_file).unwrap();
        assert!(log_content.contains("Successfully updated port to 60000"));
        assert!(log_content.contains("Transmission launch skipped"));
    }

    #[test]
    fn test_run_app_missing_settings_file() {
        let dir = tempdir().unwrap();
        let log_file = dir.path().join("log.txt");

        let args = Args {
            config_dir: Some(dir.path().to_path_buf()),
            transmission_path: None,
            log_file: log_file,
            port: Some(60000),
            skip_launch: true,
        };

        let result = run_app(args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Settings file does not exist"));
    }

    #[test]
    fn test_port_zero_validation() {
        let dir = tempdir().unwrap();
        let log_file = dir.path().join("log.txt");

        let args = Args {
            config_dir: Some(dir.path().to_path_buf()),
            transmission_path: None,
            log_file: log_file,
            port: Some(0),
            skip_launch: true,
        };

        let result = run_app(args);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Port number cannot be 0");
    }
}
