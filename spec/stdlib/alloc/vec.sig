enum alloc.vec.InsertError pub
    OutOfBounds
    Alloc(alloc.alloc.AllocError)
end alloc.vec.InsertError

type alloc.vec.Vec[T, A: alloc.alloc.Allocator] pub
    allocator: A
    buffer: *mut T
    cap: usize
    used: usize
end alloc.vec.Vec

impl[T: core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for alloc.vec.Vec<T, A>
impl[T: core.marker.Sync, A: alloc.alloc.Allocator + core.marker.Sync] core.marker.Sync for alloc.vec.Vec<T, A>

fn alloc.vec.Vec.as_mut_slice[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>) -> &mut [T] uses [] panics never pub
fn alloc.vec.Vec.as_slice[T, A: alloc.alloc.Allocator](self: &alloc.vec.Vec<T, A>) -> &[T] uses [] panics never pub
fn alloc.vec.Vec.capacity[T, A: alloc.alloc.Allocator](self: &alloc.vec.Vec<T, A>) -> usize uses [] panics never pub
fn alloc.vec.Vec.clear[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>) -> () uses [] panics never pub
impl[T, A: alloc.alloc.Allocator] core.ops.Drop for alloc.vec.Vec<T, A>
fn alloc.vec.Vec.drop[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>) -> () uses [] panics never impls core.ops.Drop
fn alloc.vec.Vec.insert[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>, index: usize, value: T) -> Result<(), alloc.vec.InsertError> uses [alloc] panics never pub
fn alloc.vec.Vec.is_empty[T, A: alloc.alloc.Allocator](self: &alloc.vec.Vec<T, A>) -> bool uses [] panics never pub
fn alloc.vec.Vec.len[T, A: alloc.alloc.Allocator](self: &alloc.vec.Vec<T, A>) -> usize uses [] panics never pub
fn alloc.vec.Vec.new[T, A: alloc.alloc.Allocator](allocator: A) -> alloc.vec.Vec<T, A> uses [] panics never pub
fn alloc.vec.Vec.pop[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>) -> Option<T> uses [] panics never pub
fn alloc.vec.Vec.push[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>, value: T) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.vec.Vec.remove[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>, index: usize) -> Option<T> uses [] panics never pub
fn alloc.vec.Vec.reserve[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>, additional: usize) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.vec.Vec.truncate[T, A: alloc.alloc.Allocator](self: &mut alloc.vec.Vec<T, A>, length: usize) -> () uses [] panics never pub
fn alloc.vec.Vec.try_clone[T: core.ops.Clone, A: alloc.alloc.Allocator + core.ops.Clone](self: &alloc.vec.Vec<T, A>) -> Result<alloc.vec.Vec<T, A>, alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.vec.Vec.with_capacity[T, A: alloc.alloc.Allocator](allocator: A, capacity: usize) -> Result<alloc.vec.Vec<T, A>, alloc.alloc.AllocError> uses [alloc] panics never pub
