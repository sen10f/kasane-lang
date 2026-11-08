extern core.panic.rt_panic(pointer: *const u8, length: usize) -> ! link "ksn_rt_panic"

fn core.panic.panic(message: &str) -> ! uses [] panics may pub
fn core.panic.todo() -> ! uses [] panics may pub
fn core.panic.unreachable() -> ! uses [] panics may pub
