// 処理系が持つ唯一の unsafe の部品。`Vec` / `String` を安全な Kasane で書くための土台 (8.3)。
type alloc.raw.Buf[T, A: alloc.alloc.Allocator] pub
    allocator: A
    capacity: usize
    initialized: usize
end alloc.raw.Buf

impl[T: core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for alloc.raw.Buf<T, A>
impl[T: core.marker.Sync, A: alloc.alloc.Allocator + core.marker.Sync] core.marker.Sync for alloc.raw.Buf<T, A>

fn alloc.raw.Buf.append_str_within_cap[A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<u8, A>, text: &str) -> Result<(), ()> uses [] panics never pub
fn alloc.raw.Buf.as_mut_slice[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>) -> &mut [T] uses [] panics never pub
fn alloc.raw.Buf.as_slice[T, A: alloc.alloc.Allocator](self: &alloc.raw.Buf<T, A>) -> &[T] uses [] panics never pub
fn alloc.raw.Buf.as_str[A: alloc.alloc.Allocator](self: &alloc.raw.Buf<u8, A>) -> Result<&str, core.str.Utf8Error> uses [] panics never pub
fn alloc.raw.Buf.cap[T, A: alloc.alloc.Allocator](self: &alloc.raw.Buf<T, A>) -> usize uses [] panics never pub
fn alloc.raw.Buf.clear[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>) -> () uses [] panics never pub
impl[T, A: alloc.alloc.Allocator] core.ops.Drop for alloc.raw.Buf<T, A>
fn alloc.raw.Buf.drop[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>) -> () uses [] panics never impls core.ops.Drop
fn alloc.raw.Buf.get[T, A: alloc.alloc.Allocator](self: &alloc.raw.Buf<T, A>, index: usize) -> Option<&T> uses [] panics never pub
fn alloc.raw.Buf.get_mut[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>, index: usize) -> Option<&mut T> uses [] panics never pub
fn alloc.raw.Buf.grow[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>, new_cap: usize) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.raw.Buf.insert_within_cap[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>, index: usize, value: T) -> Result<(), T> uses [] panics never pub
fn alloc.raw.Buf.is_empty[T, A: alloc.alloc.Allocator](self: &alloc.raw.Buf<T, A>) -> bool uses [] panics never pub
fn alloc.raw.Buf.len[T, A: alloc.alloc.Allocator](self: &alloc.raw.Buf<T, A>) -> usize uses [] panics never pub
fn alloc.raw.Buf.pop[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>) -> Option<T> uses [] panics never pub
fn alloc.raw.Buf.push_within_cap[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>, value: T) -> Result<(), T> uses [] panics never pub
fn alloc.raw.Buf.remove[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>, index: usize) -> Option<T> uses [] panics never pub
fn alloc.raw.Buf.swap_remove[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>, index: usize) -> Option<T> uses [] panics never pub
fn alloc.raw.Buf.truncate[T, A: alloc.alloc.Allocator](self: &mut alloc.raw.Buf<T, A>, length: usize) -> () uses [] panics never pub
fn alloc.raw.Buf.with_capacity[T, A: alloc.alloc.Allocator](allocator: A, capacity: usize) -> Result<alloc.raw.Buf<T, A>, alloc.alloc.AllocError> uses [alloc] panics never pub
