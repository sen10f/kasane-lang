type std.sync.Mutex[T] pub
    flag: core.sync.AtomicBool
    value: T
end std.sync.Mutex

type std.sync.MutexGuard['a, T] pub
    mutex: &'a std.sync.Mutex<T>
end std.sync.MutexGuard

type std.sync.Receiver[T: linear] pub
    channel: *mut u8
end std.sync.Receiver

type std.sync.RecvError derive [Debug, Clone, Copy, PartialEq, Eq] pub
    code: u8
end std.sync.RecvError

type std.sync.SendError[T: linear] pub
    value: T pub
end std.sync.SendError

type std.sync.Sender[T: linear] pub
    channel: *mut u8
end std.sync.Sender

impl['a, T: core.marker.Sync] core.marker.Sync for std.sync.MutexGuard<'a, T>
impl[T: core.marker.Send] core.marker.Send for std.sync.Mutex<T>
impl[T: core.marker.Send] core.marker.Sync for std.sync.Mutex<T>
impl[T: linear + core.marker.Send] core.marker.Send for std.sync.Receiver<T>
impl[T: linear + core.marker.Send] core.marker.Send for std.sync.Sender<T>
impl[T: linear + core.marker.Send] core.marker.Sync for std.sync.Sender<T>

fn std.sync.Mutex.lock['a, T](self: &'a std.sync.Mutex<T>) -> std.sync.MutexGuard<'a, T> uses [thread] panics never pub
fn std.sync.Mutex.new[T](value: T) -> std.sync.Mutex<T> const uses [] panics never pub
impl['a, T] core.ops.Drop for std.sync.MutexGuard<'a, T>
fn std.sync.MutexGuard.drop[T](self: &mut std.sync.MutexGuard<T>) -> () uses [] panics never impls core.ops.Drop
fn std.sync.MutexGuard.get[T](self: &std.sync.MutexGuard<T>) -> &T uses [] panics never pub
fn std.sync.MutexGuard.get_mut[T](self: &mut std.sync.MutexGuard<T>) -> &mut T uses [] panics never pub
fn std.sync.Receiver.clone[T: linear](self: &std.sync.Receiver<T>) -> std.sync.Receiver<T> uses [] panics never pub
fn std.sync.Receiver.close[T: linear](self: std.sync.Receiver<T>) -> Result<(), std.sync.Receiver<T>> uses [] panics never pub
impl[T] core.ops.Drop for std.sync.Receiver<T>
fn std.sync.Receiver.drop[T](self: &mut std.sync.Receiver<T>) -> () uses [] panics never impls core.ops.Drop
fn std.sync.Receiver.recv[T: linear](self: &std.sync.Receiver<T>) -> Result<T, std.sync.RecvError> uses [thread] panics never pub
fn std.sync.Sender.clone[T: linear](self: &std.sync.Sender<T>) -> std.sync.Sender<T> uses [] panics never pub
impl[T: linear] core.ops.Drop for std.sync.Sender<T>
fn std.sync.Sender.drop[T: linear](self: &mut std.sync.Sender<T>) -> () uses [] panics never impls core.ops.Drop
fn std.sync.Sender.send[T: linear](self: &std.sync.Sender<T>, value: T) -> Result<(), std.sync.SendError<T>> uses [thread] panics never pub
fn std.sync.channel[T: linear, A: alloc.alloc.Allocator + core.marker.Send + 'static](allocator: A, capacity: usize) -> Result<(std.sync.Sender<T>, std.sync.Receiver<T>), alloc.alloc.AllocError> uses [alloc] panics never pub
