//! 앱이 접속할 서버 주소 검사. 앱 첫 화면에서 사람이 입력한 값을 받는다.
//!
//! 로그인 토큰이 오가므로 원격 서버는 `https`만 받는다. 개발할 때 쓰는 이 기기·에뮬레이터의
//! 호스트(`localhost`, `127.0.0.1`, `::1`, 안드로이드 에뮬레이터의 `10.0.2.2`)만 `http`를 허용한다.

/// 주소가 너무 길면 거른다(사람이 입력하는 값).
pub const MAX_LEN: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerUrlError {
    Empty,
    TooLong,
    /// `https`(개발 호스트는 `http`도) 말고 다른 스킴.
    UnsupportedScheme,
    /// 원격 서버에 `http`. 토큰이 평문으로 오간다.
    InsecureHttp,
    /// 호스트·포트가 비었거나 형식이 틀렸다.
    InvalidHost,
    /// 경로·쿼리·조각·사용자 정보가 붙었다. 서버 주소(출처)만 받는다.
    ExtraParts,
}

impl ServerUrlError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "서버 주소를 입력해 주세요.",
            Self::TooLong => "서버 주소가 너무 길어요.",
            Self::UnsupportedScheme => "https:// 로 시작하는 주소를 입력해 주세요.",
            Self::InsecureHttp => "보안을 위해 https 주소만 쓸 수 있어요.",
            Self::InvalidHost => "서버 주소 형식이 올바르지 않아요.",
            Self::ExtraParts => "경로 없이 서버 주소만 입력해 주세요. 예: https://gw.example.com",
        }
    }
}

/// 개발용으로 `http`를 허용하는 호스트.
const LOCAL_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "[::1]", "10.0.2.2"];

/// 입력을 `https://host[:port]` 꼴의 출처로 정리한다. 스킴을 빼면 `https`로 본다.
pub fn normalize(input: &str) -> Result<String, ServerUrlError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ServerUrlError::Empty);
    }
    if trimmed.len() > MAX_LEN {
        return Err(ServerUrlError::TooLong);
    }
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("https".to_owned(), trimmed),
    };
    if scheme != "https" && scheme != "http" {
        return Err(ServerUrlError::UnsupportedScheme);
    }
    // 끝의 `/` 하나는 사람들이 흔히 붙이므로 받아 준다.
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains(['/', '?', '#', '@', '\\']) {
        return Err(ServerUrlError::ExtraParts);
    }
    let (host, port) = split_port(authority)?;
    let host = host.to_ascii_lowercase();
    if !valid_host(&host) {
        return Err(ServerUrlError::InvalidHost);
    }
    if scheme == "http" && !LOCAL_HOSTS.contains(&host.as_str()) {
        return Err(ServerUrlError::InsecureHttp);
    }
    Ok(match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

/// `host[:port]`를 나눈다. IPv6는 `[..]`로 감싼 꼴만 받는다.
fn split_port(authority: &str) -> Result<(&str, Option<u16>), ServerUrlError> {
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or(ServerUrlError::InvalidHost)?;
        let (host, after) = authority.split_at(end + 1);
        match after {
            "" => (host, None),
            _ => (
                host,
                Some(after.strip_prefix(':').ok_or(ServerUrlError::InvalidHost)?),
            ),
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port {
        None => None,
        Some(port) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ServerUrlError::InvalidHost);
            }
            match port.parse::<u16>() {
                Ok(0) | Err(_) => return Err(ServerUrlError::InvalidHost),
                Ok(port) => Some(port),
            }
        }
    };
    Ok((host, port))
}

fn valid_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_hexdigit() || b == b':');
    }
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ServerUrlError::*;

    #[test]
    fn https_addresses_are_normalized_to_an_origin() {
        assert_eq!(
            normalize("https://gw.example.com").as_deref(),
            Ok("https://gw.example.com")
        );
        assert_eq!(
            normalize("  HTTPS://GW.Example.com/ ").as_deref(),
            Ok("https://gw.example.com")
        );
        assert_eq!(
            normalize("gw.example.com").as_deref(),
            Ok("https://gw.example.com")
        );
        assert_eq!(
            normalize("gw.example.com:8443").as_deref(),
            Ok("https://gw.example.com:8443")
        );
        assert_eq!(
            normalize("https://192.168.0.10").as_deref(),
            Ok("https://192.168.0.10")
        );
        assert_eq!(
            normalize("https://[fd00::1]:443").as_deref(),
            Ok("https://[fd00::1]:443")
        );
    }

    #[test]
    fn plain_http_is_only_for_this_device_or_the_emulator_host() {
        assert_eq!(
            normalize("http://localhost:8080").as_deref(),
            Ok("http://localhost:8080")
        );
        assert_eq!(
            normalize("http://127.0.0.1:8080").as_deref(),
            Ok("http://127.0.0.1:8080")
        );
        assert_eq!(
            normalize("http://10.0.2.2:8080").as_deref(),
            Ok("http://10.0.2.2:8080")
        );
        assert_eq!(
            normalize("http://[::1]:8080").as_deref(),
            Ok("http://[::1]:8080")
        );
        assert_eq!(normalize("http://gw.example.com"), Err(InsecureHttp));
        assert_eq!(normalize("http://192.168.0.10:8080"), Err(InsecureHttp));
        // 비슷하게 생긴 이름으로 우회할 수 없다.
        assert_eq!(normalize("http://localhost.evil.com"), Err(InsecureHttp));
        assert_eq!(normalize("http://127.0.0.1.nip.io"), Err(InsecureHttp));
    }

    #[test]
    fn other_schemes_are_refused() {
        for input in [
            "ftp://gw.example.com",
            "javascript://x",
            "file:///etc/passwd",
            "ws://localhost",
        ] {
            assert_eq!(normalize(input), Err(UnsupportedScheme), "{input}");
        }
    }

    #[test]
    fn only_the_origin_is_accepted() {
        for input in [
            "https://gw.example.com/api",
            "https://gw.example.com/?a=1",
            "https://gw.example.com#x",
            "https://user:pw@gw.example.com",
            "https://localhost@evil.com",
            "https://gw.example.com\\@evil.com",
            "https://gw.example.com//",
        ] {
            assert_eq!(normalize(input), Err(ExtraParts), "{input}");
        }
    }

    #[test]
    fn malformed_hosts_and_ports_are_refused() {
        for input in [
            "https://",
            "https://:8080",
            "https://gw..example.com",
            "https://-gw.example.com",
            "https://gw_example.com",
            "https://gw.example.com:",
            "https://gw.example.com:0",
            "https://gw.example.com:65536",
            "https://gw.example.com:+80",
            "https://gw.example.com:80:80",
            "https://[fd00::1",
            "https://[]",
            "https://[fd00::1]x",
            "https://한국.example.com",
            "https://gw example.com",
        ] {
            assert_eq!(normalize(input), Err(InvalidHost), "{input}");
        }
        assert_eq!(
            normalize("https://gw.example.com:65535").as_deref(),
            Ok("https://gw.example.com:65535")
        );
    }

    #[test]
    fn empty_and_overlong_input() {
        assert_eq!(normalize(""), Err(Empty));
        assert_eq!(normalize("   "), Err(Empty));
        let long = format!("https://{}.com", "a".repeat(MAX_LEN));
        assert_eq!(normalize(&long), Err(TooLong));
    }
}
