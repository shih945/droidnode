/// A1: percent-decode one query component (`kubectl` sends `%20` for spaces).
pub(crate) fn decode_query_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| -> Option<u8> {
                    match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    }
                };
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A1: parse the RemoteCommand query (`command` may repeat, one arg each).
pub(crate) fn parse_exec_query(raw: Option<String>) -> (Vec<String>, bool, bool, bool, bool) {
    let mut command = Vec::new();
    let mut stdin = false;
    let mut stdout = false;
    let mut stderr = false;
    let mut tty = false;
    for pair in raw.unwrap_or_default().split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = decode_query_component(value);
        match key {
            "command" => command.push(value),
            "stdin" => stdin = value == "1" || value == "true",
            "stdout" => stdout = value == "1" || value == "true",
            "stderr" => stderr = value == "1" || value == "true",
            "tty" => tty = value == "1" || value == "true",
            _ => {}
        }
    }
    (command, stdin, stdout, stderr, tty)
}
