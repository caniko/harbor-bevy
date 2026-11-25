use std::{fs, io};

use minidumper_child::MinidumperChild;
use uuid::Uuid;

use super::files::{read_manifest, write_file, write_manifest};
use super::{CrashDumpConfig, CrashDumpError, CrashDumpSession, validate_instance_id};

impl CrashDumpSession {
    /// Start the native reporter. The executable must dispatch
    /// [`run_crash_reporter_process_if_requested`] at startup using the same
    /// crash root and reporter flag. Returns false when test mode suppresses startup.
    pub fn start_minidumper(&self) -> Result<bool, CrashDumpError> {
        let config = self.config();
        if config
            .test_mode_env_var
            .as_ref()
            .and_then(std::env::var_os)
            .is_some()
        {
            return Ok(false);
        }
        let flag = validate_flag(&config.reporter_flag)?;
        let server_arg = format!("{flag}={}", config.instance_id);
        if std::env::args_os()
            .skip(1)
            .any(|arg| arg.to_string_lossy().starts_with(&server_arg))
        {
            return Err(CrashDumpError::InvalidReporterArg(server_arg));
        }

        let mut client = self.inner.native_reporter.lock();
        if client.is_none() {
            // The parent supplies a callback because the dependency requires one
            // before spawning. The actual callback is installed by the child.
            let handle = MinidumperChild::new()
                .with_server_arg(server_arg)
                .on_minidump(|_, _| {})
                .spawn()?;
            *client = Some(handle);
        }
        Ok(true)
    }
}

/// Dispatch the reporter before the application's own argument parser. It
/// returns when no reporter flag is present; a successful child exits after
/// handling the native crash. Rust panics and explicit fatal exits use the
/// separate text/JSON reporting paths.
pub fn run_crash_reporter_process_if_requested(
    config: &CrashDumpConfig,
) -> Result<(), CrashDumpError> {
    let flag = validate_flag(&config.reporter_flag)?;
    let prefix = format!("{flag}=");
    // Upstream matches the prefix and then splits on '='. Reject ambiguous
    // invocations before handing the argument off to it.
    let mut args = std::env::args_os().skip(1).filter_map(|arg| {
        let arg = arg.to_str()?;
        arg.strip_prefix(&prefix).map(str::to_owned)
    });
    let Some(argument) = args.next() else {
        return Ok(());
    };
    if args.next().is_some() {
        return Err(CrashDumpError::InvalidReporterArg(argument));
    }
    let (session_id, socket) = parse_argument(&argument)?;
    let server_arg = format!("{prefix}{session_id}={socket}");
    let dir = config.root_dir().join(session_id);
    if !fs::symlink_metadata(&dir)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reporter path is not a directory",
        )
        .into());
    }
    let manifest = read_manifest(&dir, session_id)?;
    let app_name = manifest["app_name"]
        .as_str()
        .unwrap_or(&config.app_name)
        .to_owned();
    let created_at = manifest["created_at"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let session_id = session_id.to_owned();
    // Match the complete argument. With the upstream prefix check, a shorter
    // match could accidentally select a different session.
    MinidumperChild::new()
        .with_server_arg(server_arg)
        .with_crashes_dir(dir.clone())
        .on_minidump(move |bytes, path| {
            let result = (|| -> io::Result<()> {
                let original = path.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "missing native dump filename")
                })?;
                // The dependency removes its original after the callback.
                let filename = format!("captured-{}", original.to_string_lossy());
                write_file(&dir.join(&filename), &bytes)?;
                write_file(
                    &dir.join("minidumper_report.txt"),
                    format!("Native dump: {filename}\n").as_bytes(),
                )?;
                write_manifest(&dir, &app_name, &session_id, &created_at)
            })();
            if let Err(error) = result {
                eprintln!("Could not persist native crash dump: {error}");
            }
        })
        .spawn()?;
    Ok(())
}

fn validate_flag(flag: &str) -> Result<&str, CrashDumpError> {
    if flag.strip_prefix("--").is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    }) {
        Ok(flag)
    } else {
        Err(CrashDumpError::InvalidReporterArg(flag.to_owned()))
    }
}

fn parse_argument(argument: &str) -> Result<(&str, &str), CrashDumpError> {
    let invalid = || CrashDumpError::InvalidReporterArg(argument.to_owned());
    let (session, socket) = argument.split_once('=').ok_or_else(invalid)?;
    validate_instance_id(session)?;
    let socket_id = socket.rsplit_once("temp-socket-").ok_or_else(invalid)?.1;
    let id = Uuid::parse_str(socket_id).map_err(|_| invalid())?;
    if socket != minidumper_child::make_socket_name(id) {
        return Err(invalid());
    }
    Ok((session, socket))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporter_accepts_only_the_supplied_flag_and_generated_socket_format() {
        let socket = minidumper_child::make_socket_name(Uuid::new_v4());
        assert_eq!(
            parse_argument(&format!("session-1={socket}")).unwrap(),
            ("session-1", socket.as_str()),
        );
        assert!(parse_argument(&format!("../outside={socket}")).is_err());
        assert!(parse_argument(&format!("session-1=other-{socket}")).is_err());
        assert!(validate_flag("--capture-native").is_ok());
        assert!(validate_flag("--capture-native=unsafe").is_err());
    }
}
