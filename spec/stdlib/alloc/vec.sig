enum alloc.vec.InsertError pub
    OutOfBounds
    Alloc(alloc.alloc.AllocError)
end alloc.vec.InsertError

type alloc.vec.Vec[T, A: alloc.alloc.Allocator] pub
    buf: alloc.raw.Buf<T, A>
end alloc.vec.Vec

impl[T: core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for alloc.vec.Vec<T, A>
impl[T: core.marker.Sync, A: alloc.alloc.Allocator + core.marker.Sync] core.marker.Sync for alloc.vec.Vec<T, A>

impl[T: core.fmt.Debug, A: alloc.alloc.Allocator] core.fmt.Debug for alloc.vec.Vec<T, A>
fn alloc.vec.Vec.debug_fmt[T: core.fmt.Debug, A: alloc.alloc.Allocator](self: &alloc.vec.Vec<T, A>, f: &mut core.fmt.Formatter) -> Result<(), core.fmt.Error> uses [] panics never impls core.fmt.Debug
impl[T, A: alloc.alloc.Allocator] core.ops.Drop for alloc.vec.Vec<T, A>
fn alloc.vec.Vec.drop[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>) -> () uses [] panics never impls core.ops.Drop
fn alloc.vec.Vec.new[T, A: alloc.alloc.Allocator](allocator: A) -> alloc.vec.Vec<T, A> uses [] panics never pub
impl[T: alloc.alloc.TryClone, A: alloc.alloc.Allocator + core.ops.Clone] alloc.alloc.TryClone for alloc.vec.Vec<T, A>
fn alloc.vec.Vec.try_clone[T: alloc.alloc.TryClone, A: alloc.alloc.Allocator + core.ops.Clone](self: &alloc.vec.Vec<T, A>) -> Result<alloc.vec.Vec<T, A>, alloc.alloc.AllocError> uses [alloc] panics never impls alloc.alloc.TryClone pub
fn alloc.vec.Vec.with_capacity[T, A: alloc.alloc.Allocator](allocator: A, capacity: usize) -> Result<alloc.vec.Vec<T, A>, alloc.alloc.AllocError> uses [alloc] panics never pub
