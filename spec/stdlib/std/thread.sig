type std.thread.JoinHandle[T: linear, A: alloc.alloc.Allocator] linear pub
    allocator: A
    native: usize
    result: *mut T
end std.thread.JoinHandle

type std.thread.SpawnError derive [Debug, Clone, Copy, PartialEq, Eq] pub
    code: i32 pub
end std.thread.SpawnError

impl[T: linear + core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for std.thread.JoinHandle<T, A>

fn std.thread.JoinHandle.join[T: linear, A: alloc.alloc.Allocator](self: std.thread.JoinHandle<T, A>) -> T uses [thread] panics never pub
fn std.thread.sleep(milliseconds: u64) -> () uses [thread, time] panics never pub
fn std.thread.spawn[A: alloc.alloc.Allocator + core.marker.Send + 'static, F: linear + core.marker.Send + core.ops.FnOnce<(), T, E, P> + 'static, T: linear + core.marker.Send + 'static, E: uses, P: panics](allocator: A, f: F) -> Result<std.thread.JoinHandle<T, A>, std.thread.SpawnError> uses [alloc, thread, E] panics P pub
fn std.thread.spawn_deep[A: alloc.alloc.Allocator + core.marker.Send + 'static, F: linear + core.marker.Send + core.ops.FnOnce<(), T, E, P> + 'static, T: linear + core.marker.Send + 'static, E: uses, P: panics](allocator: A, f: F, depth: usize) -> Result<std.thread.JoinHandle<T, A>, std.thread.SpawnError> uses [alloc, thread, E] panics P pub
fn std.thread.yield_now() -> () uses [thread] panics never pub
