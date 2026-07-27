//! The sequences the terminal library parses but does not hand back.
//!
//! Upstream's terminal dispatches a callback for a title, a working directory
//! and a clipboard write, and swallows the rest. Its standalone OSC parser
//! knows the shapes of the others — `SHOW_DESKTOP_NOTIFICATION`,
//! `CONEMU_PROGRESS_REPORT` — but the only payload it will hand out at this
//! pin is a window title, so knowing the shape buys nothing.
//!
//! So this reads the stream alongside the parser. It watches for the
//! sequences a terminal is expected to act on and nobody else can see:
//!
//! - `OSC 9 ; text` — a desktop notification, iTerm2's spelling
//! - `OSC 777 ; notify ; title ; body` — rxvt's, which carries a title
//! - `OSC 99 ; metadata ; payload` — kitty's, which arrives in chunks
//! - `OSC 9 ; 4 ; state ; percent` — ConEmu's progress report
//! - `CSI > Ps s`: XTSHIFTESCAPE, an application asking to see Shift on
//!   mouse events, which upstream records in a flag the C API never shows
//! - `ESC c`: a full reset, which takes that request back
//!
//! Everything else is skipped, including the bodies of the string sequences
//! (DCS, APC, PM, SOS) so that a `tmux` passthrough carrying an OSC cannot
//! fire a notification the multiplexer meant to keep to itself.

use std::collections::HashMap;

/// A notification an application asked the desktop to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Empty for `OSC 9`, which carries no title of its own.
    pub title: String,
    pub body: String,
}

/// A progress report, in ConEmu's spelling, which is what shells and package
/// managers emit on this desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Nothing is running any more.
    Remove,
    /// A percentage, 0 to 100.
    Set(u8),
    /// Something failed, with the percentage it failed at when it gave one.
    Error(Option<u8>),
    /// Something is running and cannot say how far along it is.
    Indeterminate,
    /// Waiting on the user.
    Pause(Option<u8>),
}

/// What one sequence turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Notify(Notification),
    Progress(Progress),
    /// XTSHIFTESCAPE: whether the application asked for Shift on mouse events.
    ShiftCapture(bool),
    /// `ESC c`, which resets the terminal and the request above with it.
    Reset,
}

/// Where the scan is in the stream. PTY output arrives in arbitrary chunks, so
/// a sequence split across two reads has to survive the boundary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// An `ESC` went by; the next byte says what kind of sequence it opens.
    Escape,
    /// Inside `OSC …`, collecting the payload.
    Osc,
    /// Inside `CSI …`, collecting parameter and intermediate bytes.
    Csi,
    /// An `ESC` inside an OSC payload: `ESC \` ends it, anything else means
    /// the sequence was abandoned mid-way.
    OscEscape,
    /// Inside a DCS, APC, PM or SOS string, whose payload is none of our
    /// business and must not be scanned for OSCs.
    Skip,
    /// An `ESC` inside one of those.
    SkipEscape,
}

/// A payload longer than this is not a notification anybody meant to read, and
/// an unterminated one would otherwise grow without limit on hostile output.
const MAX_PAYLOAD: usize = 8 * 1024;
/// Kitty's protocol lets a notification arrive in pieces. Hold a few, so that
/// a stream which never finishes one cannot pin memory.
const MAX_PENDING: usize = 8;

/// A kitty notification being assembled from chunks.
#[derive(Debug, Default)]
struct Pending {
    title: String,
    body: String,
}

/// Reads the byte stream for the sequences upstream does not report.
#[derive(Debug, Default)]
pub(crate) struct Sniffer {
    state: State,
    payload: Vec<u8>,
    /// Set when a payload ran past [`MAX_PAYLOAD`]: the rest of it is still
    /// skipped, but nothing is reported for it.
    overflow: bool,
    pending: HashMap<String, Pending>,
}

impl Sniffer {
    /// Scans one chunk of PTY output, appending whatever it recognises.
    pub(crate) fn feed(&mut self, data: &[u8], out: &mut Vec<Event>) {
        for &byte in data {
            self.step(byte, out);
        }
    }

    fn step(&mut self, byte: u8, out: &mut Vec<Event>) {
        match self.state {
            State::Ground => {
                if byte == 0x1b {
                    self.state = State::Escape;
                }
            }
            State::Escape => match byte {
                b']' => {
                    self.payload.clear();
                    self.overflow = false;
                    self.state = State::Osc;
                }
                b'[' => {
                    self.payload.clear();
                    self.overflow = false;
                    self.state = State::Csi;
                }
                // DCS, SOS, PM, APC: a string whose contents are not ours.
                b'P' | b'X' | b'^' | b'_' => self.state = State::Skip,
                // RIS. Upstream resets the whole terminal on it, including the
                // XTSHIFTESCAPE flag only this scan remembers.
                b'c' => {
                    out.push(Event::Reset);
                    self.state = State::Ground;
                }
                // Another `ESC` starts over; anything else is a short escape
                // sequence, and a CSI cannot contain an `ESC` to confuse us.
                0x1b => self.state = State::Escape,
                _ => self.state = State::Ground,
            },
            State::Osc => match byte {
                0x07 => {
                    self.finish(out);
                    self.state = State::Ground;
                }
                0x1b => self.state = State::OscEscape,
                _ => {
                    if self.payload.len() < MAX_PAYLOAD {
                        self.payload.push(byte);
                    } else {
                        self.overflow = true;
                    }
                }
            },
            State::OscEscape => {
                if byte == b'\\' {
                    self.finish(out);
                    self.state = State::Ground;
                } else {
                    // The sequence was cut short by another one. Drop what was
                    // collected and read the new one from its second byte.
                    self.payload.clear();
                    self.state = State::Ground;
                    self.step(0x1b, out);
                    self.step(byte, out);
                }
            }
            State::Csi => match byte {
                // Parameter and intermediate bytes accumulate; the final byte
                // names the control and ends the sequence.
                0x20..=0x3f => {
                    if self.payload.len() < MAX_PAYLOAD {
                        self.payload.push(byte);
                    } else {
                        self.overflow = true;
                    }
                }
                0x40..=0x7e => {
                    self.finish_csi(byte, out);
                    self.state = State::Ground;
                }
                // CAN and SUB abort a control sequence; ESC starts a new one.
                0x18 | 0x1a => self.state = State::Ground,
                0x1b => self.state = State::Escape,
                // Any other C0 control executes and the sequence carries on,
                // which is what the parser this scan shadows does with them.
                _ => {}
            },
            State::Skip => {
                if byte == 0x1b {
                    self.state = State::SkipEscape;
                }
            }
            State::SkipEscape => {
                self.state = if byte == b'\\' {
                    State::Ground
                } else {
                    State::Skip
                };
            }
        }
    }

    /// A CSI ended. The only one read is XTSHIFTESCAPE, `CSI > Ps s`: `Ps`
    /// absent or `0` means Shift stays the user's, `1` asks to see it.
    /// Ghostty accepts exactly these and logs anything else, so anything else
    /// is dropped here too.
    fn finish_csi(&mut self, final_byte: u8, out: &mut Vec<Event>) {
        let payload = std::mem::take(&mut self.payload);
        if std::mem::take(&mut self.overflow) || final_byte != b's' {
            return;
        }
        match payload.as_slice() {
            b">" | b">0" => out.push(Event::ShiftCapture(false)),
            b">1" => out.push(Event::ShiftCapture(true)),
            _ => {}
        }
    }

    fn finish(&mut self, out: &mut Vec<Event>) {
        let payload = std::mem::take(&mut self.payload);
        if self.overflow {
            self.overflow = false;
            return;
        }
        let text = String::from_utf8_lossy(&payload);
        if let Some(event) = self.parse(&text) {
            out.push(event);
        }
    }

    fn parse(&mut self, payload: &str) -> Option<Event> {
        let (code, rest) = payload.split_once(';')?;
        match code {
            "9" => {
                // ConEmu hung its progress reports off the same number as
                // iTerm2's notifications, so the first field decides which
                // this is. Ghostty resolves it the same way.
                if rest == "4" {
                    return Some(Event::Progress(Progress::Remove));
                }
                if let Some(fields) = rest.strip_prefix("4;") {
                    return progress(fields).map(Event::Progress);
                }
                (!rest.is_empty()).then(|| {
                    Event::Notify(Notification {
                        title: String::new(),
                        body: rest.to_owned(),
                    })
                })
            }
            "777" => {
                let body = rest.strip_prefix("notify;")?;
                // The body is whatever is left, semicolons and all: only the
                // title is delimited.
                let (title, body) = body.split_once(';').unwrap_or((body, ""));
                (!title.is_empty() || !body.is_empty()).then(|| {
                    Event::Notify(Notification {
                        title: title.to_owned(),
                        body: body.to_owned(),
                    })
                })
            }
            "99" => self.kitty(rest),
            _ => None,
        }
    }

    /// Kitty's protocol: `OSC 99 ; key=value : key=value ; payload`. A
    /// notification may arrive in several of these, held together by `i=` and
    /// closed by `d=1`, which is also the default.
    fn kitty(&mut self, rest: &str) -> Option<Event> {
        let (metadata, payload) = rest.split_once(';').unwrap_or((rest, ""));
        let mut id = String::new();
        let mut done = true;
        let mut is_body = false;
        let mut encoded = false;
        for field in metadata.split(':') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key {
                "i" => id = value.to_owned(),
                "d" => done = value != "0",
                "p" => is_body = value == "body",
                "e" => encoded = value == "1",
                _ => {}
            }
        }

        let text = if encoded {
            base64(payload)?
        } else {
            payload.to_owned()
        };

        // A finished notification that was never chunked never touches the
        // table, which is the common case.
        if done && !self.pending.contains_key(&id) {
            let (title, body) = if is_body {
                (String::new(), text)
            } else {
                (text, String::new())
            };
            return (!title.is_empty() || !body.is_empty())
                .then_some(Event::Notify(Notification { title, body }));
        }

        if !self.pending.contains_key(&id) && self.pending.len() >= MAX_PENDING {
            return None;
        }
        let held = self.pending.entry(id.clone()).or_default();
        let field = if is_body {
            &mut held.body
        } else {
            &mut held.title
        };
        if field.len() + text.len() <= MAX_PAYLOAD {
            field.push_str(&text);
        }
        if !done {
            return None;
        }
        let held = self.pending.remove(&id)?;
        (!held.title.is_empty() || !held.body.is_empty()).then_some(Event::Notify(Notification {
            title: held.title,
            body: held.body,
        }))
    }
}

/// `state ; percent`, where the percentage is optional for every state that
/// does not need one.
fn progress(fields: &str) -> Option<Progress> {
    let (state, percent) = fields.split_once(';').unwrap_or((fields, ""));
    let percent = percent.trim().parse::<u32>().ok().map(|p| p.min(100) as u8);
    Some(match state.trim() {
        "0" => Progress::Remove,
        "1" => Progress::Set(percent.unwrap_or(0)),
        "2" => Progress::Error(percent),
        "3" => Progress::Indeterminate,
        "4" => Progress::Pause(percent),
        _ => return None,
    })
}

/// Standard base64, which is what kitty's `e=1` means. Padding is tolerated
/// rather than required, since terminals emit it both ways.
fn base64(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{Event, Notification, Progress, Sniffer};

    fn events(chunks: &[&str]) -> Vec<Event> {
        let mut sniffer = Sniffer::default();
        let mut out = Vec::new();
        for chunk in chunks {
            sniffer.feed(chunk.as_bytes(), &mut out);
        }
        out
    }

    fn notify(title: &str, body: &str) -> Event {
        Event::Notify(Notification {
            title: title.to_owned(),
            body: body.to_owned(),
        })
    }

    #[test]
    fn osc_9_is_a_notification() {
        assert_eq!(
            events(&["\x1b]9;build finished\x07"]),
            [notify("", "build finished")]
        );
    }

    #[test]
    fn a_string_terminator_ends_it_too() {
        assert_eq!(events(&["\x1b]9;done\x1b\\"]), [notify("", "done")]);
    }

    #[test]
    fn a_sequence_split_across_reads_survives() {
        assert_eq!(
            events(&["\x1b]777;notify;Buil", "d;it worked\x07"]),
            [notify("Build", "it worked")]
        );
    }

    #[test]
    fn a_body_may_carry_semicolons() {
        assert_eq!(
            events(&["\x1b]777;notify;Build;one; two; three\x07"]),
            [notify("Build", "one; two; three")]
        );
    }

    #[test]
    fn osc_9_4_is_progress_not_a_notification() {
        assert_eq!(
            events(&["\x1b]9;4;1;40\x07"]),
            [Event::Progress(Progress::Set(40))]
        );
        assert_eq!(
            events(&["\x1b]9;4;0\x07"]),
            [Event::Progress(Progress::Remove)]
        );
        assert_eq!(
            events(&["\x1b]9;4;3\x07"]),
            [Event::Progress(Progress::Indeterminate)]
        );
        assert_eq!(
            events(&["\x1b]9;4;2\x07"]),
            [Event::Progress(Progress::Error(None))]
        );
    }

    #[test]
    fn a_percentage_is_clamped() {
        assert_eq!(
            events(&["\x1b]9;4;1;400\x07"]),
            [Event::Progress(Progress::Set(100))]
        );
    }

    #[test]
    fn kitty_notifications_carry_a_title_by_default() {
        assert_eq!(events(&["\x1b]99;;hello\x1b\\"]), [notify("hello", "")]);
        assert_eq!(
            events(&["\x1b]99;i=1:p=body;hello\x1b\\"]),
            [notify("", "hello")]
        );
    }

    #[test]
    fn kitty_chunks_are_joined() {
        assert_eq!(
            events(&[
                "\x1b]99;i=7:d=0;one \x1b\\",
                "\x1b]99;i=7:d=0;two \x1b\\",
                "\x1b]99;i=7;three\x1b\\",
            ]),
            [notify("one two three", "")]
        );
    }

    #[test]
    fn kitty_base64_is_decoded() {
        assert_eq!(
            events(&["\x1b]99;i=2:e=1;aGVsbG8=\x1b\\"]),
            [notify("hello", "")]
        );
    }

    #[test]
    fn a_passthrough_body_is_not_scanned() {
        // tmux wrapping an OSC 9 in a DCS passthrough means the sequence is
        // for tmux, not for us.
        assert!(events(&["\x1bPtmux;\x1b\x1b]9;inner\x07\x1b\\"]).is_empty());
    }

    #[test]
    fn other_sequences_are_ignored() {
        assert!(
            events(&[
                "\x1b[31mred\x1b[0m",
                "\x1b]0;a title\x07",
                "\x1b]52;c;Zm9v\x07"
            ])
            .is_empty()
        );
    }

    #[test]
    fn an_abandoned_payload_does_not_leak_into_the_next() {
        assert_eq!(
            events(&["\x1b]9;lost\x1b[0m", "\x1b]9;found\x07"]),
            [notify("", "found")]
        );
    }

    #[test]
    fn a_runaway_payload_is_dropped() {
        let mut giant = String::from("\x1b]9;");
        giant.push_str(&"x".repeat(16 * 1024));
        giant.push('\x07');
        assert!(events(&[&giant]).is_empty());
    }
}
