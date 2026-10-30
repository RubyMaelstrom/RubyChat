//! SASL over IRC (IRCv3 SASL 3.1 and 3.2): the AUTHENTICATE exchange, with
//! base64 (RFC 4648 §4) in 400-byte chunks, and the PLAIN (RFC 4616),
//! EXTERNAL (RFC 4422 appendix A) and SCRAM-SHA-256 (RFC 5802, RFC 7677)
//! mechanisms. SCRAM's hashing comes from aws-lc-rs, already here for TLS.

use std::num::NonZeroU32;

use aws_lc_rs::{digest, hmac, pbkdf2, rand};

/// The SASL mechanisms RubyChat speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mechanism {
    Plain,
    External,
    ScramSha256,
}

impl Mechanism {
    pub fn name(self) -> &'static str {
        match self {
            Mechanism::Plain => "PLAIN",
            Mechanism::External => "EXTERNAL",
            Mechanism::ScramSha256 => "SCRAM-SHA-256",
        }
    }
}

/// Server-chosen SCRAM iteration counts past this are refused: the work
/// would hold up every connection on the network thread.
const MAX_ITERATIONS: u32 = 100_000;
const CHUNK: usize = 400;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 with padding (RFC 4648 §4).
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let n = group
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for index in 0..4 {
            if index <= group.len() {
                out.push(ALPHABET[(n >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for (index, group) in text.chunks(4).enumerate() {
        let last = index == text.len() / 4 - 1;
        let padding = group.iter().rev().take_while(|&&b| b == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &byte in &group[..4 - padding] {
            let value = ALPHABET.iter().position(|&a| a == byte)? as u32;
            n = n << 6 | value;
        }
        n <<= 6 * padding as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - padding]);
    }
    Some(out)
}

/// AUTHENTICATE lines carrying `response`: base64 in 400-byte chunks, `+`
/// for an empty response or after a final chunk of exactly 400 bytes.
pub fn authenticate_lines(response: &[u8]) -> Vec<String> {
    let encoded = base64_encode(response);
    let mut lines: Vec<String> = encoded
        .as_bytes()
        .chunks(CHUNK)
        .map(|chunk| format!("AUTHENTICATE {}", String::from_utf8_lossy(chunk)))
        .collect();
    if encoded.is_empty() || encoded.len().is_multiple_of(CHUNK) {
        lines.push(String::from("AUTHENTICATE +"));
    }
    lines
}

/// What to do after a server's AUTHENTICATE.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// More chunks of the server's message are coming.
    Wait,
    /// Send these lines.
    Send(Vec<String>),
    /// Give up: send `AUTHENTICATE *`, for this reason.
    Abort(String),
}

enum Scram {
    /// The client-first message is next.
    Start,
    /// Waiting for the server-first message.
    First {
        nonce: String,
        bare: String,
    },
    /// Waiting for the server-final message, which must carry this signature.
    Final {
        signature: Vec<u8>,
    },
    Done,
}

/// One SASL exchange.
pub struct Exchange {
    mechanism: Mechanism,
    account: String,
    password: String,
    /// The server's message so far, in base64.
    incoming: String,
    scram: Scram,
    /// For tests: a fixed SCRAM client nonce.
    nonce: Option<String>,
}

impl Exchange {
    /// SCRAM's credentials must be ASCII: RFC 5802 §2.2 has clients either
    /// apply SASLprep or refuse other characters, and SASLprep needs Unicode
    /// normalization tables RubyChat doesn't carry.
    pub fn new(mechanism: Mechanism, account: &str, password: &str) -> Result<Self, String> {
        if mechanism == Mechanism::ScramSha256 {
            let printable = |text: &str| text.bytes().all(|b| (0x20..0x7f).contains(&b));
            if !printable(account) || !printable(password) {
                return Err(String::from(
                    "SCRAM-SHA-256 here needs an account and password in plain ASCII; use SASL PLAIN for others",
                ));
            }
        }
        Ok(Self {
            mechanism,
            account: account.to_string(),
            password: password.to_string(),
            incoming: String::new(),
            scram: Scram::Start,
            nonce: None,
        })
    }

    /// The first line: `AUTHENTICATE <mechanism>`.
    pub fn start(&self) -> String {
        format!("AUTHENTICATE {}", self.mechanism.name())
    }

    /// The parameter of a server AUTHENTICATE.
    pub fn receive(&mut self, data: &str) -> Step {
        if data != "+" {
            self.incoming.push_str(data);
            if self.incoming.len() > 16 * 1024 {
                self.incoming.clear();
                return Step::Abort(String::from("the server's SASL message was too long"));
            }
            if data.len() == CHUNK {
                return Step::Wait;
            }
        }
        let Some(payload) = base64_decode(&std::mem::take(&mut self.incoming)) else {
            return Step::Abort(String::from("the server's SASL message wasn't base64"));
        };
        match self.respond(&payload) {
            Ok(response) => Step::Send(authenticate_lines(&response)),
            Err(reason) => Step::Abort(reason),
        }
    }

    fn respond(&mut self, payload: &[u8]) -> Result<Vec<u8>, String> {
        match self.mechanism {
            // [authzid] NUL authcid NUL passwd (RFC 4616 §2).
            Mechanism::Plain => {
                Ok(format!("{0}\0{0}\0{1}", self.account, self.password).into_bytes())
            }
            // An empty authorization identity: the certificate's (RFC 4422
            // appendix A).
            Mechanism::External => Ok(Vec::new()),
            Mechanism::ScramSha256 => self.scram(payload),
        }
    }

    /// RFC 5802 §3 and §5, without channel binding (gs2 header `n,,`).
    fn scram(&mut self, payload: &[u8]) -> Result<Vec<u8>, String> {
        match std::mem::replace(&mut self.scram, Scram::Done) {
            Scram::Start => {
                let nonce = self.nonce.take().unwrap_or_else(|| {
                    let mut bytes = [0u8; 24];
                    let _ = rand::fill(&mut bytes);
                    base64_encode(&bytes)
                });
                // ',' and '=' in names travel as =2C and =3D (§5.1).
                let name = self.account.replace('=', "=3D").replace(',', "=2C");
                let bare = format!("n={name},r={nonce}");
                let message = format!("n,,{bare}").into_bytes();
                self.scram = Scram::First { nonce, bare };
                Ok(message)
            }
            Scram::First { nonce, bare } => {
                let server_first = std::str::from_utf8(payload)
                    .map_err(|_| "the server's SCRAM message wasn't UTF-8")?;
                let attribute = |key: &str| {
                    server_first.split(',').find_map(|part| {
                        part.strip_prefix(key)
                            .and_then(|rest| rest.strip_prefix('='))
                    })
                };
                if server_first.starts_with("m=") {
                    return Err(String::from(
                        "the server asked for a SCRAM extension RubyChat doesn't know",
                    ));
                }
                let combined = attribute("r")
                    .filter(|r| r.starts_with(&nonce) && r.len() > nonce.len())
                    .ok_or("the server's SCRAM nonce didn't extend ours")?;
                let salt = attribute("s")
                    .and_then(base64_decode)
                    .ok_or("the server sent no SCRAM salt")?;
                let iterations: u32 = attribute("i")
                    .and_then(|i| i.parse().ok())
                    .ok_or("the server sent no SCRAM iteration count")?;
                let iterations = NonZeroU32::new(iterations).filter(|i| i.get() <= MAX_ITERATIONS).ok_or_else(|| {
                    format!("the server asked for {iterations} SCRAM iterations; RubyChat stops at {MAX_ITERATIONS}")
                })?;
                let mut salted = [0u8; 32];
                pbkdf2::derive(
                    pbkdf2::PBKDF2_HMAC_SHA256,
                    iterations,
                    &salt,
                    self.password.as_bytes(),
                    &mut salted,
                );
                let mac = |key: &[u8], data: &[u8]| {
                    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data)
                        .as_ref()
                        .to_vec()
                };
                let client_key = mac(&salted, b"Client Key");
                let stored_key = digest::digest(&digest::SHA256, &client_key);
                // "biws" is base64 of the gs2 header "n,,".
                let without_proof = format!("c=biws,r={combined}");
                let auth_message = format!("{bare},{server_first},{without_proof}");
                let client_signature = mac(stored_key.as_ref(), auth_message.as_bytes());
                let proof: Vec<u8> = client_key
                    .iter()
                    .zip(&client_signature)
                    .map(|(k, s)| k ^ s)
                    .collect();
                let server_key = mac(&salted, b"Server Key");
                let signature = mac(&server_key, auth_message.as_bytes());
                self.scram = Scram::Final { signature };
                Ok(format!("{without_proof},p={}", base64_encode(&proof)).into_bytes())
            }
            Scram::Final { signature } => {
                let server_final = std::str::from_utf8(payload).unwrap_or("");
                if let Some(error) = server_final.strip_prefix("e=") {
                    return Err(format!("the server refused the login ({error})"));
                }
                let verifier = server_final.strip_prefix("v=").and_then(base64_decode);
                if verifier.as_deref() != Some(&signature[..]) {
                    return Err(String::from(
                        "the server couldn't prove it knows the password",
                    ));
                }
                // The exchange ends with an empty response (IRCv3 SASL 3.1).
                Ok(Vec::new())
            }
            Scram::Done => Err(String::from(
                "the server sent more SCRAM messages than expected",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_follows_rfc_4648() {
        // RFC 4648 §10.
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), encoded);
            assert_eq!(base64_decode(encoded).as_deref(), Some(plain.as_bytes()));
        }
        assert_eq!(base64_decode("Zm9"), None);
        assert_eq!(base64_decode("Zg==Zg=="), None, "padding only at the end");
        assert_eq!(base64_decode("Z!=="), None);
    }

    #[test]
    fn responses_go_in_400_byte_chunks() {
        assert_eq!(authenticate_lines(b""), ["AUTHENTICATE +"]);
        // 300 bytes encode to exactly 400 characters: a "+" ends the response.
        let lines = authenticate_lines(&[7u8; 300]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "AUTHENTICATE +");
        let lines = authenticate_lines(&[7u8; 301]);
        assert_eq!(
            (lines.len(), lines[0].len()),
            (2, "AUTHENTICATE ".len() + 400)
        );
    }

    #[test]
    fn plain_and_external_follow_the_ircv3_example() {
        // IRCv3 SASL 3.1, "Example protocol exchange".
        let mut plain = Exchange::new(Mechanism::Plain, "jilles", "sesame").unwrap();
        assert_eq!(plain.start(), "AUTHENTICATE PLAIN");
        assert_eq!(
            plain.receive("+"),
            Step::Send(vec![String::from(
                "AUTHENTICATE amlsbGVzAGppbGxlcwBzZXNhbWU="
            )])
        );
        let mut external = Exchange::new(Mechanism::External, "", "").unwrap();
        assert_eq!(
            external.receive("+"),
            Step::Send(vec![String::from("AUTHENTICATE +")])
        );
    }

    #[test]
    fn scram_sha_256_matches_rfc_7677() {
        // RFC 7677 §3: user "user", password "pencil".
        let mut scram = Exchange::new(Mechanism::ScramSha256, "user", "pencil").unwrap();
        scram.nonce = Some(String::from("rOprNGfwEbeRWgbNEkqO"));
        let decoded = |step: Step| match step {
            Step::Send(lines) => String::from_utf8(
                base64_decode(lines[0].strip_prefix("AUTHENTICATE ").unwrap()).unwrap_or_default(),
            )
            .unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            decoded(scram.receive("+")),
            "n,,n=user,r=rOprNGfwEbeRWgbNEkqO"
        );
        let server_first = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        assert_eq!(
            decoded(scram.receive(&base64_encode(server_first.as_bytes()))),
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        let server_final = base64_encode(b"v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=");
        assert_eq!(
            scram.receive(&server_final),
            Step::Send(vec![String::from("AUTHENTICATE +")])
        );
    }

    #[test]
    fn scram_refuses_what_it_should() {
        assert!(
            Exchange::new(Mechanism::ScramSha256, "ruby", "pässword").is_err(),
            "no SASLprep, so ASCII only"
        );
        let mut scram = Exchange::new(Mechanism::ScramSha256, "a,b=c", "pencil").unwrap();
        scram.nonce = Some(String::from("abc"));
        let Step::Send(lines) = scram.receive("+") else {
            panic!()
        };
        let first = String::from_utf8(base64_decode(&lines[0][13..]).unwrap()).unwrap();
        assert_eq!(first, "n,,n=a=2Cb=3Dc,r=abc");
        let foreign = base64_encode(b"r=xyz123,s=AAAA,i=4096");
        assert!(
            matches!(scram.receive(&foreign), Step::Abort(_)),
            "the nonce must extend ours"
        );
        let mut greedy = Exchange::new(Mechanism::ScramSha256, "u", "p").unwrap();
        greedy.nonce = Some(String::from("n"));
        greedy.receive("+");
        let costly = base64_encode(b"r=nmore,s=AAAA,i=999999999");
        assert!(
            matches!(greedy.receive(&costly), Step::Abort(reason) if reason.contains("100000"))
        );
    }
}
