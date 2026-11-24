type std.task.Task[T] pub
    id: usize
end std.task.Task

fn std.task.Task.cancel[T](self: std.task.Task<T>) -> () uses [] panics never pub
fn std.task.Task.join_task[T](self: std.task.Task<T>) -> T uses [suspend] panics may pub
fn std.task.await_js(handle: u32) -> i32 uses [suspend] panics never pub
fn std.task.block_on[F: linear + core.ops.FnOnce<(), T, E, P>, T, E: uses, P: panics](f: F) -> T uses [E] panics may pub
fn std.task.block_on_threads[A: alloc.alloc.Allocator + core.marker.Send + 'static, F: linear + core.marker.Send + core.ops.FnOnce<(), T, E, P> + 'static, T: linear + core.marker.Send + 'static, E: uses, P: panics](allocator: A, n: usize, f: F) -> T uses [alloc, thread, E] panics may pub
fn std.task.spawn[F: linear + core.marker.Send + core.ops.FnOnce<(), T, E, P> + 'static, T: linear + core.marker.Send + 'static, E: uses, P: panics](f: F) -> std.task.Task<T> uses [E] panics may pub
fn std.task.spawn_local[F: linear + core.ops.FnOnce<(), T, E, P> + 'static, T, E: uses, P: panics](f: F) -> std.task.Task<T> uses [E] panics may pub
fn std.task.wait_io(fd: i32, write: i32) -> i32 uses [suspend] panics never
fn std.task.yield_now() -> () uses [suspend] panics never pub
