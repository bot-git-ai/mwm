//! The request/response protocol spoken over the Unix socket.
//!
//! One canonical representation shared by the CLI, the keybinding map and
//! the daemon: a command string (`focus left`, `columns 2.5`) is parsed into
//! a [`Request`], and a request serialises to one line of JSON on the wire.

use crate::types::Direction;

/// A command the daemon understands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Request {
    /// Move keyboard focus.
    Focus(Direction),
    /// Move the focused window within the layout.
    Move(Direction),
    /// Switch to another desktop (Space).
    GotoDesktop(u8),
    /// Set how many columns to keep, possibly fractional (2.5).
    Columns(f64),
    /// Toggle fullscreen for the focused window.
    Fullscreen,
    /// Close the focused window.
    Close,
    /// Re-apply the layout now.
    Retile,
    /// Report a one-line status summary.
    Status,
    /// Stop the daemon.
    Stop,
    /// Restart the daemon.
    Restart,
}

impl Request {
    /// The canonical command string, as used by the CLI and keybindings.
    #[must_use]
    pub fn command(&self) -> String {
        match self {
            Self::Focus(direction) => format!("focus {direction}"),
            Self::Move(direction) => format!("move {direction}"),
            Self::GotoDesktop(desktop) => format!("goto-desktop {desktop}"),
            Self::Columns(columns) => format!("columns {columns}"),
            Self::Fullscreen => "fullscreen".to_string(),
            Self::Close => "close".to_string(),
            Self::Retile => "retile".to_string(),
            Self::Status => "status".to_string(),
            Self::Stop => "stop".to_string(),
            Self::Restart => "restart".to_string(),
        }
    }

    /// Parse a canonical command string; `None` for anything else.
    #[must_use]
    pub fn parse_command(value: &str) -> Option<Self> {
        let mut words = value.split_whitespace();
        let head = words.next()?;
        let mut argument = || {
            let next = words.next();
            if words.next().is_some() {
                return None;
            }
            next
        };
        match head {
            "focus" => Some(Self::Focus(Direction::parse(argument()?)?)),
            "move" => Some(Self::Move(Direction::parse(argument()?)?)),
            "goto-desktop" => Some(Self::GotoDesktop(argument()?.parse().ok()?)),
            "columns" => Some(Self::Columns(argument()?.parse().ok()?)),
            "fullscreen" if argument().is_none() => Some(Self::Fullscreen),
            "close" if argument().is_none() => Some(Self::Close),
            "retile" if argument().is_none() => Some(Self::Retile),
            "status" if argument().is_none() => Some(Self::Status),
            "stop" if argument().is_none() => Some(Self::Stop),
            "restart" if argument().is_none() => Some(Self::Restart),
            _ => None,
        }
    }
}

/// The daemon's answer, one line of JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcResponse {
    /// Whether the command succeeded.
    pub ok: bool,
    /// Human-readable outcome.
    pub message: String,
}

impl IpcResponse {
    /// A successful response.
    #[must_use]
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
        }
    }

    /// A failed response.
    #[must_use]
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
        }
    }
}

/// Escape a Rust string as a JSON string literal, quotes included.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parse a JSON string literal starting at `input[0]`; returns the decoded
/// string and the rest of the input after the closing quote.
fn parse_json_string(input: &str) -> Option<(String, &str)> {
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if first != '"' {
        return None;
    }
    let mut out = String::new();
    let mut rest = &input[first.len_utf8()..];
    loop {
        let ch = rest.chars().next()?;
        let width = ch.len_utf8();
        if ch == '"' {
            return Some((out, &rest[width..]));
        }
        if ch != '\\' {
            let next = &rest[width..];
            out.push(ch);
            rest = next;
            continue;
        }
        let after = &rest[width..];
        let (escape, tail) = after.split_at(after.chars().next()?.len_utf8());
        match escape {
            "n" => out.push('\n'),
            "r" => out.push('\r'),
            "t" => out.push('\t'),
            "b" => out.push('\u{8}'),
            "f" => out.push('\u{c}'),
            "\"" => out.push('"'),
            "\\" => out.push('\\'),
            "/" => out.push('/'),
            "u" => {
                let hex = tail.get(..4)?;
                let code = u32::from_str_radix(hex, 16).ok()?;
                out.push(char::from_u32(code)?);
                rest = &tail[4..];
                continue;
            }
            _ => return None,
        }
        rest = tail;
    }
}

impl IpcResponse {
    /// Serialise to one line of JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(
            "{{\"ok\":{},\"message\":{}}}",
            self.ok,
            json_string(&self.message)
        )
    }

    /// Parse a response, rejecting anything that is not exactly one object.
    #[must_use]
    pub fn from_json(payload: &str) -> Option<Self> {
        let input = payload.trim();
        let rest = input.strip_prefix("{\"ok\":")?;
        let (ok, rest) = match rest.strip_prefix("true,") {
            Some(rest) => (true, rest),
            None => (false, rest.strip_prefix("false,")?),
        };
        let rest = rest.strip_prefix("\"message\":")?;
        let (message, rest) = parse_json_string(rest)?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('}')?;
        rest.trim().is_empty().then_some(Self { ok, message })
    }
}

impl Request {
    /// Serialise to one line of JSON.
    #[must_use]
    pub fn to_json(self) -> String {
        match self {
            Self::Focus(direction) => {
                format!("{{\"kind\":\"focus\",\"direction\":\"{direction}\"}}")
            }
            Self::Move(direction) => format!("{{\"kind\":\"move\",\"direction\":\"{direction}\"}}"),
            Self::GotoDesktop(desktop) => {
                format!("{{\"kind\":\"goto-desktop\",\"desktop\":{desktop}}}")
            }
            Self::Columns(columns) => format!("{{\"kind\":\"columns\",\"columns\":{columns}}}"),
            Self::Fullscreen => "{\"kind\":\"fullscreen\"}".to_string(),
            Self::Close => "{\"kind\":\"close\"}".to_string(),
            Self::Retile => "{\"kind\":\"retile\"}".to_string(),
            Self::Status => "{\"kind\":\"status\"}".to_string(),
            Self::Stop => "{\"kind\":\"stop\"}".to_string(),
            Self::Restart => "{\"kind\":\"restart\"}".to_string(),
        }
    }

    /// Parse a request; `None` for unknown or malformed payloads.
    #[must_use]
    pub fn from_json(payload: &str) -> Option<Self> {
        let input = payload.trim();
        let rest = input.strip_prefix("{\"kind\":")?;
        let (kind, rest) = parse_json_string(rest)?;
        let mut rest = rest;
        let mut direction = None;
        let mut desktop = None;
        let mut columns = None;
        // Fields follow as `,"name":value` until the closing brace.
        while let Some(tail) = rest.strip_prefix(',') {
            let (key, tail) = parse_json_string(tail)?;
            let tail = tail.strip_prefix(':')?;
            match key.as_str() {
                "direction" => {
                    let (value, tail) = parse_json_string(tail)?;
                    direction = Direction::parse(&value);
                    rest = tail;
                }
                "desktop" | "columns" => {
                    let end = tail
                        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
                        .unwrap_or(tail.len());
                    let (digits, tail) = tail.split_at(end);
                    if key == "desktop" {
                        desktop = digits.parse().ok();
                    } else {
                        columns = digits.parse().ok();
                    }
                    rest = tail;
                }
                _ => return None,
            }
        }
        let rest = rest.strip_prefix('}')?;
        if !rest.trim().is_empty() {
            return None;
        }
        match kind.as_str() {
            "focus" => Some(Self::Focus(direction?)),
            "move" => Some(Self::Move(direction?)),
            "goto-desktop" => Some(Self::GotoDesktop(desktop?)),
            "columns" => Some(Self::Columns(columns?)),
            "fullscreen" => Some(Self::Fullscreen),
            "close" => Some(Self::Close),
            "retile" => Some(Self::Retile),
            "status" => Some(Self::Status),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IpcResponse, Request};
    use crate::types::Direction;

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Focus(Direction::Left),
            Request::Focus(Direction::Down),
            Request::Move(Direction::Right),
            Request::Move(Direction::Up),
            Request::GotoDesktop(1),
            Request::GotoDesktop(10),
            Request::Columns(2.5),
            Request::Columns(1.0),
            Request::Fullscreen,
            Request::Close,
            Request::Retile,
            Request::Status,
            Request::Stop,
            Request::Restart,
        ]
    }

    #[test]
    fn command_round_trips() {
        for request in all_requests() {
            let text = request.command();
            assert_eq!(Request::parse_command(&text), Some(request), "{text}");
        }
    }

    #[test]
    fn command_strings_are_canonical() {
        assert_eq!(Request::Focus(Direction::Left).command(), "focus left");
        assert_eq!(Request::GotoDesktop(3).command(), "goto-desktop 3");
        assert_eq!(Request::Columns(2.5).command(), "columns 2.5");
        assert_eq!(Request::Columns(1.0).command(), "columns 1");
        assert_eq!(Request::Restart.command(), "restart");
    }

    #[test]
    fn parse_command_rejects_bad_input() {
        for bad in [
            "",
            "focus",
            "focus left extra",
            "focus nowhere",
            "columns",
            "columns x",
            "goto-desktop 99x",
            "fullscreen now",
            "warp 9",
            "columns 2 3",
        ] {
            assert_eq!(Request::parse_command(bad), None, "{bad}");
        }
    }

    #[test]
    fn json_round_trips() {
        for request in all_requests() {
            let text = request.to_json();
            assert!(!text.contains('\n'));
            assert_eq!(Request::from_json(&text), Some(request), "{text}");
        }
    }

    #[test]
    fn response_round_trips() {
        for response in [
            IpcResponse::ok("retiled"),
            IpcResponse::err("no target window"),
        ] {
            let text = response.to_json();
            assert!(!text.contains('\n'));
            assert_eq!(IpcResponse::from_json(&text), Some(response));
        }
    }

    #[test]
    fn response_escapes_awkward_messages() {
        for message in [
            "quote \" backslash \\ newline \n tab \t",
            "unicode → ok",
            "control \u{1}",
        ] {
            let response = IpcResponse::ok(message);
            assert_eq!(IpcResponse::from_json(&response.to_json()), Some(response));
        }
    }

    #[test]
    fn json_parsing_is_strict() {
        for bad in [
            "",
            "{}",
            "{\"ok\":maybe,\"message\":\"x\"}",
            "{\"ok\":true,\"message\":5}",
            "{\"ok\":true,\"message\":\"x\"} trailing",
            "not json",
        ] {
            assert_eq!(IpcResponse::from_json(bad), None, "{bad}");
        }
        for bad in [
            "",
            "{\"kind\":\"warp\"}",
            "{\"kind\":\"focus\"}",
            "{\"kind\":\"focus\",\"direction\":\"sideways\"}",
            "{\"kind\":\"columns\",\"columns\":\"lots\"}",
            "{\"kind\":\"focus\",\"direction\":\"left\"} x",
        ] {
            assert_eq!(Request::from_json(bad), None, "{bad}");
        }
    }

    #[test]
    fn unicode_escape_decodes() {
        let response = IpcResponse::from_json(r#"{"ok":true,"message":"a\u0020b"}"#);
        assert_eq!(response, Some(IpcResponse::ok("a b")));
        assert_eq!(
            IpcResponse::from_json(r#"{"ok":true,"message":"\u0041"}"#),
            Some(IpcResponse::ok("A"))
        );
    }
}
