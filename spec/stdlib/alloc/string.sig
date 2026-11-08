type alloc.string.String[A: alloc.alloc.Allocator] pub
    bytes: alloc.vec.Vec<u8, A>
end alloc.string.String

fn alloc.string.String.as_str[A: alloc.alloc.Allocator](self: &alloc.string.String<A>) -> &str uses [] panics never pub
fn alloc.string.String.clear[A: alloc.alloc.Allocator](self: &mut alloc.string.String<A>) -> () uses [] panics never pub
fn alloc.string.String.from_str[A: alloc.alloc.Allocator](allocator: A, text: &str) -> Result<alloc.string.String<A>, alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.string.String.is_empty[A: alloc.alloc.Allocator](self: &alloc.string.String<A>) -> bool uses [] panics never pub
fn alloc.string.String.len[A: alloc.alloc.Allocator](self: &alloc.string.String<A>) -> usize uses [] panics never pub
fn alloc.string.String.new[A: alloc.alloc.Allocator](allocator: A) -> alloc.string.String<A> uses [] panics never pub
fn alloc.string.String.push[A: alloc.alloc.Allocator](self: &mut alloc.string.String<A>, c: char) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.string.String.push_str[A: alloc.alloc.Allocator](self: &mut alloc.string.String<A>, text: &str) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
