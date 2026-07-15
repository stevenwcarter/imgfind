//! Pure telnet byte-stream parser: strips IAC negotiation, surfaces NAWS
//! window-size updates, and yields real user-input bytes.

// Telnet command bytes (RFC 854 / 1073).
const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_NAWS: u8 = 31;

/// Hard cap on a subnegotiation payload's length. Real subnegotiations we
/// understand (NAWS) are a handful of bytes; this is generous headroom. A
/// client that sends `IAC SB` and then streams bytes forever without a
/// terminating `IAC SE` must not be able to grow `sb` without bound.
const MAX_SUBNEG_LEN: usize = 64;

/// One parsed event from the client byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelnetEvent {
    /// A real user-input byte.
    Data(u8),
    /// A NAWS window-size update.
    WindowSize { cols: u16, rows: u16 },
    /// An observed WILL/WONT/DO/DONT negotiation (we don't act on these).
    Negotiation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Normal data flow.
    Data,
    /// Saw IAC, waiting for the command byte.
    Iac,
    /// Saw IAC + (WILL/WONT/DO/DONT), waiting for the option byte.
    Negotiate,
    /// Inside a subnegotiation (after IAC SB), collecting bytes until IAC SE.
    Subneg,
    /// Inside a subnegotiation and just saw IAC (waiting for SE or escaped IAC).
    SubnegIac,
}

/// Incremental telnet parser. Feed it raw socket bytes; get clean events.
pub struct TelnetParser {
    state: State,
    sb: Vec<u8>,
}

impl TelnetParser {
    pub fn new() -> Self {
        Self {
            state: State::Data,
            sb: Vec::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TelnetEvent> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                State::Data => {
                    if b == IAC {
                        self.state = State::Iac;
                    } else {
                        out.push(TelnetEvent::Data(b));
                    }
                }
                State::Iac => match b {
                    IAC => {
                        // Escaped literal 0xFF.
                        out.push(TelnetEvent::Data(0xFF));
                        self.state = State::Data;
                    }
                    SB => {
                        self.sb.clear();
                        self.state = State::Subneg;
                    }
                    WILL | WONT | DO | DONT => {
                        self.state = State::Negotiate;
                    }
                    _ => {
                        // Standalone command (e.g. GA, NOP) — ignore.
                        self.state = State::Data;
                    }
                },
                State::Negotiate => {
                    // b is the option byte; we don't act on peer negotiation.
                    let _ = b;
                    out.push(TelnetEvent::Negotiation);
                    self.state = State::Data;
                }
                State::Subneg => {
                    if b == IAC {
                        self.state = State::SubnegIac;
                    } else if self.sb.len() >= MAX_SUBNEG_LEN {
                        // Runaway subnegotiation (no terminating IAC SE in
                        // sight): abandon it rather than growing `sb`
                        // unbounded, and fall back to normal data parsing.
                        self.sb.clear();
                        self.state = State::Data;
                    } else {
                        self.sb.push(b);
                    }
                }
                State::SubnegIac => match b {
                    IAC => {
                        // Escaped 0xFF inside subnegotiation payload.
                        self.sb.push(0xFF);
                        self.state = State::Subneg;
                    }
                    SE => {
                        if let Some(ev) = parse_subneg(&self.sb) {
                            out.push(ev);
                        }
                        self.sb.clear();
                        self.state = State::Data;
                    }
                    _ => {
                        // Unexpected; abandon subnegotiation.
                        self.sb.clear();
                        self.state = State::Data;
                    }
                },
            }
        }
        out
    }
}

impl Default for TelnetParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a completed subnegotiation payload (option byte + data).
fn parse_subneg(sb: &[u8]) -> Option<TelnetEvent> {
    if sb.first() == Some(&OPT_NAWS) && sb.len() >= 5 {
        let cols = u16::from_be_bytes([sb[1], sb[2]]);
        let rows = u16::from_be_bytes([sb[3], sb[4]]);
        return Some(TelnetEvent::WindowSize { cols, rows });
    }
    None
}

/// Bytes the server sends on connect: take over echo, suppress go-ahead,
/// and ask the client for its window size.
pub fn initial_negotiation() -> Vec<u8> {
    vec![IAC, WILL, OPT_ECHO, IAC, WILL, OPT_SGA, IAC, DO, OPT_NAWS]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Telnet command bytes for building test inputs.
    const IAC: u8 = 255;
    const DO: u8 = 253;
    const WILL: u8 = 251;
    const SB: u8 = 250;
    const SE: u8 = 240;
    const NAWS: u8 = 31;

    #[test]
    fn plain_data_passes_through() {
        let mut p = TelnetParser::new();
        let events = p.feed(b"hi");
        assert_eq!(
            events,
            vec![TelnetEvent::Data(b'h'), TelnetEvent::Data(b'i')]
        );
    }

    #[test]
    fn naws_subnegotiation_yields_window_size() {
        // IAC SB NAWS 0 120 0 40 IAC SE  => 120 cols, 40 rows
        let mut p = TelnetParser::new();
        let bytes = [IAC, SB, NAWS, 0, 120, 0, 40, IAC, SE];
        let events = p.feed(&bytes);
        assert_eq!(
            events,
            vec![TelnetEvent::WindowSize {
                cols: 120,
                rows: 40
            }]
        );
    }

    #[test]
    fn sequence_split_across_feeds_is_handled() {
        let mut p = TelnetParser::new();
        // Feed a WILL negotiation one byte at a time; expect no Data events.
        assert!(p.feed(&[IAC]).is_empty());
        let ev = p.feed(&[WILL]);
        assert!(ev.iter().all(|e| !matches!(e, TelnetEvent::Data(_))));
        let ev = p.feed(&[NAWS]);
        assert_eq!(ev, vec![TelnetEvent::Negotiation]);
        // Then real data still flows.
        assert_eq!(p.feed(b"x"), vec![TelnetEvent::Data(b'x')]);
    }

    #[test]
    fn escaped_iac_is_a_single_data_byte() {
        let mut p = TelnetParser::new();
        // IAC IAC => literal 0xFF data byte.
        assert_eq!(p.feed(&[IAC, IAC]), vec![TelnetEvent::Data(0xFF)]);
    }

    #[test]
    fn do_command_consumes_option_byte() {
        let mut p = TelnetParser::new();
        // IAC DO NAWS then a data byte.
        let ev = p.feed(&[IAC, DO, NAWS, b'z']);
        assert_eq!(ev, vec![TelnetEvent::Negotiation, TelnetEvent::Data(b'z')]);
    }

    #[test]
    fn runaway_subnegotiation_is_capped_and_recovers() {
        // IAC SB NAWS then 200 arbitrary bytes with no terminating IAC SE:
        // `sb` must never grow past `MAX_SUBNEG_LEN`, and once the cap is
        // hit the parser abandons the subnegotiation and falls back to
        // normal data parsing for whatever follows.
        let mut p = TelnetParser::new();
        assert!(p.feed(&[IAC, SB, NAWS]).is_empty());
        let filler = [b'a'; 200];
        for chunk in filler.chunks(7) {
            p.feed(chunk);
            assert!(p.sb.len() <= MAX_SUBNEG_LEN);
        }
        assert!(p.sb.len() <= MAX_SUBNEG_LEN);
        // The parser has abandoned the subnegotiation and returned to
        // State::Data, so a normal byte now parses as ordinary input.
        assert_eq!(p.feed(b"x"), vec![TelnetEvent::Data(b'x')]);
    }

    #[test]
    fn initial_negotiation_requests_echo_sga_naws() {
        let bytes = initial_negotiation();
        // Must contain IAC WILL ECHO, IAC WILL SGA, IAC DO NAWS.
        assert!(bytes.windows(3).any(|w| w == [IAC, WILL, 1])); // ECHO=1
        assert!(bytes.windows(3).any(|w| w == [IAC, WILL, 3])); // SGA=3
        assert!(bytes.windows(3).any(|w| w == [IAC, DO, NAWS])); // NAWS=31
    }
}
