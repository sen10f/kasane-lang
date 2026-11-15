type std.rand.Rng pub
    state: u64
end std.rand.Rng

fn std.rand.Rng.from_entropy() -> std.rand.Rng uses [rand] panics never pub
fn std.rand.Rng.from_seed(seed: u64) -> std.rand.Rng uses [] panics never pub
fn std.rand.Rng.next_u64(self: &mut std.rand.Rng) -> u64 uses [] panics never pub
fn std.rand.fill_secure(buf: &mut [u8]) -> Result<(), std.io.Error> uses [rand] panics never pub
