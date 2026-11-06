use bytes::Bytes;
use smallvec::SmallVec;

#[derive(Debug, PartialEq, Eq)]
pub enum RespCommand {
    Ping(Option<Bytes>),
    Echo(Bytes),
    Get(Bytes),
    Set {
        key: Bytes,
        value: Bytes,
        ttl_ms: Option<u64>,
        nx: bool,
        xx: bool,
    },
    Del(Vec<Bytes>),
    Exists(Vec<Bytes>),
    MGet(Vec<Bytes>),
    MSet(Vec<(Bytes, Bytes)>),
    Incr {
        key: Bytes,
        delta: i64,
    },
    Ttl(Bytes),
    Pttl(Bytes),
    Expire {
        key: Bytes,
        seconds: u64,
    },
    DbSize,
    FlushDb,
    Command,
    Info(Option<Bytes>),
    Hello {
        proto: usize,
    },
    Quit,
    Cluster(Vec<Bytes>),
    Tier(Vec<Bytes>),
    Config(Vec<Bytes>),
    Client,
    Auth,
    Unknown(Bytes),
}

#[inline(always)]
fn parse_ascii_usize(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() {
        return None;
    }
    let mut val = 0usize;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as usize)?;
    }
    Some(val)
}

#[inline(always)]
fn parse_ascii_i64(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let (neg, digits) = if bytes[0] == b'-' {
        (true, &bytes[1..])
    } else if bytes[0] == b'+' {
        (false, &bytes[1..])
    } else {
        (false, bytes)
    };
    if digits.is_empty() {
        return None;
    }
    let mut val = 0i64;
    for &b in digits {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as i64)?;
    }
    Some(if neg { -val } else { val })
}

/// Zero-copy RESP parser. Returns the parsed command and how many bytes were consumed from the input buffer.
pub struct RespParser;

impl RespParser {
    pub fn parse(input: &[u8]) -> Result<Option<(RespCommand, usize)>, String> {
        if input.is_empty() {
            return Ok(None);
        }

        if input[0] == b'*' {
            Self::parse_array(input)
        } else {
            Self::parse_inline(input)
        }
    }

    fn parse_inline(input: &[u8]) -> Result<Option<(RespCommand, usize)>, String> {
        let crlf = match memchr::memchr(b'\r', input) {
            Some(pos) if pos + 1 < input.len() && input[pos + 1] == b'\n' => pos,
            _ => return Ok(None),
        };

        let line = &input[..crlf];
        let consumed = crlf + 2;

        let mut parts: SmallVec<[&[u8]; 4]> = SmallVec::new();
        for p in line.split(|&b| b == b' ' || b == b'\t').filter(|p| !p.is_empty()) {
            parts.push(p);
        }

        if parts.is_empty() {
            return Ok(Some((RespCommand::Unknown(Bytes::new()), consumed)));
        }

        let cmd = Self::build_command(&parts)?;
        Ok(Some((cmd, consumed)))
    }

    fn parse_array(input: &[u8]) -> Result<Option<(RespCommand, usize)>, String> {
        let mut offset = 1;

        let line_end = match memchr::memchr(b'\r', &input[offset..]) {
            Some(pos) if offset + pos + 1 < input.len() && input[offset + pos + 1] == b'\n' => offset + pos,
            _ => return Ok(None),
        };

        let count = match parse_ascii_usize(&input[offset..line_end]) {
            Some(c) => c,
            None => return Err("ERR Protocol error: invalid array length".to_string()),
        };

        offset = line_end + 2;
        let mut parts: SmallVec<[&[u8]; 4]> = SmallVec::with_capacity(count);

        for _ in 0..count {
            if offset >= input.len() {
                return Ok(None);
            }

            if input[offset] != b'$' {
                return Err("ERR Protocol error: expected bulk string '$'".to_string());
            }
            offset += 1;

            let bulk_len_end = match memchr::memchr(b'\r', &input[offset..]) {
                Some(pos) if offset + pos + 1 < input.len() && input[offset + pos + 1] == b'\n' => offset + pos,
                _ => return Ok(None),
            };

            let bulk_len = match parse_ascii_usize(&input[offset..bulk_len_end]) {
                Some(l) => l,
                None => return Err("ERR Protocol error: invalid bulk string length".to_string()),
            };

            offset = bulk_len_end + 2;

            if offset + bulk_len + 2 > input.len() {
                return Ok(None); // Need more data for bulk payload
            }

            let slice = &input[offset..offset + bulk_len];
            parts.push(slice);
            offset += bulk_len + 2; // skip \r\n
        }

        let cmd = Self::build_command(&parts)?;
        Ok(Some((cmd, offset)))
    }

    fn build_command(parts: &[&[u8]]) -> Result<RespCommand, String> {
        if parts.is_empty() {
            return Ok(RespCommand::Unknown(Bytes::new()));
        }

        let p0 = parts[0];
        if p0.is_empty() {
            return Ok(RespCommand::Unknown(Bytes::new()));
        }

        // Direct first-byte jump table dispatch for maximum CPU branch predictability
        match p0[0] {
            b'G' | b'g' if p0.len() == 3 && p0.eq_ignore_ascii_case(b"GET") => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'get' command".to_string())
                } else {
                    Ok(RespCommand::Get(Bytes::copy_from_slice(parts[1])))
                }
            }
            b'S' | b's' if p0.len() == 3 && p0.eq_ignore_ascii_case(b"SET") => {
                if parts.len() < 3 {
                    return Err("ERR wrong number of arguments for 'set' command".to_string());
                }
                let key = Bytes::copy_from_slice(parts[1]);
                let value = Bytes::copy_from_slice(parts[2]);
                let mut ttl_ms = None;
                let mut nx = false;
                let mut xx = false;

                let mut i = 3;
                while i < parts.len() {
                    let opt = parts[i];
                    if opt.eq_ignore_ascii_case(b"EX") {
                        if i + 1 >= parts.len() {
                            return Err("ERR syntax error".to_string());
                        }
                        let secs = parse_ascii_usize(parts[i + 1])
                            .ok_or_else(|| "ERR value is not an integer or out of range".to_string())?;
                        ttl_ms = Some((secs as u64) * 1000);
                        i += 2;
                    } else if opt.eq_ignore_ascii_case(b"PX") {
                        if i + 1 >= parts.len() {
                            return Err("ERR syntax error".to_string());
                        }
                        let ms = parse_ascii_usize(parts[i + 1])
                            .ok_or_else(|| "ERR value is not an integer or out of range".to_string())?;
                        ttl_ms = Some(ms as u64);
                        i += 2;
                    } else if opt.eq_ignore_ascii_case(b"NX") {
                        nx = true;
                        i += 1;
                    } else if opt.eq_ignore_ascii_case(b"XX") {
                        xx = true;
                        i += 1;
                    } else {
                        i += 1;
                    }
                }

                Ok(RespCommand::Set {
                    key,
                    value,
                    ttl_ms,
                    nx,
                    xx,
                })
            }
            b'P' | b'p' => {
                if p0.len() == 4 && p0.eq_ignore_ascii_case(b"PING") {
                    let arg = if parts.len() > 1 {
                        Some(Bytes::copy_from_slice(parts[1]))
                    } else {
                        None
                    };
                    Ok(RespCommand::Ping(arg))
                } else if p0.len() == 4 && p0.eq_ignore_ascii_case(b"PTTL") {
                    if parts.len() < 2 {
                        Err("ERR wrong number of arguments for 'pttl' command".to_string())
                    } else {
                        Ok(RespCommand::Pttl(Bytes::copy_from_slice(parts[1])))
                    }
                } else {
                    Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0)))
                }
            }
            b'I' | b'i' => {
                if p0.len() == 4 && p0.eq_ignore_ascii_case(b"INCR") {
                    if parts.len() < 2 {
                        Err("ERR wrong number of arguments for 'incr' command".to_string())
                    } else {
                        Ok(RespCommand::Incr {
                            key: Bytes::copy_from_slice(parts[1]),
                            delta: 1,
                        })
                    }
                } else if p0.len() == 6 && p0.eq_ignore_ascii_case(b"INCRBY") {
                    if parts.len() < 3 {
                        Err("ERR wrong number of arguments for 'incrby' command".to_string())
                    } else {
                        let delta = parse_ascii_i64(parts[2])
                            .ok_or_else(|| "ERR value is not an integer or out of range".to_string())?;
                        Ok(RespCommand::Incr {
                            key: Bytes::copy_from_slice(parts[1]),
                            delta,
                        })
                    }
                } else if p0.len() == 4 && p0.eq_ignore_ascii_case(b"INFO") {
                    let section = if parts.len() > 1 {
                        Some(Bytes::copy_from_slice(parts[1]))
                    } else {
                        None
                    };
                    Ok(RespCommand::Info(section))
                } else {
                    Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0)))
                }
            }
            b'D' | b'd' => {
                if p0.len() == 3 && p0.eq_ignore_ascii_case(b"DEL") {
                    if parts.len() < 2 {
                        Err("ERR wrong number of arguments for 'del' command".to_string())
                    } else {
                        let keys = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                        Ok(RespCommand::Del(keys))
                    }
                } else if p0.len() == 4 && p0.eq_ignore_ascii_case(b"DECR") {
                    if parts.len() < 2 {
                        Err("ERR wrong number of arguments for 'decr' command".to_string())
                    } else {
                        Ok(RespCommand::Incr {
                            key: Bytes::copy_from_slice(parts[1]),
                            delta: -1,
                        })
                    }
                } else if p0.len() == 6 && p0.eq_ignore_ascii_case(b"DECRBY") {
                    if parts.len() < 3 {
                        Err("ERR wrong number of arguments for 'decrby' command".to_string())
                    } else {
                        let delta = parse_ascii_i64(parts[2])
                            .ok_or_else(|| "ERR value is not an integer or out of range".to_string())?;
                        Ok(RespCommand::Incr {
                            key: Bytes::copy_from_slice(parts[1]),
                            delta: -delta,
                        })
                    }
                } else if p0.len() == 6 && p0.eq_ignore_ascii_case(b"DBSIZE") {
                    Ok(RespCommand::DbSize)
                } else {
                    Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0)))
                }
            }
            b'E' | b'e' => {
                if p0.len() == 6 && p0.eq_ignore_ascii_case(b"EXISTS") {
                    if parts.len() < 2 {
                        Err("ERR wrong number of arguments for 'exists' command".to_string())
                    } else {
                        let keys = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                        Ok(RespCommand::Exists(keys))
                    }
                } else if p0.len() == 6 && p0.eq_ignore_ascii_case(b"EXPIRE") {
                    if parts.len() < 3 {
                        Err("ERR wrong number of arguments for 'expire' command".to_string())
                    } else {
                        let seconds = parse_ascii_usize(parts[2])
                            .ok_or_else(|| "ERR value is not an integer or out of range".to_string())? as u64;
                        Ok(RespCommand::Expire {
                            key: Bytes::copy_from_slice(parts[1]),
                            seconds,
                        })
                    }
                } else if p0.len() == 4 && p0.eq_ignore_ascii_case(b"ECHO") {
                    if parts.len() < 2 {
                        Err("ERR wrong number of arguments for 'echo' command".to_string())
                    } else {
                        Ok(RespCommand::Echo(Bytes::copy_from_slice(parts[1])))
                    }
                } else {
                    Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0)))
                }
            }
            b'M' | b'm' => {
                if p0.len() == 4 && p0.eq_ignore_ascii_case(b"MGET") {
                    if parts.len() < 2 {
                        Err("ERR wrong number of arguments for 'mget' command".to_string())
                    } else {
                        let keys = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                        Ok(RespCommand::MGet(keys))
                    }
                } else if p0.len() == 4 && p0.eq_ignore_ascii_case(b"MSET") {
                    if parts.len() < 3 || (parts.len() - 1) % 2 != 0 {
                        Err("ERR wrong number of arguments for 'mset' command".to_string())
                    } else {
                        let mut pairs = Vec::with_capacity((parts.len() - 1) / 2);
                        for chunk in parts[1..].chunks(2) {
                            pairs.push((Bytes::copy_from_slice(chunk[0]), Bytes::copy_from_slice(chunk[1])));
                        }
                        Ok(RespCommand::MSet(pairs))
                    }
                } else {
                    Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0)))
                }
            }
            b'T' | b't' => {
                if p0.len() == 3 && p0.eq_ignore_ascii_case(b"TTL") {
                    if parts.len() < 2 {
                        Err("ERR wrong number of arguments for 'ttl' command".to_string())
                    } else {
                        Ok(RespCommand::Ttl(Bytes::copy_from_slice(parts[1])))
                    }
                } else if p0.len() == 4 && p0.eq_ignore_ascii_case(b"TIER") {
                    let args = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                    Ok(RespCommand::Tier(args))
                } else {
                    Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0)))
                }
            }
            b'C' | b'c' => {
                if p0.len() == 6 && p0.eq_ignore_ascii_case(b"CONFIG") {
                    let args = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                    Ok(RespCommand::Config(args))
                } else if p0.len() == 6 && p0.eq_ignore_ascii_case(b"CLIENT") {
                    Ok(RespCommand::Client)
                } else if p0.len() == 7 && p0.eq_ignore_ascii_case(b"CLUSTER") {
                    let args = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                    Ok(RespCommand::Cluster(args))
                } else if p0.len() == 7 && p0.eq_ignore_ascii_case(b"COMMAND") {
                    Ok(RespCommand::Command)
                } else {
                    Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0)))
                }
            }
            b'Q' | b'q' if p0.len() == 4 && p0.eq_ignore_ascii_case(b"QUIT") => Ok(RespCommand::Quit),
            b'A' | b'a' if p0.len() == 4 && p0.eq_ignore_ascii_case(b"AUTH") => Ok(RespCommand::Auth),
            b'F' | b'f' if (p0.len() == 7 && p0.eq_ignore_ascii_case(b"FLUSHDB"))
                || (p0.len() == 8 && p0.eq_ignore_ascii_case(b"FLUSHALL")) => {
                Ok(RespCommand::FlushDb)
            }
            b'H' | b'h' => {
                if p0.len() == 5 && p0.eq_ignore_ascii_case(b"HELLO") {
                    let proto = if parts.len() > 1 {
                        parse_ascii_usize(parts[1]).unwrap_or(2)
                    } else {
                        2
                    };
                    Ok(RespCommand::Hello { proto })
                } else {
                    Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0)))
                }
            }
            _ => Ok(RespCommand::Unknown(Bytes::copy_from_slice(p0))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_inline_ping() {
        let input = b"PING\r\n";
        let (cmd, consumed) = RespParser::parse(input).unwrap().unwrap();
        assert_eq!(cmd, RespCommand::Ping(None));
        assert_eq!(consumed, 6);
    }

    #[test]
    fn test_parse_array_set() {
        let input = b"*3\r\n$3\r\nSET\r\n$5\r\nmykey\r\n$7\r\nmyvalue\r\n";
        let (cmd, consumed) = RespParser::parse(input).unwrap().unwrap();
        assert_eq!(
            cmd,
            RespCommand::Set {
                key: Bytes::from_static(b"mykey"),
                value: Bytes::from_static(b"myvalue"),
                ttl_ms: None,
                nx: false,
                xx: false,
            }
        );
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_parse_incomplete() {
        let input = b"*3\r\n$3\r\nSET\r\n$4\r\n";
        let res = RespParser::parse(input).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn test_parse_incr() {
        let input = b"*2\r\n$4\r\nINCR\r\n$7\r\ncounter\r\n";
        let (cmd, _) = RespParser::parse(input).unwrap().unwrap();
        assert_eq!(
            cmd,
            RespCommand::Incr {
                key: Bytes::from_static(b"counter"),
                delta: 1,
            }
        );
    }
}
