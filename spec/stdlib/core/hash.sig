type core.hash.Fnv64 derive [Debug, Clone] pub
    state: u64
end core.hash.Fnv64

impl core.hash.Hasher for core.hash.Fnv64
fn core.hash.Fnv64.finish(self: &core.hash.Fnv64) -> u64 uses [] panics never impls core.hash.Hasher
fn core.hash.Fnv64.new() -> core.hash.Fnv64 const uses [] panics never pub
fn core.hash.Fnv64.write_bytes(self: &mut core.hash.Fnv64, bytes: &[u8]) -> () uses [] panics never impls core.hash.Hasher
