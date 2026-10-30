//! The few X.509 certificate fields the trust prompt shows (DESIGN.md
//! §6.2): validity, subject and issuer, and subject alternative names.
//! Ported from trust-desktop's `src/tls/x509.rs`, with the issuer and
//! organization added. RFC 5280 §4.1 (Certificate, TBSCertificate),
//! §4.1.2.4 (Issuer), §4.1.2.5 (Validity; UTCTime §4.1.2.5.1,
//! GeneralizedTime §4.1.2.5.2), §4.1.2.6 (Subject) and §4.2.1.6 (Subject
//! Alternative Name); X.690 DER definite-length encoding.
//!
//! This reader never decides trust: a malformed or unusual certificate
//! simply yields fewer facts.

/// One DER element: its tag and contents.
struct Element<'a> {
    tag: u8,
    body: &'a [u8],
}

/// Reads the element at the start of `input`; returns it and what follows.
fn element(input: &[u8]) -> Option<(Element<'_>, &[u8])> {
    let (&tag, rest) = input.split_first()?;
    // Multi-byte tag numbers never occur in the fields read here.
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        // DER forbids the indefinite form; 4 octets bound any certificate.
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let length = rest[..count]
            .iter()
            .fold(0usize, |n, b| (n << 8) | usize::from(*b));
        (length, &rest[count..])
    };
    if rest.len() < length {
        return None;
    }
    Some((
        Element {
            tag,
            body: &rest[..length],
        },
        &rest[length..],
    ))
}

fn children(mut body: &[u8]) -> impl Iterator<Item = Element<'_>> {
    std::iter::from_fn(move || {
        let (item, rest) = element(body)?;
        body = rest;
        Some(item)
    })
}

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const OID: u8 = 0x06;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const OCTET_STRING: u8 = 0x04;
const EXTENSIONS: u8 = 0xa3;
/// id-at-commonName 2.5.4.3
const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
/// id-at-organizationName 2.5.4.10
const ORGANIZATION: &[u8] = &[0x55, 0x04, 0x0a];
/// id-ce-subjectAltName 2.5.29.17
const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];

/// A distinguished name's common name and organization.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Name {
    pub common_name: Option<String>,
    pub organization: Option<String>,
}

impl Name {
    /// `common name (organization)`, or whichever there is.
    pub fn display(&self) -> String {
        match (&self.common_name, &self.organization) {
            (Some(name), Some(organization)) if name != organization => {
                format!("{name} ({organization})")
            }
            (Some(name), _) => name.clone(),
            (None, Some(organization)) => organization.clone(),
            (None, None) => String::from("(unnamed)"),
        }
    }
}

/// Facts about one certificate. Times are seconds since the Unix epoch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Certificate {
    pub not_before: Option<i64>,
    pub not_after: Option<i64>,
    pub subject: Name,
    pub issuer: Name,
    pub dns_names: Vec<String>,
    pub ip_addresses: Vec<std::net::IpAddr>,
}

/// RFC 5280 §4.1.2.5.1–2: `YYMMDDHHMMSSZ` or `YYYYMMDDHHMMSSZ`.
fn time(item: &Element<'_>) -> Option<i64> {
    let text = std::str::from_utf8(item.body).ok()?;
    let digits = text.strip_suffix('Z')?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (year, rest) = match (item.tag, digits.len()) {
        (UTC_TIME, 12) => {
            let yy: i64 = digits[..2].parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &digits[2..])
        }
        (GENERALIZED_TIME, 14) => (digits[..4].parse().ok()?, &digits[4..]),
        _ => return None,
    };
    let field = |n: usize| -> Option<i64> { rest.get(n * 2..n * 2 + 2)?.parse().ok() };
    let (month, day, hour, minute, second) =
        (field(0)?, field(1)?, field(2)?, field(3)?, field(4)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = crate::platform::time::days_from_civil(year as i32, month as u8, day as u8);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

fn name(element: &Element<'_>) -> Name {
    // Name ::= SEQUENCE OF RelativeDistinguishedName (SET OF
    // AttributeTypeAndValue); the most specific (last) value wins.
    let mut name = Name::default();
    for rdn in children(element.body).filter(|e| e.tag == SET) {
        for attribute in children(rdn.body).filter(|e| e.tag == SEQUENCE) {
            let mut parts = children(attribute.body);
            if let (Some(oid), Some(value)) = (parts.next(), parts.next())
                && oid.tag == OID
            {
                // PrintableString, UTF8String, IA5String, TeletexString...
                let text = || Some(String::from_utf8_lossy(value.body).into_owned());
                if oid.body == COMMON_NAME {
                    name.common_name = text();
                } else if oid.body == ORGANIZATION {
                    name.organization = text();
                }
            }
        }
    }
    name
}

fn alternative_names(certificate: &mut Certificate, extensions: &Element<'_>) {
    let Some((list, _)) = element(extensions.body) else {
        return;
    };
    for extension in children(list.body).filter(|e| e.tag == SEQUENCE) {
        let mut parts = children(extension.body);
        let Some(oid) = parts.next() else { continue };
        if oid.tag != OID || oid.body != SUBJECT_ALT_NAME {
            continue;
        }
        // extnValue is the last OCTET STRING (critical BOOLEAN may precede).
        let Some(value) = parts.filter(|e| e.tag == OCTET_STRING).last() else {
            continue;
        };
        let Some((names, _)) = element(value.body) else {
            continue;
        };
        for name in children(names.body).take(256) {
            match name.tag {
                // dNSName [2] IA5String
                0x82 => {
                    if let Ok(dns) = std::str::from_utf8(name.body) {
                        certificate.dns_names.push(dns.to_ascii_lowercase());
                    }
                }
                // iPAddress [7] OCTET STRING
                0x87 => match name.body.len() {
                    4 => certificate.ip_addresses.push(std::net::IpAddr::from(
                        <[u8; 4]>::try_from(name.body).unwrap(),
                    )),
                    16 => certificate.ip_addresses.push(std::net::IpAddr::from(
                        <[u8; 16]>::try_from(name.body).unwrap(),
                    )),
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// RFC 5280 §4.1: reads validity, names and alternative names from a DER
/// certificate.
pub fn parse(der: &[u8]) -> Option<Certificate> {
    let (certificate, _) = element(der)?;
    if certificate.tag != SEQUENCE {
        return None;
    }
    let (tbs, _) = element(certificate.body)?;
    if tbs.tag != SEQUENCE {
        return None;
    }
    let mut fields = children(tbs.body).peekable();
    // version [0] EXPLICIT is optional.
    if fields.peek().is_some_and(|e| e.tag == 0xa0) {
        fields.next();
    }
    let _serial = fields.next()?;
    let _signature = fields.next()?;
    let issuer = fields.next()?;
    let validity = fields.next()?;
    let subject = fields.next()?;
    let _key = fields.next()?;
    let mut parsed = Certificate::default();
    if validity.tag == SEQUENCE {
        let mut times = children(validity.body);
        parsed.not_before = times.next().as_ref().and_then(time);
        parsed.not_after = times.next().as_ref().and_then(time);
    }
    if issuer.tag == SEQUENCE {
        parsed.issuer = name(&issuer);
    }
    if subject.tag == SEQUENCE {
        parsed.subject = name(&subject);
    }
    for field in fields {
        if field.tag == EXTENSIONS {
            alternative_names(&mut parsed, &field);
        }
    }
    Some(parsed)
}

impl Certificate {
    /// Names this certificate presents, for display.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.dns_names.clone();
        names.extend(self.ip_addresses.iter().map(ToString::to_string));
        if names.is_empty()
            && let Some(name) = &self.subject.common_name
        {
            names.push(name.clone());
        }
        names
    }

    /// Issued by itself: the subject is the issuer.
    pub fn self_signed(&self) -> bool {
        self.subject == self.issuer
    }
}

/// A UTC calendar date for display, `YYYY-MM-DD`.
pub fn date(seconds: i64) -> String {
    let utc = crate::platform::time::utc(seconds * 1000);
    format!("{:04}-{:02}-{:02}", utc.year, utc.month, utc.day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;

    fn der(pem: &str) -> Vec<u8> {
        CertificateDer::from_pem_slice(pem.as_bytes())
            .unwrap()
            .as_ref()
            .to_vec()
    }

    #[test]
    fn reads_the_test_certificate() {
        let parsed = parse(&der(include_str!("testdata/server.pem"))).unwrap();
        // 2026 is a UTCTime and 2126 a GeneralizedTime (RFC 5280 §4.1.2.5).
        assert_eq!(date(parsed.not_before.unwrap())[..4], *"2026");
        assert_eq!(date(parsed.not_after.unwrap())[..4], *"2126");
        assert_eq!(parsed.subject.display(), "localhost (RubyChat test server)");
        assert!(parsed.self_signed());
        assert_eq!(parsed.names(), ["localhost", "127.0.0.1"]);
    }

    #[test]
    fn garbage_yields_nothing() {
        let der = der(include_str!("testdata/other.pem"));
        assert!(parse(b"").is_none());
        assert!(parse(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]).is_none());
        assert!(parse(&der[..40]).is_none());
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(Name::default().display(), "(unnamed)");
    }
}
