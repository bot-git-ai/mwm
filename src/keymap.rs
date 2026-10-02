//! Key chords and the chord-to-command map.
//!
//! A chord is written `alt-h`, `shift-cmd-left` or `ctrl-alt-4`; separators
//! may be `-` or `+`, and modifier sides (`cmd_l`, `alt_r`, ...) are
//! accepted and collapse. The default map mirrors the shortcuts mwm ships
//! with, and a JSON file can replace it entirely.

use std::path::{Path, PathBuf};

use crate::platform::KeyEvent;
use crate::request::Request;
use crate::types::{Modifier, ModifierSet};

/// A key plus the modifiers that must be held with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    /// Modifiers held during the press.
    pub modifiers: ModifierSet,
    /// Canonical key name (`h`, `left`, `space`, `vk:0x7b`).
    pub key: String,
}

impl KeyChord {
    /// Parse a chord such as `shift-cmd-left`; `None` when malformed.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let tokens: Vec<String> = value
            .replace('+', "-")
            .split('-')
            .map(|token| token.trim().to_lowercase())
            .filter(|token| !token.is_empty())
            .collect();
        let (key, modifier_tokens) = tokens.split_last()?;
        if key.is_empty() {
            return None;
        }
        let modifiers = modifier_tokens
            .iter()
            .map(|token| Modifier::parse(token))
            .collect::<Option<ModifierSet>>()?;
        Some(Self {
            modifiers,
            key: (*key).clone(),
        })
    }

    /// Whether this chord matches an observed key press exactly.
    #[must_use]
    pub fn matches(&self, event: &KeyEvent) -> bool {
        self.key == event.key.as_str() && self.modifiers == event.modifiers
    }
}

/// A chord bound to a command.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyBinding {
    /// The chord that triggers it.
    pub chord: KeyChord,
    /// The command it runs.
    pub request: Request,
}

/// The bindings mwm uses when no keybinding file is given.
#[must_use]
pub fn default_bindings() -> Vec<(&'static str, &'static str)> {
    vec![
        ("alt-h", "focus left"),
        ("alt-j", "focus down"),
        ("alt-k", "focus up"),
        ("alt-l", "focus right"),
        ("shift-alt-h", "move left"),
        ("shift-alt-j", "move down"),
        ("shift-alt-k", "move up"),
        ("shift-alt-l", "move right"),
        ("cmd-left", "focus left"),
        ("cmd-down", "focus down"),
        ("cmd-up", "focus up"),
        ("cmd-right", "focus right"),
        ("shift-cmd-left", "move left"),
        ("shift-cmd-down", "move down"),
        ("shift-cmd-up", "move up"),
        ("shift-cmd-right", "move right"),
        ("alt-1", "goto-desktop 1"),
        ("alt-2", "goto-desktop 2"),
        ("alt-3", "goto-desktop 3"),
        ("alt-4", "goto-desktop 4"),
        ("alt-5", "goto-desktop 5"),
        ("alt-6", "goto-desktop 6"),
        ("alt-7", "goto-desktop 7"),
        ("alt-8", "goto-desktop 8"),
        ("alt-9", "goto-desktop 9"),
        ("alt-0", "goto-desktop 10"),
        ("shift-alt-q", "close"),
        ("alt-f", "fullscreen"),
        ("alt-r", "retile"),
        ("shift-alt-r", "restart"),
        ("ctrl-alt-1", "columns 1"),
        ("ctrl-alt-2", "columns 2"),
        ("ctrl-alt-3", "columns 3"),
        ("ctrl-alt-4", "columns 2.5"),
        ("ctrl-alt-5", "columns 1.7"),
        ("alt-space", "status"),
    ]
}

/// Parse a flat `{"chord": "command"}` JSON object into bindings.
pub fn parse_binding_map(text: &str) -> Result<Vec<KeyBinding>, String> {
    let entries = parse_string_object(text)?;
    let mut bindings = Vec::with_capacity(entries.len());
    for (chord_text, command_text) in entries {
        let chord = KeyChord::parse(&chord_text)
            .ok_or_else(|| format!("invalid keybinding chord: {chord_text}"))?;
        let request = Request::parse_command(&command_text)
            .ok_or_else(|| format!("invalid command for chord {chord_text}: {command_text}"))?;
        bindings.push(KeyBinding { chord, request });
    }
    Ok(bindings)
}

/// Minimal JSON object reader accepting string keys and string values only.
fn parse_string_object(text: &str) -> Result<Vec<(String, String)>, String> {
    let input = text.trim();
    let body = input
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .ok_or_else(|| "expected a JSON object".to_string())?;
    let mut entries = Vec::new();
    let mut rest = body.trim();
    if rest.is_empty() {
        return Ok(entries);
    }
    loop {
        let (key, tail) = read_json_string(rest)?;
        let tail = tail
            .trim_start()
            .strip_prefix(':')
            .ok_or_else(|| format!("expected ':' after key {key}"))?;
        let (value, tail) = read_json_string(tail.trim_start())?;
        entries.push((key, value));
        rest = tail.trim_start();
        match rest.chars().next() {
            Some(',') => rest = rest[1..].trim_start(),
            Some('}') | None => break,
            Some(other) => return Err(format!("unexpected character '{other}' in keybinding map")),
        }
    }
    Ok(entries)
}

/// Read one JSON string literal, rejecting anything that is not a string.
fn read_json_string(input: &str) -> Result<(String, &str), String> {
    if !input.starts_with('"') {
        return Err("expected a quoted string in the keybinding map".to_string());
    }
    let mut out = String::new();
    let mut chars = input[1..].char_indices();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '"' => return Ok((out, &input[index + 2..])),
            '\\' => {
                let (_, escape) = chars
                    .next()
                    .ok_or_else(|| "unterminated escape".to_string())?;
                out.push(match escape {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '"' => '"',
                    '\\' => '\\',
                    other => return Err(format!("unsupported escape \\{other}")),
                });
            }
            other => out.push(other),
        }
    }
    Err("unterminated string in the keybinding map".to_string())
}

/// The command bound to an observed key press, if any.
#[must_use]
pub fn match_key<'a>(event: &KeyEvent, bindings: &'a [KeyBinding]) -> Option<&'a Request> {
    bindings
        .iter()
        .find(|binding| binding.chord.matches(event))
        .map(|binding| &binding.request)
}

/// Build the bindings the daemon should use: the file's contents when given,
/// otherwise the built-in defaults.
/// Where mwm looks for a keybindings file when none is given on the command
/// line: `$XDG_CONFIG_HOME/mwm/keybindings.json`, or `~/.config/mwm/keybindings.json`
/// when that variable is not set.
#[must_use]
pub fn default_keybindings_path() -> Option<PathBuf> {
    let base: PathBuf = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(std::env::var_os("HOME")?),
    };
    Some(base.join("mwm").join("keybindings.json"))
}

/// The bindings the daemon should use.
///
/// An explicit `path` is read as given. With no path, the file at
/// [`default_keybindings_path`] is used when it exists, and the built-in
/// defaults otherwise — a missing file is not an error, because most people
/// will never write one.
pub fn load_bindings(path: Option<&Path>) -> Result<Vec<KeyBinding>, String> {
    let chosen = match path {
        Some(path) => PathBuf::from(path),
        None => match default_keybindings_path() {
            Some(candidate) if candidate.is_file() => candidate,
            _ => return built_in_bindings(),
        },
    };
    let text = std::fs::read_to_string(&chosen)
        .map_err(|error| format!("cannot read keybindings {}: {error}", chosen.display()))?;
    parse_binding_map(&text)
}

/// The bindings mwm ships with, parsed.
fn built_in_bindings() -> Result<Vec<KeyBinding>, String> {
    let pairs = default_bindings();
    let mut bindings = Vec::with_capacity(pairs.len());
    for (chord, command) in pairs {
        bindings.push(KeyBinding {
            chord: KeyChord::parse(chord)
                .ok_or_else(|| format!("invalid default chord: {chord}"))?,
            request: Request::parse_command(command)
                .ok_or_else(|| format!("invalid default command: {command}"))?,
        });
    }
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::{default_bindings, load_bindings, match_key, parse_binding_map, KeyChord};
    use crate::platform::{KeyEvent, KeyName};
    use crate::request::Request;
    use crate::types::Direction;
    use crate::types::{Modifier, ModifierSet};
    use std::collections::BTreeSet;

    fn mods(names: &[Modifier]) -> ModifierSet {
        names.iter().copied().collect()
    }

    fn event(key: KeyName, modifiers: &[Modifier]) -> KeyEvent {
        KeyEvent {
            modifiers: mods(modifiers),
            key,
        }
    }

    #[test]
    fn parses_plain_chord() {
        let chord = KeyChord::parse("alt-h").expect("parses");
        assert_eq!(chord.key, "h");
        assert_eq!(chord.modifiers, mods(&[Modifier::Alt]));
    }

    #[test]
    fn accepts_plus_separator_and_order() {
        let chord = KeyChord::parse("shift+cmd+left").expect("parses");
        assert_eq!(chord.key, "left");
        assert_eq!(chord.modifiers, mods(&[Modifier::Cmd, Modifier::Shift]));
    }

    #[test]
    fn collapses_duplicate_and_sided_modifiers() {
        let chord = KeyChord::parse("alt_l-alt-ALT-h").expect("parses");
        assert_eq!(chord.modifiers, mods(&[Modifier::Alt]));
    }

    #[test]
    fn rejects_bad_chords() {
        for bad in ["", "   ", "-", "hyper-h"] {
            assert!(KeyChord::parse(bad).is_none(), "{bad}");
        }
        // the last token is always the key, so an unknown trailing word is a
        // key name rather than an error
        assert_eq!(
            KeyChord::parse("alt-hyper").map(|c| c.key),
            Some("hyper".to_string())
        );
        // a trailing separator is tolerated: "alt-" is just "alt"
        assert_eq!(
            KeyChord::parse("alt-").map(|c| c.key),
            Some("alt".to_string())
        );
    }

    #[test]
    fn keeps_virtual_key_codes_literal() {
        let chord = KeyChord::parse("vk:0x7b").expect("parses");
        assert_eq!(chord.key, "vk:0x7b");
        assert_eq!(chord.modifiers.len(), 0);
    }

    #[test]
    fn every_default_binding_parses() {
        for (chord, command) in default_bindings() {
            assert!(KeyChord::parse(chord).is_some(), "chord {chord}");
            assert!(
                Request::parse_command(command).is_some(),
                "command {command}"
            );
        }
    }

    #[test]
    fn default_bindings_load() {
        let bindings = load_bindings(None).expect("defaults load");
        assert_eq!(bindings.len(), default_bindings().len());
    }

    #[test]
    fn parses_a_binding_map() {
        let bindings = parse_binding_map(r#"{"alt-h": "focus left", "ctrl-alt-4": "columns 2.5"}"#)
            .expect("parses");
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].request, Request::Focus(Direction::Left));
        assert_eq!(bindings[1].request, Request::Columns(2.5));
    }

    #[test]
    fn rejects_malformed_binding_maps() {
        for bad in [
            "not json",
            "[1,2]",
            r#"{"alt-h": 5}"#,
            r#"{"alt-h": {"nested": "focus left"}}"#,
            r#"{"hyper-h": "focus left"}"#,
            r#"{"alt-h": "warp 9"}"#,
            r#"{"alt-h" "focus left"}"#,
        ] {
            assert!(parse_binding_map(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn match_key_requires_exact_modifiers() {
        let bindings = load_bindings(None).expect("defaults load");
        let hit = match_key(&event(KeyName::Letter('h'), &[Modifier::Alt]), &bindings);
        assert_eq!(hit, Some(&Request::Focus(Direction::Left)));
        // alt+shift+h is bound (shift-alt-h = move left), but cmd+shift+h is not
        assert_eq!(
            match_key(
                &event(KeyName::Letter('h'), &[Modifier::Alt, Modifier::Shift]),
                &bindings
            ),
            Some(&Request::Move(Direction::Left))
        );
        let extra = match_key(
            &event(
                KeyName::Letter('h'),
                &[
                    Modifier::Cmd,
                    Modifier::Shift,
                    Modifier::Alt,
                    Modifier::Ctrl,
                ],
            ),
            &bindings,
        );
        assert_eq!(extra, None);
        // wrong direction chord
        assert_eq!(
            match_key(&event(KeyName::Letter('h'), &[Modifier::Cmd]), &bindings),
            None
        );
        // unbound key
        assert_eq!(
            match_key(&event(KeyName::Letter('z'), &[Modifier::Alt]), &bindings),
            None
        );
    }

    #[test]
    fn match_key_handles_arrows_and_digits() {
        let bindings = load_bindings(None).expect("defaults load");
        assert_eq!(
            match_key(
                &event(KeyName::Arrow(Direction::Right), &[Modifier::Cmd]),
                &bindings
            ),
            Some(&Request::Focus(Direction::Right))
        );
        assert_eq!(
            match_key(&event(KeyName::Digit(0), &[Modifier::Alt]), &bindings),
            Some(&Request::GotoDesktop(10))
        );
        assert_eq!(
            match_key(
                &event(KeyName::Digit(4), &[Modifier::Alt, Modifier::Ctrl]),
                &bindings
            ),
            Some(&Request::Columns(2.5))
        );
    }

    #[test]
    fn config_path_follows_xdg_config_home() {
        // The path is derived from the environment, so assert the shape rather
        // than an absolute value: XDG_CONFIG_HOME when set, HOME otherwise.
        let path = super::default_keybindings_path().expect("a home directory exists");
        assert_eq!(
            path.file_name(),
            Some(std::ffi::OsStr::new("keybindings.json"))
        );
        assert_eq!(
            path.parent().and_then(std::path::Path::file_name),
            Some(std::ffi::OsStr::new("mwm"))
        );
        let expected_base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from));
        assert_eq!(
            path.parent().and_then(std::path::Path::parent),
            expected_base.as_deref()
        );
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let missing = std::env::temp_dir().join("mwm-no-such-keybindings.json");
        let _ = std::fs::remove_file(&missing);
        // An explicit path that does not exist is still an error: the user
        // asked for that file.
        assert!(load_bindings(Some(missing.as_path())).is_err());
        // With no path, a missing file simply means the defaults.
        let bindings = load_bindings(None).expect("defaults load");
        assert_eq!(bindings.len(), default_bindings().len());
    }

    #[test]
    fn an_explicit_file_is_used() {
        let path =
            std::env::temp_dir().join(format!("mwm-keybindings-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"alt-h": "focus left", "shift-alt-q": "close"}"#).expect("write");
        let bindings = load_bindings(Some(path.as_path())).expect("parses");
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].request, Request::Focus(Direction::Left));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_set_is_not_the_same_as_missing() {
        let bindings = parse_binding_map(r#"{"q": "close"}"#).expect("parses");
        assert!(match_key(&event(KeyName::Letter('q'), &[]), &bindings).is_some());
        assert_eq!(BTreeSet::from([Modifier::Shift]).len(), 1);
    }
}
