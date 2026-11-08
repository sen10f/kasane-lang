type std.time.Duration derive [Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash] pub
    nanos: u64
end std.time.Duration

type std.time.Instant derive [Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord] pub
    ticks: u64
end std.time.Instant

fn std.time.Duration.as_nanos(self: &std.time.Duration) -> u64 uses [] panics never pub
fn std.time.Duration.from_millis(milliseconds: u64) -> std.time.Duration const uses [] panics never pub
fn std.time.Instant.elapsed(self: &std.time.Instant) -> std.time.Duration uses [time] panics never pub
fn std.time.Instant.now() -> std.time.Instant uses [time] panics never pub
fn std.time.Instant.since(self: &std.time.Instant, earlier: &std.time.Instant) -> Option<std.time.Duration> uses [] panics never pub
