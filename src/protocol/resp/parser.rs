use bytes::Bytes;

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
    Quit,
    Cluster(Vec<Bytes>),
    Tier(Vec<Bytes>),
    Unknown(Bytes),
}

/// Zero-copy RESP parser. Returns the parsed command and how many bytes were consumed from the input buffer.
pub struct RespParser;

impl RespParser {
    pub fn parse(input: &[u8]) -> Result<Option<(RespCommand, usize)>, String> {
        if input.is_empty() {
            return Ok(None);
        }

        // RESP commands usually start with '*' (Array of Bulk Strings)
        // or inline commands like "PING\r\n"
        if input[0] == b'*' {
            Self::parse_array(input)
        } else {
            Self::parse_inline(input)
        }
    }

    fn parse_inline(input: &[u8]) -> Result<Option<(RespCommand, usize)>, String> {
        let crlf = match input.windows(2).position(|w| w == b"\r\n") {
            Some(pos) => pos,
            None => return Ok(None), // Incomplete line
        };

        let line = &input[..crlf];
        let consumed = crlf + 2;

        let parts: Vec<&[u8]> = line
            .split(|&b| b == b' ' || b == b'\t')
            .filter(|p| !p.is_empty())
            .collect();

        if parts.is_empty() {
            return Ok(Some((RespCommand::Unknown(Bytes::new()), consumed)));
        }

        let cmd = Self::build_command(&parts)?;
        Ok(Some((cmd, consumed)))
    }

    fn parse_array(input: &[u8]) -> Result<Option<(RespCommand, usize)>, String> {
        let mut offset = 1;

        // Parse array length
        let line_end = match input[offset..].windows(2).position(|w| w == b"\r\n") {
            Some(pos) => offset + pos,
            None => return Ok(None),
        };

        let count_str = std::str::from_utf8(&input[offset..line_end])
            .map_err(|_| "ERR Protocol error: invalid array length".to_string())?;
        let count: usize = count_str
            .parse()
            .map_err(|_| "ERR Protocol error: invalid array length".to_string())?;

        offset = line_end + 2;
        let mut parts = Vec::with_capacity(count);

        for _ in 0..count {
            if offset >= input.len() {
                return Ok(None); // Need more data
            }

            if input[offset] != b'$' {
                return Err("ERR Protocol error: expected bulk string '$'".to_string());
            }
            offset += 1;

            let bulk_len_end = match input[offset..].windows(2).position(|w| w == b"\r\n") {
                Some(pos) => offset + pos,
                None => return Ok(None),
            };

            let bulk_len_str = std::str::from_utf8(&input[offset..bulk_len_end])
                .map_err(|_| "ERR Protocol error: invalid bulk string length".to_string())?;
            let bulk_len: usize = bulk_len_str
                .parse()
                .map_err(|_| "ERR Protocol error: invalid bulk string length".to_string())?;

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

        let cmd_name = std::str::from_utf8(parts[0])
            .unwrap_or("")
            .to_ascii_uppercase();

        match cmd_name.as_str() {
            "PING" => {
                let arg = if parts.len() > 1 {
                    Some(Bytes::copy_from_slice(parts[1]))
                } else {
                    None
                };
                Ok(RespCommand::Ping(arg))
            }
            "ECHO" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'echo' command".to_string())
                } else {
                    Ok(RespCommand::Echo(Bytes::copy_from_slice(parts[1])))
                }
            }
            "GET" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'get' command".to_string())
                } else {
                    Ok(RespCommand::Get(Bytes::copy_from_slice(parts[1])))
                }
            }
            "SET" => {
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
                    let opt = std::str::from_utf8(parts[i])
                        .unwrap_or("")
                        .to_ascii_uppercase();
                    match opt.as_str() {
                        "EX" => {
                            if i + 1 >= parts.len() {
                                return Err("ERR syntax error".to_string());
                            }
                            let secs: u64 = std::str::from_utf8(parts[i + 1])
                                .unwrap_or("0")
                                .parse()
                                .map_err(|_| "ERR value is not an integer or out of range".to_string())?;
                            ttl_ms = Some(secs * 1000);
                            i += 2;
                        }
                        "PX" => {
                            if i + 1 >= parts.len() {
                                return Err("ERR syntax error".to_string());
                            }
                            let ms: u64 = std::str::from_utf8(parts[i + 1])
                                .unwrap_or("0")
                                .parse()
                                .map_err(|_| "ERR value is not an integer or out of range".to_string())?;
                            ttl_ms = Some(ms);
                            i += 2;
                        }
                        "NX" => {
                            nx = true;
                            i += 1;
                        }
                        "XX" => {
                            xx = true;
                            i += 1;
                        }
                        _ => {
                            i += 1;
                        }
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
            "DEL" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'del' command".to_string())
                } else {
                    let keys = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                    Ok(RespCommand::Del(keys))
                }
            }
            "EXISTS" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'exists' command".to_string())
                } else {
                    let keys = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                    Ok(RespCommand::Exists(keys))
                }
            }
            "MGET" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'mget' command".to_string())
                } else {
                    let keys = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                    Ok(RespCommand::MGet(keys))
                }
            }
            "MSET" => {
                if parts.len() < 3 || (parts.len() - 1) % 2 != 0 {
                    Err("ERR wrong number of arguments for 'mset' command".to_string())
                } else {
                    let mut pairs = Vec::with_capacity((parts.len() - 1) / 2);
                    for chunk in parts[1..].chunks(2) {
                        pairs.push((Bytes::copy_from_slice(chunk[0]), Bytes::copy_from_slice(chunk[1])));
                    }
                    Ok(RespCommand::MSet(pairs))
                }
            }
            "INCR" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'incr' command".to_string())
                } else {
                    Ok(RespCommand::Incr {
                        key: Bytes::copy_from_slice(parts[1]),
                        delta: 1,
                    })
                }
            }
            "DECR" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'decr' command".to_string())
                } else {
                    Ok(RespCommand::Incr {
                        key: Bytes::copy_from_slice(parts[1]),
                        delta: -1,
                    })
                }
            }
            "INCRBY" => {
                if parts.len() < 3 {
                    Err("ERR wrong number of arguments for 'incrby' command".to_string())
                } else {
                    let delta: i64 = std::str::from_utf8(parts[2])
                        .unwrap_or("0")
                        .parse()
                        .map_err(|_| "ERR value is not an integer or out of range".to_string())?;
                    Ok(RespCommand::Incr {
                        key: Bytes::copy_from_slice(parts[1]),
                        delta,
                    })
                }
            }
            "DECRBY" => {
                if parts.len() < 3 {
                    Err("ERR wrong number of arguments for 'decrby' command".to_string())
                } else {
                    let delta: i64 = std::str::from_utf8(parts[2])
                        .unwrap_or("0")
                        .parse()
                        .map_err(|_| "ERR value is not an integer or out of range".to_string())?;
                    Ok(RespCommand::Incr {
                        key: Bytes::copy_from_slice(parts[1]),
                        delta: -delta,
                    })
                }
            }
            "TTL" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'ttl' command".to_string())
                } else {
                    Ok(RespCommand::Ttl(Bytes::copy_from_slice(parts[1])))
                }
            }
            "PTTL" => {
                if parts.len() < 2 {
                    Err("ERR wrong number of arguments for 'pttl' command".to_string())
                } else {
                    Ok(RespCommand::Pttl(Bytes::copy_from_slice(parts[1])))
                }
            }
            "EXPIRE" => {
                if parts.len() < 3 {
                    Err("ERR wrong number of arguments for 'expire' command".to_string())
                } else {
                    let seconds: u64 = std::str::from_utf8(parts[2])
                        .unwrap_or("0")
                        .parse()
                        .map_err(|_| "ERR value is not an integer or out of range".to_string())?;
                    Ok(RespCommand::Expire {
                        key: Bytes::copy_from_slice(parts[1]),
                        seconds,
                    })
                }
            }
            "DBSIZE" => Ok(RespCommand::DbSize),
            "FLUSHDB" | "FLUSHALL" => Ok(RespCommand::FlushDb),
            "COMMAND" => Ok(RespCommand::Command),
            "INFO" => {
                let section = if parts.len() > 1 {
                    Some(Bytes::copy_from_slice(parts[1]))
                } else {
                    None
                };
                Ok(RespCommand::Info(section))
            }
            "QUIT" => Ok(RespCommand::Quit),
            "CLUSTER" => {
                let args = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                Ok(RespCommand::Cluster(args))
            }
            "TIER" => {
                let args = parts[1..].iter().map(|p| Bytes::copy_from_slice(p)).collect();
                Ok(RespCommand::Tier(args))
            }
            _ => Ok(RespCommand::Unknown(Bytes::copy_from_slice(parts[0]))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ping_inline() {
        let input = b"PING\r\n";
        let (cmd, consumed) = RespParser::parse(input).unwrap().unwrap();
        assert_eq!(cmd, RespCommand::Ping(None));
        assert_eq!(consumed, 6);
    }

    #[test]
    fn test_parse_set_array() {
        let input = b"*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n";
        let (cmd, consumed) = RespParser::parse(input).unwrap().unwrap();
        match cmd {
            RespCommand::Set { key, value, ttl_ms, .. } => {
                assert_eq!(key, Bytes::from("foo"));
                assert_eq!(value, Bytes::from("bar"));
                assert!(ttl_ms.is_none());
            }
            _ => panic!("Expected Set command"),
        }
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_parse_set_with_ex() {
        let input = b"*5\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n$2\r\nEX\r\n$2\r\n10\r\n";
        let (cmd, consumed) = RespParser::parse(input).unwrap().unwrap();
        match cmd {
            RespCommand::Set { key, value, ttl_ms, .. } => {
                assert_eq!(key, Bytes::from("foo"));
                assert_eq!(value, Bytes::from("bar"));
                assert_eq!(ttl_ms, Some(10000));
            }
            _ => panic!("Expected Set with EX"),
        }
        assert_eq!(consumed, input.len());
    }
}
