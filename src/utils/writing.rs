use std::{
    fs::OpenOptions,
    io::{self, StdoutLock, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Debug, Default)]
struct LogSinkState {
    mode: LogSinkMode,
}

#[derive(Debug, Default)]
enum LogSinkMode {
    #[default]
    Stderr,
    File {
        path: PathBuf,
        file: std::fs::File,
    },
    Muted,
}

static LOG_SINK: OnceLock<Mutex<LogSinkState>> = OnceLock::new();

fn log_sink() -> &'static Mutex<LogSinkState> {
    LOG_SINK.get_or_init(|| Mutex::new(LogSinkState::default()))
}

pub fn set_log_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut sink = log_sink()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sink.mode = LogSinkMode::File {
        path: path.to_path_buf(),
        file,
    };
    Ok(())
}

pub fn mute_logs() {
    let mut sink = log_sink()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sink.mode = LogSinkMode::Muted;
}

pub fn restore_stderr_logging() {
    let mut sink = log_sink()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sink.mode = LogSinkMode::Stderr;
}

pub fn log_file_path() -> Option<PathBuf> {
    let sink = log_sink()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match &sink.mode {
        LogSinkMode::File { path, .. } => Some(path.clone()),
        _ => None,
    }
}

fn strip_ansi_codes(input: &str) -> std::borrow::Cow<'_, str> {
    if !input.contains('\x1b') {
        return std::borrow::Cow::Borrowed(input);
    }

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }

        // Skip ANSI escape sequence: ESC [ ... m
        if chars.peek() == Some(&'[') {
            let _ = chars.next();
            for inner in chars.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        }
    }

    std::borrow::Cow::Owned(out)
}

fn redact_url_queries(input: &str) -> String {
    const SCHEMES: [&str; 4] = ["http://", "https://", "ws://", "wss://"];

    let mut cursor = 0usize;
    let mut out = String::with_capacity(input.len());

    while cursor < input.len() {
        let mut next: Option<(usize, &str)> = None;
        for scheme in SCHEMES {
            if let Some(rel) = input[cursor..].find(scheme) {
                let abs = cursor + rel;
                if next.is_none_or(|(best, _)| abs < best) {
                    next = Some((abs, scheme));
                }
            }
        }

        let Some((start, _scheme)) = next else {
            out.push_str(&input[cursor..]);
            break;
        };

        out.push_str(&input[cursor..start]);

        let mut end = start;
        while end < input.len() && !input.as_bytes()[end].is_ascii_whitespace() {
            end += 1;
        }

        let segment = &input[start..end];
        if let Some(qpos) = segment.find('?') {
            out.push_str(&segment[..qpos]);
            out.push_str("?<redacted>");
        } else {
            out.push_str(segment);
        }

        cursor = end;
    }

    out
}

pub fn emit_log_line(line: &str) {
    let mut sink = log_sink()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    match &mut sink.mode {
        LogSinkMode::Stderr => {
            let mut stderr = ::std::io::stderr().lock();
            let _ = stderr.write_all(line.as_bytes());
        }
        LogSinkMode::File { file, .. } => {
            let stripped = strip_ansi_codes(line);
            let redacted = redact_url_queries(&stripped);
            let _ = file.write_all(redacted.as_bytes());
        }
        LogSinkMode::Muted => {}
    }
}

#[macro_export]
macro_rules! log {
    // colored, no extra args
    ($color:expr, $fmt:literal) => {{
        let time = chrono::Utc::now().format("%H:%M:%S%.3f").to_string();
        let line = format!(
            concat!("{}{} | {}", "{}", $fmt, "{}", "\n"),
            $crate::utils::writing::cc::LIGHT_GRAY,
            time,
            $crate::utils::writing::cc::RESET,
            $color,
            $crate::utils::writing::cc::RESET
        );
        $crate::utils::writing::emit_log_line(&line);
    }};

    // colored, with args
    ($color:expr, $fmt:literal, $($arg:tt)+) => {{
        let time = chrono::Utc::now().format("%H:%M:%S%.3f").to_string();
        let line = format!(
            concat!("{}{} | {}", "{}", $fmt, "{}", "\n"),
            $crate::utils::writing::cc::LIGHT_GRAY,
            time,
            $crate::utils::writing::cc::RESET,
            $color,
            $($arg)+,
            $crate::utils::writing::cc::RESET
        );
        $crate::utils::writing::emit_log_line(&line);
    }};

    // default color, no extra args
    ($fmt:literal) => {{
        let time = chrono::Utc::now().format("%H:%M:%S%.3f").to_string();
        let line = format!(
            concat!("{}{} | {}", "{}", $fmt, "{}", "\n"),
            $crate::utils::writing::cc::LIGHT_GRAY,
            time,
            $crate::utils::writing::cc::RESET,
            $crate::utils::writing::cc::LIGHT_GRAY,
            $crate::utils::writing::cc::RESET
        );
        $crate::utils::writing::emit_log_line(&line);
    }};

    // default color, with args
    ($fmt:literal, $($arg:tt)+) => {{
        let time = chrono::Utc::now().format("%H:%M:%S%.3f").to_string();
        let line = format!(
            concat!("{}{} | {}", "{}", $fmt, "{}", "\n"),
            $crate::utils::writing::cc::LIGHT_GRAY,
            time,
            $crate::utils::writing::cc::RESET,
            $crate::utils::writing::cc::LIGHT_GRAY,
            $($arg)+,
            $crate::utils::writing::cc::RESET
        );
        $crate::utils::writing::emit_log_line(&line);
    }};
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {{
        let line = format!(
            "{}{}{}\n",
            $crate::utils::writing::cc::ORANGE,
            format_args!($($arg)*),
            $crate::utils::writing::cc::RESET
        );
        $crate::utils::writing::emit_log_line(&line);
    }};
}

pub struct Colors<'a> {
    lock: StdoutLock<'a>,
}

pub mod cc {
    pub const RED: &str = "\x1b[31m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const BLUE: &str = "\x1b[34m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const CYAN: &str = "\x1b[36m";
    pub const WHITE: &str = "\x1b[37m";
    pub const BOLD: &str = "\x1b[1m";
    pub const RESET: &str = "\x1b[0m";
    pub const BLINK: &str = "\x1b[5m";
    pub const BLACK: &str = "\x1b[30m";
    pub const ORANGE: &str = "\x1b[38;5;208m";
    pub const PURPLE: &str = "\x1b[38;5;93m";
    pub const DARK_GRAY: &str = "\x1b[38;5;238m";
    pub const LIGHT_GRAY: &str = "\x1b[38;5;245m";
    pub const PINK: &str = "\x1b[38;5;213m";
    pub const BROWN: &str = "\x1b[38;5;130m";
    pub const LIGHT_GREEN: &str = "\x1b[92m";
    pub const LIGHT_BLUE: &str = "\x1b[94m";
    pub const LIGHT_CYAN: &str = "\x1b[96m";
    pub const LIGHT_RED: &str = "\x1b[91m";
    pub const LIGHT_MAGENTA: &str = "\x1b[95m";
    pub const LIGHT_YELLOW: &str = "\x1b[93m";
    pub const LIGHT_WHITE: &str = "\x1b[97m";
}

impl<'a> Colors<'a> {
    pub fn new(lock: StdoutLock<'a>) -> Self {
        Self { lock }
    }

    pub fn cprint(&mut self, text: &str, color: &str) {
        let _ = writeln!(self.lock, "{}{}{}", color, text, cc::RESET);
    }

    pub fn cinput(&mut self, text: &str, color: &str) -> String {
        let mut input = String::new();
        let _ = writeln!(self.lock, "{}{}{}", color, text, cc::RESET);
        if io::stdin().read_line(&mut input).is_err() {
            return String::new();
        }
        input.trim().to_string()
    }

    pub fn err_print(&mut self, text: &str) {
        let _ = writeln!(self.lock, "{}{}{}", cc::RED, text, cc::RESET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cprint() {
        let lock: StdoutLock<'static> = io::stdout().lock();
        let mut colors: Colors<'static> = Colors::new(lock);
        colors.cprint(&format!("{}Hello, world!", cc::BOLD), cc::RED);
        colors.cprint("This is your captain speaking!", cc::BLUE);
    }

    #[test]
    fn test_err_print() {
        let lock: StdoutLock<'static> = io::stdout().lock();
        let mut colors: Colors<'static> = Colors::new(lock);
        colors.err_print("This is an error message!");
    }

    // #[test]
    // fn test_cinput() {
    //     let lock: StdoutLock<'static> = io::stdout().lock();
    //     let mut colors: Colors<'static> = Colors::new(lock);
    //     let input: String = colors.cinput("Enter your name: ", cc::BLUE);
    //     colors.cprint(&format!("Hello, {}!", input), cc::GREEN);
    // }
}
