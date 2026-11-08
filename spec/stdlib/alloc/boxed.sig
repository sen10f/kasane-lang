type alloc.boxed.Box[T, A: alloc.alloc.Allocator] pub
    allocator: A
    pointer: *mut T
end alloc.boxed.Box

impl[T: core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for alloc.boxed.Box<T, A>
impl[T: core.marker.Sync, A: alloc.alloc.Allocator + core.marker.Sync] core.marker.Sync for alloc.boxed.Box<T, A>

fn alloc.boxed.Box.as_mut[T, A: alloc.alloc.Allocator](self: &mut alloc.boxed.Box<T, A>) -> &mut T uses [] panics never pub
fn alloc.boxed.Box.as_ref[T, A: alloc.alloc.Allocator](self: &alloc.boxed.Box<T, A>) -> &T uses [] panics never pub
impl[T, A: alloc.alloc.Allocator] core.ops.Drop for alloc.boxed.Box<T, A>
fn alloc.boxed.Box.drop[T, A: alloc.alloc.Allocator](self: &mut alloc.boxed.Box<T, A>) -> () uses [] panics never impls core.ops.Drop
fn alloc.boxed.Box.into_inner[T, A: alloc.alloc.Allocator](self: alloc.boxed.Box<T, A>) -> T uses [] panics never pub
fn alloc.boxed.Box.new[T, A: alloc.alloc.Allocator](allocator: A, value: T) -> Result<alloc.boxed.Box<T, A>, alloc.alloc.AllocError> uses [alloc] panics never pub
