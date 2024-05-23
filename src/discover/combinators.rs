pub fn tag<'a>(buf: &'a [u8], tag: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    if buf.len() >= tag.len() && &buf[..tag.len()] == tag {
        Some((&buf[tag.len()..], &buf[..tag.len()]))
    } else {
        None
    }
}

pub fn u8(buf: &[u8]) -> Option<(&[u8], u8)> {
    buf.first().map(|n| (&buf[1..], *n))
}

pub fn u64_le(buf: &[u8]) -> Option<(&[u8], u64)> {
    let size = std::mem::size_of::<u64>();
    if buf.len() >= size {
        let ret = u64::from_le_bytes(buf[0..size].try_into().unwrap());
        Some((&buf[size..], ret))
    } else {
        None
    }
}

pub fn take(buf: &[u8], n: usize) -> Option<(&[u8], &[u8])> {
    if buf.len() >= n {
        Some((&buf[n..], &buf[..n]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_combinators_work() {
        let test = b"abba";
        assert_eq!(u8(test), (Some((&b"bba"[..], b'a'))));
        assert_eq!(tag(test, b"ab"), Some((&b"ba"[..], &b"ab"[..])));
        assert_eq!(take(test, 3), Some((&b"a"[..], &b"abb"[..])));
    }
}
