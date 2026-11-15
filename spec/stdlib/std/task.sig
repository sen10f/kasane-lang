type std.task.Task[T] pub
    id: usize pub
end std.task.Task

fn std.task.Task.cancel[T](self: std.task.Task<T>) -> () uses [] panics never pub
fn std.task.Task.join_task[T](self: std.task.Task<T>) -> T uses [suspend] panics may pub
fn std.task.block_on[F: core.ops.FnOnce<(), T, E, P>, T, E: uses, P: panics](f: F) -> T uses [E] panics may pub
fn std.task.spawn_local[F: core.ops.FnOnce<(), T, E, P> + 'static, T, E: uses, P: panics](f: F) -> std.task.Task<T> uses [E] panics may pub
fn std.task.yield_now() -> () uses [suspend] panics never pub
