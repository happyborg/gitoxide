use crate::{object, Sign, Time};
use hex::FromHex;
use std::str;

quick_error! {
    #[derive(Debug)]
    pub enum Error {
        InvalidObjectKind(kind: Vec<u8>) {
            display("Unknown object kind: {:?}", std::str::from_utf8(&kind))
        }
        ParseError(msg: &'static str, kind: Vec<u8>) {
            display("{}: {:?}", msg, std::str::from_utf8(&kind))
        }
        ObjectKind(err: object::Error) {
            from()
            cause(err)
        }
    }
}

const PGP_SIGNATURE_BEGIN: &'static [u8] = b"-----BEGIN PGP SIGNATURE-----";
const PGP_SIGNATURE_END: &'static [u8] = b"-----END PGP SIGNATURE-----";

#[derive(PartialEq, Eq, Debug, Hash)]
pub enum Object<'data> {
    Tag(Tag<'data>),
}

impl<'data> Object<'data> {
    pub fn kind(&self) -> object::Kind {
        match self {
            Object::Tag(_) => object::Kind::Tag,
        }
    }
}

#[derive(PartialEq, Eq, Debug, Hash)]
pub struct Signature<'data> {
    pub name: &'data [u8],
    pub email: &'data [u8],
    pub time: Time,
}

#[derive(PartialEq, Eq, Debug, Hash)]
pub struct Tag<'data> {
    pub target_raw: &'data [u8],
    pub name_raw: &'data [u8],
    pub target_kind: object::Kind,
    pub message: Option<&'data [u8]>,
    pub pgp_signature: Option<&'data [u8]>,
    pub signature: Signature<'data>,
}

fn split2_at_space(
    d: &[u8],
    is_valid: impl FnOnce(&[u8], &[u8]) -> bool,
) -> Result<(&[u8], &[u8]), Error> {
    let mut t = d.splitn(2, |&b| b == b' ');
    Ok(match (t.next(), t.next()) {
        (Some(t1), Some(t2)) => {
            if !is_valid(t1, t2) {
                return Err(Error::ParseError(
                    "Invalid space separated tokens - validation failed",
                    d.to_owned(),
                ));
            }
            (t1, t2)
        }
        _ => {
            return Err(Error::ParseError(
                "Invalid tokens - expected 2 when split at space",
                d.to_owned(),
            ))
        }
    })
}

fn parse_timezone_offset(d: &str) -> Result<(i32, Sign), Error> {
    let db = d.as_bytes();
    if d.len() < 5 || !(db[0] == b'+' || db[0] == b'-') {
        return Err(Error::ParseError(
            "invalid timezone offset",
            d.as_bytes().to_owned(),
        ));
    }
    let sign = if db[0] == b'-' {
        Sign::Minus
    } else {
        Sign::Plus
    };
    let hours = str::from_utf8(&db[..3])
        .expect("valid utf8")
        .parse::<i32>()
        .map_err(|_| Error::ParseError("invalid 'hours' string", db[..3].to_owned()))?;
    let minutes = str::from_utf8(&db[3..])
        .expect("valid utf8")
        .parse::<i32>()
        .map_err(|_| Error::ParseError("invalid 'minutes' string", db[3..].to_owned()))?;
    Ok((hours * 3600 + minutes * 60, sign))
}

fn parse_signature(d: &[u8]) -> Result<Signature, Error> {
    const ONE_SPACE: usize = 1;
    let email_begin = d
        .iter()
        .position(|&b| b == b'<')
        .ok_or_else(|| {
            Error::ParseError(
                "Could not find beginning of email marked by '<'",
                d.to_owned(),
            )
        })
        .and_then(|pos| {
            if pos == 0 {
                Err(Error::ParseError(
                    "Email found in place of author name",
                    d.to_owned(),
                ))
            } else {
                Ok(pos)
            }
        })?;
    let email_end = email_begin
        + d.iter()
            .skip(email_begin)
            .position(|&b| b == b'>')
            .ok_or_else(|| {
                Error::ParseError("Could not find end of email marked by '>'", d.to_owned())
            })
            .and_then(|pos| {
                if pos >= d.len() - 1 - ONE_SPACE {
                    Err(Error::ParseError(
                        "There is no time after email",
                        d.to_owned(),
                    ))
                } else {
                    Ok(pos)
                }
            })?;
    let (time_in_seconds, tzofz) = split2_at_space(&d[email_end + ONE_SPACE + 1..], |_, _| true)
        .map(|(t1, t2)| {
            (
                str::from_utf8(t1).expect("utf-8 encoded time in seconds"),
                str::from_utf8(t2).expect("utf=8 encoded timezone offset"),
            )
        })?;
    let (offset, sign) = parse_timezone_offset(tzofz)?;

    Ok(Signature {
        name: &d[..email_begin - ONE_SPACE],
        email: &d[email_begin + 1..email_end],
        time: Time {
            time: time_in_seconds.parse::<u32>().map_err(|_| {
                Error::ParseError(
                    "Could parse to seconds",
                    time_in_seconds.as_bytes().to_owned(),
                )
            })?,
            offset,
            sign,
        },
    })
}

fn parse_message<'data>(
    d: &'data [u8],
    mut lines: impl Iterator<Item = &'data [u8]>,
) -> Result<(Option<&'data [u8]>, Option<&'data [u8]>), Error> {
    Ok(match lines.next() {
        Some(l) if l.len() == 0 => {
            let msg_begin = 0; // TODO: use nom to parse this or do it without needing nightly
            if msg_begin >= d.len() {
                return Err(Error::ParseError(
                    "Message separator was not followed by message",
                    d.to_owned(),
                ));
            }
            let mut msg_end = d.len();
            let mut pgp_signature = None;
            if let Some(_pgp_begin_line) = lines.find(|l| l.starts_with(PGP_SIGNATURE_BEGIN)) {
                match lines.find(|l| l.starts_with(PGP_SIGNATURE_END)) {
                    None => {
                        return Err(Error::ParseError(
                            "Didn't find end of signature marker",
                            d.to_owned(),
                        ))
                    }
                    Some(_) => {
                        msg_end = d.len(); // TODO: use nom to parse this or do it without needing nightly
                        pgp_signature = Some(&d[msg_end..])
                    }
                }
            }
            (Some(&d[msg_begin..msg_end]), pgp_signature)
        }
        Some(l) => {
            return Err(Error::ParseError(
                "Expected empty newline to separate message",
                l.to_owned(),
            ))
        }
        None => (None, None),
    })
}

impl<'data> Tag<'data> {
    pub fn target(&self) -> object::Id {
        <[u8; 20]>::from_hex(self.target_raw).expect("prior validation")
    }
    pub fn from_bytes(d: &'data [u8]) -> Result<Tag<'data>, Error> {
        let mut lines = d.split(|&b| b == b'\n');
        let (target, target_kind, name, signature) =
            match (lines.next(), lines.next(), lines.next(), lines.next()) {
                (Some(target), Some(kind), Some(name), Some(tagger)) => {
                    let (_, target) = split2_at_space(target, |f, v| {
                        f == b"object" && v.len() == 40 && <[u8; 20]>::from_hex(v).is_ok()
                    })?;
                    let kind = split2_at_space(kind, |f, _v| f == b"type")
                        .and_then(|(_, kind)| object::Kind::from_bytes(kind).map_err(Into::into))?;
                    let (_, name) = split2_at_space(name, |f, _v| f == b"tag")?;
                    let (_, tagger) = split2_at_space(tagger, |f, _v| f == b"tagger")?;
                    (target, kind, name, parse_signature(tagger)?)
                }
                _ => {
                    return Err(Error::ParseError(
                        "Expected four lines: target, type, tag and tagger",
                        d.to_owned(),
                    ))
                }
            };

        let (message, pgp_signature) = parse_message(d, &mut lines)?;

        Ok(Tag {
            target_raw: target,
            name_raw: name,
            target_kind,
            message,
            signature,
            pgp_signature,
        })
    }
}