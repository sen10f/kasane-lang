type core.str.Chars['a] pub
    rest: &'a str
end core.str.Chars

type core.str.Utf8Error derive [Debug, Clone, Copy, PartialEq, Eq] pub
    valid_up_to: usize
end core.str.Utf8Error

impl['a] core.iter.Iterator<char, [], never> for core.str.Chars<'a>
fn core.str.Chars.next(self: &mut core.str.Chars) -> Option<char> uses [] panics never impls core.iter.Iterator<char, [], never>
fn core.str.as_bytes(self: &str) -> &[u8] uses [] panics never pub
fn core.str.chars['a](self: &'a str) -> core.str.Chars<'a> uses [] panics never pub
fn core.str.find(self: &str, pattern: &str) -> Option<usize> uses [] panics never pub
fn core.str.from_utf8(bytes: &[u8]) -> Result<&str, core.str.Utf8Error> uses [] panics never pub
fn core.str.is_empty(self: &str) -> bool uses [] panics never pub
fn core.str.len(self: &str) -> usize uses [] panics never pub
fn core.str.split_at(self: &str, mid: usize) -> Option<(&str, &str)> uses [] panics never pub
fn core.str.starts_with(self: &str, prefix: &str) -> bool uses [] panics never pub
