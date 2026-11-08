type core.sync.AtomicBool pub
    value: u8
end core.sync.AtomicBool

type core.sync.AtomicU64 pub
    value: u64
end core.sync.AtomicU64

type core.sync.AtomicUsize pub
    value: usize
end core.sync.AtomicUsize

enum core.sync.Ordering derive [Debug, Clone, Copy, PartialEq, Eq] pub
    Relaxed
    Acquire
    Release
    AcqRel
    SeqCst
end core.sync.Ordering

fn core.sync.AtomicBool.compare_exchange(self: &core.sync.AtomicBool, expected: bool, desired: bool, order: core.sync.Ordering) -> Result<bool, bool> uses [] panics never pub
fn core.sync.AtomicBool.load(self: &core.sync.AtomicBool, order: core.sync.Ordering) -> bool uses [] panics never pub
fn core.sync.AtomicBool.new(value: bool) -> core.sync.AtomicBool const uses [] panics never pub
fn core.sync.AtomicBool.store(self: &core.sync.AtomicBool, value: bool, order: core.sync.Ordering) -> () uses [] panics never pub
fn core.sync.AtomicBool.swap(self: &core.sync.AtomicBool, value: bool, order: core.sync.Ordering) -> bool uses [] panics never pub
fn core.sync.AtomicU64.compare_exchange(self: &core.sync.AtomicU64, expected: u64, desired: u64, order: core.sync.Ordering) -> Result<u64, u64> uses [] panics never pub
fn core.sync.AtomicU64.fetch_add(self: &core.sync.AtomicU64, value: u64, order: core.sync.Ordering) -> u64 uses [] panics never pub
fn core.sync.AtomicU64.load(self: &core.sync.AtomicU64, order: core.sync.Ordering) -> u64 uses [] panics never pub
fn core.sync.AtomicU64.new(value: u64) -> core.sync.AtomicU64 const uses [] panics never pub
fn core.sync.AtomicU64.store(self: &core.sync.AtomicU64, value: u64, order: core.sync.Ordering) -> () uses [] panics never pub
fn core.sync.AtomicU64.swap(self: &core.sync.AtomicU64, value: u64, order: core.sync.Ordering) -> u64 uses [] panics never pub
fn core.sync.AtomicUsize.compare_exchange(self: &core.sync.AtomicUsize, expected: usize, desired: usize, order: core.sync.Ordering) -> Result<usize, usize> uses [] panics never pub
fn core.sync.AtomicUsize.fetch_add(self: &core.sync.AtomicUsize, value: usize, order: core.sync.Ordering) -> usize uses [] panics never pub
fn core.sync.AtomicUsize.load(self: &core.sync.AtomicUsize, order: core.sync.Ordering) -> usize uses [] panics never pub
fn core.sync.AtomicUsize.new(value: usize) -> core.sync.AtomicUsize const uses [] panics never pub
fn core.sync.AtomicUsize.store(self: &core.sync.AtomicUsize, value: usize, order: core.sync.Ordering) -> () uses [] panics never pub
fn core.sync.AtomicUsize.swap(self: &core.sync.AtomicUsize, value: usize, order: core.sync.Ordering) -> usize uses [] panics never pub
